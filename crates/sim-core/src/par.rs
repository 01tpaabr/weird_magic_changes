//! Deterministic parallel helpers on Bevy's compute task pool.
//!
//! Bevy gives two parallel primitives: `Query::par_iter_mut` (each chunk
//! entity handled by exactly one task, no reduction) and
//! `ComputeTaskPool::scope` (arbitrary tasks whose results come back **in
//! spawn order**). Both are deterministic as long as every task writes only
//! what it owns and the merge walks results in a fixed order; that is what
//! these wrappers make the default.
//!
//! The pool is a process-wide singleton. [`init_task_pool`] sizes it from
//! `WMC_THREADS` (default: all cores; anything but a count of 1 or more
//! panics, and `wmc` refuses it before that); the first initialiser in a
//! process wins (`wmc --threads N` over `WMC_THREADS`), so `WMC_THREADS=1
//! wmc run ...` vs the default is the thread-count gate for the whole binary.

use bevy_tasks::{ComputeTaskPool, TaskPoolBuilder};

/// Thread count requested through the environment, if any. Panics on a
/// value that is not one: a mistyped `WMC_THREADS=1` would otherwise run
/// on all cores and the thread-count gate would compare a run with itself.
pub fn threads_from_env() -> Option<usize> {
    parse_threads(std::env::var("WMC_THREADS").ok().as_deref()).unwrap_or_else(|e| panic!("{e}"))
}

/// `WMC_THREADS`'s value: unset or empty is `None` (all cores), else a
/// count of 1 or more. `wmc` checks it with this before it makes a world,
/// so a bad one is an error there, not a panic.
pub fn parse_threads(v: Option<&str>) -> Result<Option<usize>, String> {
    match v {
        None | Some("") => Ok(None),
        Some(v) => match v.trim().parse() {
            Ok(n) if n >= 1 => Ok(Some(n)),
            _ => Err(format!(
                "WMC_THREADS=`{v}` is not a thread count (1 or more)"
            )),
        },
    }
}

/// Make sure the compute pool exists, sized from `WMC_THREADS` when set.
/// Idempotent; returns the pool's thread count. Every `par_iter` and
/// [`par_map`] needs this to have run once (an `App` with `TaskPoolPlugin`
/// does it for you). A pool that exists already (`wmc --threads N`) is
/// kept, and `WMC_THREADS` not read.
pub fn init_task_pool() -> usize {
    match ComputeTaskPool::try_get() {
        Some(pool) => pool.thread_num(),
        None => init_task_pool_with(threads_from_env()),
    }
}

/// [`init_task_pool`] with this many threads (`None`: all cores); the
/// first initialiser in a process wins (`wmc ... --threads N`).
pub fn init_task_pool_with(threads: Option<usize>) -> usize {
    ComputeTaskPool::get_or_init(|| {
        let mut b = TaskPoolBuilder::new().thread_name("compute".into());
        if let Some(n) = threads {
            b = b.num_threads(n);
        }
        b.build()
    })
    .thread_num()
}

/// Threads in the compute pool (0 if not initialised).
pub fn thread_count() -> usize {
    ComputeTaskPool::try_get().map_or(0, |p| p.thread_num())
}

/// `items.iter().map(f).collect()`, computed in parallel in batches of
/// `batch`, results in input order. `f` must be a pure function of its item
/// (it may read shared state); the output never depends on the thread count.
pub fn par_map<T, R>(items: &[T], batch: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send + 'static,
{
    let batch = batch.max(1);
    if items.len() <= batch {
        return items.iter().map(f).collect();
    }
    let f = &f;
    let per_batch: Vec<Vec<R>> = ComputeTaskPool::get().scope(|s| {
        for chunk in items.chunks(batch) {
            s.spawn(async move { chunk.iter().map(f).collect::<Vec<R>>() });
        }
    });
    per_batch.into_iter().flatten().collect()
}

/// `for (x, y) in items.zip(out) { f(x, y) }` in parallel, `batch` pairs per
/// task. Every task owns a disjoint slice of `out`, so the writes need no
/// coordination and no copy afterwards; the result is identical for any
/// thread count or batch size.
pub fn par_zip_mut<T, U>(items: &[T], out: &mut [U], batch: usize, f: impl Fn(&T, &mut U) + Sync)
where
    T: Sync,
    U: Send,
{
    assert_eq!(items.len(), out.len(), "par_zip_mut: length mismatch");
    let batch = batch.max(1);
    if items.len() <= batch {
        for (x, y) in items.iter().zip(out.iter_mut()) {
            f(x, y);
        }
        return;
    }
    let f = &f;
    ComputeTaskPool::get().scope(|s| {
        for (xs, ys) in items.chunks(batch).zip(out.chunks_mut(batch)) {
            s.spawn(async move {
                for (x, y) in xs.iter().zip(ys.iter_mut()) {
                    f(x, y);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_thread_count_is_refused() {
        assert!(parse_threads(Some("l")).is_err());
        assert!(parse_threads(Some("0")).is_err());
        assert!(parse_threads(Some("-1")).is_err());
        assert_eq!(parse_threads(Some(" 1")), Ok(Some(1)));
        assert_eq!(parse_threads(Some("8")), Ok(Some(8)));
        assert_eq!(parse_threads(Some("")), Ok(None));
        assert_eq!(parse_threads(None), Ok(None));
    }

    #[test]
    fn par_map_preserves_order_for_every_batch_size() {
        init_task_pool();
        let items: Vec<u64> = (0..1000).collect();
        let want: Vec<u64> = items.iter().map(|x| x * x).collect();
        for batch in [1, 3, 64, 999, 1000, 5000] {
            assert_eq!(par_map(&items, batch, |x| x * x), want, "batch {batch}");
        }
        assert!(par_map(&[] as &[u64], 4, |x| *x).is_empty());
    }

    #[test]
    fn par_zip_mut_writes_every_slot_in_place() {
        init_task_pool();
        let items: Vec<u32> = (0..1000).collect();
        for batch in [1, 7, 256, 1000, 4000] {
            let mut out = vec![0u64; 1000];
            par_zip_mut(&items, &mut out, batch, |&x, y| *y = u64::from(x) * 3);
            assert!(
                out.iter().enumerate().all(|(i, &y)| y == i as u64 * 3),
                "batch {batch}"
            );
        }
        par_zip_mut(&[] as &[u32], &mut [] as &mut [u64], 1, |_, _| {});
    }
}
