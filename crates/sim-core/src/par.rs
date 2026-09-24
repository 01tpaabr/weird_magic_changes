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
//! `WMC_THREADS` (default: all cores); the first initialiser in a process
//! wins, so `WMC_THREADS=1 wmc run ...` vs the default is the thread-count
//! gate for the whole binary.

use bevy_tasks::{ComputeTaskPool, TaskPoolBuilder};

/// Thread count requested through the environment, if any.
pub fn threads_from_env() -> Option<usize> {
    std::env::var("WMC_THREADS")
        .ok()?
        .parse()
        .ok()
        .filter(|&n| n >= 1)
}

/// Make sure the compute pool exists, sized from `WMC_THREADS` when set.
/// Idempotent; returns the pool's thread count. Every `par_iter` and
/// [`par_map`] needs this to have run once (an `App` with `TaskPoolPlugin`
/// does it for you).
pub fn init_task_pool() -> usize {
    ComputeTaskPool::get_or_init(|| {
        let mut b = TaskPoolBuilder::new().thread_name("compute".into());
        if let Some(n) = threads_from_env() {
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

/// Run `f` on `n` disjoint work indices `0..n` in parallel, `batch` indices
/// per task. For phases that write through disjoint `&mut` slices the caller
/// split beforehand (a frame's rows, a buffer's bands): `f(i)` must touch only
/// what index `i` owns.
pub fn par_for(n: usize, batch: usize, f: impl Fn(usize) + Sync) {
    let batch = batch.max(1);
    if n <= batch {
        (0..n).for_each(f);
        return;
    }
    let f = &f;
    ComputeTaskPool::get().scope(|s| {
        let mut start = 0;
        while start < n {
            let end = (start + batch).min(n);
            s.spawn(async move {
                for i in start..end {
                    f(i);
                }
            });
            start = end;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn par_for_touches_every_index_once() {
        init_task_pool();
        let n = 777;
        let mut hits = vec![0u8; n];
        // Hand each index its own cell through a raw split: the pattern a
        // caller uses is disjoint slices, here simulated with atomics-free
        // per-index ownership via `chunks_mut(1)` collected up front.
        let cells: Vec<std::sync::atomic::AtomicU8> = (0..n)
            .map(|_| std::sync::atomic::AtomicU8::new(0))
            .collect();
        par_for(n, 10, |i| {
            cells[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        for (h, c) in hits.iter_mut().zip(&cells) {
            *h = c.load(std::sync::atomic::Ordering::Relaxed);
        }
        assert!(hits.iter().all(|&h| h == 1));
    }
}
