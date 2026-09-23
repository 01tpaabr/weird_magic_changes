# Bevy 0.19.1 fact sheet (verified against crate sources + bevy.org migration guides)

Verification method: downloaded `bevy`, `bevy_internal`, `bevy_ecs`, `bevy_ecs_macros`, `bevy_app`, `bevy_tasks`, `bevy_time`, `bevy_utils`, `bevy_platform` 0.19.1 tarballs from static.crates.io and grepped the real source; migration guides fetched from https://bevy.org/learn/migration-guides/{0-16-to-0-17,0-17-to-0-18,0-18-to-0-19}/. Items marked **UNVERIFIED** come only from guide summaries, not source. Note: the guide index also lists a `0-19-to-0-20` page, so a 0.20 may be out or imminent; everything below is pinned to 0.19.1.

Facade paths: `bevy::ecs` = bevy_ecs, `bevy::app` = bevy_app, `bevy::tasks` = bevy_tasks, `bevy::time` = bevy_time, `bevy::utils` = bevy_utils, `bevy::platform` = bevy_platform.

---

## 1. Crates / features / MSRV

- `bevy` 0.19.1: `rust-version = "1.95.0"`, `edition = "2024"`.
- **`bevy` default features changed in 0.19**: `default = ["2d", "3d", "ui", "audio"]` (meta-features). `ui` is no longer implied by `2d`/`3d`; `audio` no longer implied by others.
  - `2d = ["default_app","default_platform","2d_bevy_render","scene","picking"]`
  - `default_app = ["async_executor","bevy_asset","bevy_log","bevy_state","reflect_auto_register"]`
  - `default_platform = ["std","bevy_gilrs","bevy_winit","bevy_clipboard","default_font","multi_threaded","webgl2","x11","wayland","custom_cursor","sysinfo_plugin"]`
  - `ui = ["default_app","default_platform","ui_api","ui_bevy_render","scene","picking","bevy_ui_widgets"]`; `ui_api = ["default_app","common_api","bevy_input_focus","bevy_ui"]`
  - `scene = ["bevy_world_serialization","bevy_scene"]`, `picking = ["bevy_picking","mesh_picking","sprite_picking","ui_picking"]`
- **`bevy` with `default-features = false`** pulls in `bevy_internal` whose always-on deps are: `bevy_app, bevy_derive, bevy_diagnostic, bevy_ecs (features=["bevy_reflect"], default-features=false), bevy_input, bevy_math, bevy_platform, bevy_ptr, bevy_reflect, bevy_tasks (default-features=false), bevy_time, bevy_transform, bevy_utils`. No `std`, no `multi_threaded`, no `async_executor` unless enabled.
- Per-feature (all `bevy` → `bevy_internal/...`):
  - `std = ["bevy_internal/std"]` → enables std on app/ecs/platform/reflect/time/transform/etc.
  - `multi_threaded = ["bevy_internal/multi_threaded"]` → internally `["std","bevy_asset?/multi_threaded","bevy_ecs/multi_threaded","bevy_render?/multi_threaded","bevy_tasks/multi_threaded","bevy_transform/multi_threaded"]`
  - `async_executor = ["std","bevy_internal/async_executor"]`
  - `dynamic_linking = ["dep:bevy_dylib","bevy_internal/dynamic_linking"]`
  - `bevy_winit`, `bevy_sprite`, `bevy_ui`, `bevy_text`, `default_font`, `x11`, `wayland`, `bevy_dev_tools`, `file_watcher`, `bevy_log`, `bevy_state`, `bevy_window`, `bevy_asset`, `bevy_render`, `bevy_core_pipeline`, `bevy_camera`, `bevy_sprite_render`, `bevy_ui_render`, `track_location`, `serialize`, `debug`, `sysinfo_plugin`, `web`, `critical-section`, `libm` — each is `["bevy_internal/<same>"]`.
  - `trace = ["bevy_internal/trace","dep:tracing"]`, `trace_tracy = ["trace","bevy_internal/trace_tracy","debug"]`, `trace_chrome = [...]`
  - `bevy_debug_stepping = ["bevy_internal/bevy_debug_stepping","bevy_internal/debug"]`
  - Renamed in 0.18/0.19: `experimental_bevy_feathers`→`bevy_feathers`, `experimental_ui_widgets`→`bevy_ui_widgets`, `animation`→`gltf_animation`, `bevy_*_picking_backend`→`mesh_picking`/`sprite_picking`/`ui_picking`, `bevy_scene`→ also `bevy_world_serialization`.
- **`bevy_ecs` 0.19.1 features**: `default = ["std","bevy_reflect","async_executor","backtrace"]`. Others: `multi_threaded = ["bevy_tasks/multi_threaded"]` (**NOT default**), `std`, `trace = ["std","dep:tracing"]`, `detailed_trace`, `track_location`, `serialize`, `bevy_debug_stepping`, `hotpatching`, `reflect_functions`, `reflect_auto_register`, `critical-section`, `debug`, `backtrace`.
- **`bevy_tasks` 0.19.1 features**: `default = ["async_executor","futures-lite"]`; `multi_threaded = ["bevy_platform/std","dep:async-channel","dep:concurrent-queue","async_executor"]` (**NOT default**); `async_executor`, `futures-lite`, `async-io`.
- `bevy_app`: `default = ["std","bevy_reflect","bevy_ecs/default","error_panic_hook"]`. `bevy_time`: `default = ["std","bevy_reflect","bevy_app/default"]`.
- Minimal headless-sim Cargo line (verified feature names):
  ```toml
  bevy = { version = "0.19.1", default-features = false, features = ["std", "multi_threaded", "bevy_log"] }
  ```

---

## 2. bevy_ecs core

### Derives (`bevy::ecs::prelude::*` unless noted)
```rust
#[derive(Component)]                                   // proc_macro_derive(Component, attributes(component, require, relationship, relationship_target, entities))
#[component(storage = "SparseSet")]                    // default is Table
#[component(immutable)]                                // optional
#[component(on_add = hook_fn, on_insert = .., on_remove = .., on_discard = ..)]
#[component(clone_behavior = Ignore)]
#[require(B)]                                          // B: Default
#[require(B(1), C { x: 1, ..Default::default() }, D::One)]  // inline constructors
#[require(C = init_c())]                               // expression form
struct A;
```
- **`Resource` is now a Component**: `pub trait Resource: Component {}` (`bevy_ecs::resource::Resource`). `#[derive(Resource)]` (attrs: `component`, `require`) implements both. Resources live as components on dedicated entities tagged with `bevy::ecs::resource::IsResource`. A type cannot be both `Resource` and a normal `Component`. `IsResource` is **not** a default disabling filter, so `Query<Entity>`/`Query<()>` iterate resource entities too — filter with `Without<IsResource>` if that matters. `ResMut<T>` requires `T: Resource<Mutability = Mutable>`.
- `#[derive(SystemSet)]` and `#[derive(ScheduleLabel)]` — `bevy::ecs::schedule::{SystemSet, ScheduleLabel}` (`pub use bevy_ecs_macros::{ScheduleLabel, SystemSet}` in schedule/set.rs). Usual companions: `Clone, Debug, PartialEq, Eq, Hash`.
- `#[derive(SystemParam)]` — `bevy::ecs::system::SystemParam`, attrs `system_param`:
  ```rust
  #[derive(SystemParam)]
  struct SimParams<'w, 's> {
      cfg:   Res<'w, SimConfig>,
      cells: Query<'w, 's, (&'static Pos, &'static mut Vel)>,   // 'static on component refs
      local: Local<'s, u8>,
      cmds:  Commands<'w, 's>,
      #[system_param(validation_message = "Custom Message")] other: Res<'w, Other>,
  }
  ```
- `#[derive(Message)]`, `#[derive(Event, attributes(event))]`, `#[derive(EntityEvent, attributes(entity_event, event_target))]` — see §7.

### Entity (`bevy::ecs::entity`)
- `pub struct Entity` (u64 bits), `pub struct EntityIndex(NonMaxU32)`, `pub struct EntityGeneration(u32)`.
- `Entity::PLACEHOLDER`, `Entity::from_index(EntityIndex) -> Entity`, `Entity::from_raw_u32(u32) -> Option<Entity>`, `Entity::from_bits(u64)`, `Entity::try_from_bits(u64) -> Option`, `to_bits() -> u64`, `index() -> EntityIndex`, `index_u32() -> u32`, `generation() -> EntityGeneration`. (`Entity::from_raw`/`row()` are gone; 0.18 renamed `row`→`index`, `EntityRow`→`EntityIndex`.)
- `bevy::ecs::entity::{EntityHashMap<V>, EntityHashSet, EntityHash, EntityIndexMap, EntityIndexSet}`; `EntityHashMap<V>(HashMap<Entity, V, EntityHash>)`.

### Commands (`bevy::ecs::system::Commands`)
```rust
pub fn spawn<T: Bundle>(&mut self, bundle: T) -> EntityCommands<'_>
pub fn spawn_empty(&mut self) -> EntityCommands<'_>
pub fn spawn_batch<I>(&mut self, batch: I) where I: IntoIterator + Send + Sync + 'static, I::Item: Bundle<Effect: NoBundleEffect>
pub fn entity(&mut self, entity: Entity) -> EntityCommands<'_>          // panics later if missing
pub fn get_entity(&mut self, entity: Entity) -> Result<EntityCommands<'_>, InvalidEntityError>
pub fn queue(&mut self, command: impl Command)
pub fn insert_resource<R: Resource>(&mut self, r: R) / init_resource<R: Resource + FromWorld>() / remove_resource<R>()
pub fn run_system(&mut self, id: impl Into<SystemId> + Send) / register_system<I,O,M>(..) -> SystemId<I,O>
pub fn trigger<'a>(&mut self, event: impl Event<Trigger<'a>: Default>)
pub fn write_message<M: Message>(&mut self, message: M) -> &mut Self
pub fn run_schedule(&mut self, label: impl ScheduleLabel)
pub fn add_observer<M>(&mut self, observer: impl IntoObserver<M>) -> EntityCommands<'_>
```
`EntityCommands`: `id()`, `insert(bundle)`, `insert_if_new`, `try_insert`, `remove::<B: Bundle>()`, `try_remove`, `remove_with_requires::<B>()`, `retain::<B>()`, `clear()`, `despawn()` (**recursive via relationships; `despawn_recursive` no longer exists**), `try_despawn()`, `entry::<T>()`, `observe(..)`, `trigger(EntityEvent)`, `with_children(|spawner| ..)`, `with_child(bundle)`, `add_child(e)`, `add_children(&[e])`, `queue(impl EntityCommand)`.

### World (`bevy::ecs::world::World`)
```rust
pub fn new() -> World
pub fn spawn<B: Bundle>(&mut self, bundle: B) -> EntityWorldMut<'_>
pub fn spawn_empty(&mut self) -> EntityWorldMut<'_>
pub fn spawn_batch<I>(&mut self, iter: I) -> SpawnBatchIter<'_, I::IntoIter> where I: IntoIterator, I::Item: Bundle<Effect: NoBundleEffect>
pub fn despawn(&mut self, entity: Entity) -> bool                          // recursive
pub fn try_despawn(&mut self, entity: Entity) -> Result<(), EntityDespawnError>
pub fn entity<F: WorldEntityFetch>(&self, e: F) -> F::Ref<'_>              // Entity, [Entity; N], &[Entity] ...
pub fn entity_mut<F: WorldEntityFetch>(&mut self, e: F) -> F::Mut<'_>
pub fn get_entity<F>(..) -> Result<F::Ref<'_>, EntityNotSpawnedError>      // (0.18: Result, not Option)
pub fn get<T: Component>(&self, entity: Entity) -> Option<&T>
pub fn get_mut<T: Component<Mutability = Mutable>>(&mut self, entity: Entity) -> Option<Mut<'_, T>>
pub fn init_resource<R: Resource + FromWorld>(&mut self) -> ComponentId
pub fn insert_resource<R: Resource>(&mut self, value: R)
pub fn remove_resource<R: Resource>(&mut self) -> Option<R>
pub fn contains_resource<R: Resource>(&self) -> bool
pub fn resource<R: Resource>(&self) -> &R                                   // panics if missing
pub fn resource_ref<R: Resource>(&self) -> Ref<'_, R>
pub fn resource_mut<R: Resource<Mutability = Mutable>>(&mut self) -> Mut<'_, R>
pub fn get_resource<R: Resource>(&self) -> Option<&R>
pub fn get_resource_mut<R: Resource<Mutability = Mutable>>(&mut self) -> Option<Mut<'_, R>>
pub fn resource_scope<R: Resource, U>(&mut self, f: impl FnOnce(&mut World, Mut<R>) -> U) -> U
pub fn init_non_send<R: 'static + FromWorld>() / insert_non_send<R>(v) / non_send<R>() / non_send_mut<R>() / get_non_send(_mut)   // renamed in 0.19 from *_non_send_resource
pub fn query<D: QueryData>(&mut self) -> QueryState<D, ()>
pub fn query_filtered<D: QueryData, F: QueryFilter>(&mut self) -> QueryState<D, F>
pub fn try_query<D>(&self) -> Option<QueryState<D, ()>> / try_query_filtered<D,F>(&self)
pub fn add_schedule(&mut self, schedule: Schedule)
pub fn run_schedule(&mut self, label: impl ScheduleLabel)                   // panics if missing
pub fn try_run_schedule(&mut self, label: impl ScheduleLabel) -> Result<(), TryRunScheduleError>
pub fn schedule_scope<R>(&mut self, label, f: impl FnOnce(&mut World, &mut Schedule) -> R) -> R  / try_schedule_scope
pub fn flush(&mut self)
pub fn commands(&mut self) -> Commands<'_, '_>
pub fn write_message<M: Message>(&mut self, m: M) -> Option<MessageId<M>>
pub fn trigger<'a, E: Event<Trigger<'a>: Default>>(&mut self, event: E)    // observer/mod.rs
pub fn add_observer<M>(&mut self, observer: impl IntoObserver<M>) -> EntityWorldMut<'_>
pub fn register_system<I, O, M>(&mut self, system: impl IntoSystem<I,O,M> + 'static) -> SystemId<I, O>  // system/system_registry.rs
pub fn run_system<O: 'static>(&mut self, id: SystemId<(), O>) -> Result<O, RegisteredSystemError<(), O>>  (summary)
pub fn run_system_with<I, O>(..)  / unregister_system
pub fn clear_trackers(&mut self); increment_change_tick() -> Tick; change_tick(); last_change_tick()
pub fn register_component<T: Component>(&mut self) -> ComponentId
```
- `run_system_once`: **trait**, not in prelude — `use bevy::ecs::system::RunSystemOnce;`
  ```rust
  pub trait RunSystemOnce: Sized {
      fn run_system_once<T, Out, Marker>(self, system: T) -> Result<Out, RunSystemError> where T: IntoSystem<(), Out, Marker>;
      fn run_system_once_with<T, In, Out, Marker>(self, system: T, input: SystemIn<'_, T::System>) -> Result<Out, RunSystemError> where T: IntoSystem<In, Out, Marker>, In: SystemInput;
  }
  let n = world.run_system_once(|q: Query<&Pos>| q.iter().count()).unwrap();
  ```
- `SystemId<I: SystemInput = (), O = ()>` in `bevy::ecs::system`.

---

## 3. Queries (`bevy::ecs::system::Query<'w, 's, D, F = ()>`)

```rust
pub fn iter(&self) -> QueryIter<'_, 's, D::ReadOnly, F>
pub fn iter_mut(&mut self) -> QueryIter<'_, 's, D, F>
pub fn iter_inner(self) -> QueryIter<'w, 's, D, F>
pub fn get(&self, entity: Entity) -> Result<ROQueryItem<'_, 's, D>, QueryEntityError>
pub fn get_mut(&mut self, entity: Entity) -> Result<D::Item<'_, 's>, QueryEntityError>
pub fn get_many<const N: usize>(..) / get_many_mut / get_many_unique(_mut)
pub fn iter_many<EntityList: IntoIterator<Item: EntityEquivalent>>(&self, entities: EntityList) -> QueryManyIter<..>
pub fn iter_many_mut<EntityList: IntoIterator<Item: EntityEquivalent>>(&mut self, ..)
pub fn iter_many_unique<EntityList: EntitySet>(&self, ..) -> QueryManyUniqueIter<..>        // §10
pub fn iter_many_unique_mut<EntityList: EntitySet>(&mut self, ..)
pub fn single(&self) -> Result<ROQueryItem<'_, 's, D>, QuerySingleError>     // Result since 0.16 (confirmed)
pub fn single_mut(&mut self) -> Result<D::Item<'_, 's>, QuerySingleError>
pub fn is_empty(&self) -> bool; contains(&self, Entity) -> bool; count(&self) -> usize
pub fn par_iter(&self) -> QueryParIter<'_, 's, D::ReadOnly, F>
pub fn par_iter_mut(&mut self) -> QueryParIter<'_, 's, D, F>
pub fn par_iter_inner(self) -> QueryParIter<'w, 's, D, F>
pub fn par_iter_many<EntityList: IntoIterator<Item: EntityEquivalent>>(&self, ..)
pub fn par_iter_many_unique<EntityList: EntitySet<Item: Sync>>(&self, ..)
pub fn par_iter_many_unique_mut<EntityList: EntitySet<Item: Sync>>(&mut self, ..)
pub fn transmute_lens<NewD: SingleEntityQueryData>(&mut self) -> QueryLens<'_, NewD>; join(..); as_readonly(); reborrow()
```
Also `Single<'w, 's, D, F>` and `Populated<'w, 's, D, F>` system params (`bevy::ecs::system`).

### `QueryParIter` (`bevy::ecs::query::QueryParIter`)
```rust
pub fn batching_strategy(mut self, strategy: BatchingStrategy) -> Self
pub fn for_each<FN: Fn(QueryItem<'w, 's, D>) + Send + Sync + Clone>(self, func: FN)
pub fn for_each_init<FN, INIT, T>(self, init: INIT, func: FN)
    where FN: Fn(&mut T, QueryItem<'w, 's, D>) + Send + Sync + Clone, INIT: Fn() -> T + Sync + Send + Clone
```
- **Panics** if `ComputeTaskPool` is not initialized *when `multi_threaded` is on* (calls `bevy_tasks::ComputeTaskPool::get().thread_num()`; `get()` `expect`s). If `thread_num() <= 1` it runs a plain sequential fold on the calling thread.
- With `multi_threaded` **off** (or wasm32): `for_each_init` is a sequential `fold` on the calling thread; **no task pool access, no panic**.
- Batches are dispatched via `ComputeTaskPool::get().scope(|scope| { ... scope.spawn(async move { ... }) })` per storage/batch (`QueryState::par_fold_init_unchecked_manual`). Ordering of batch *execution* is nondeterministic; only use for data-disjoint per-entity writes or `Parallel<T>` accumulation + deterministic merge.
- `init` "may be called multiple times per thread, and the values returned may be discarded between tasks" — not a parallel fold.

### `BatchingStrategy` (`bevy::ecs::batching::BatchingStrategy`)
```rust
pub struct BatchingStrategy { pub batch_size_limits: Range<usize>, /* default 1..usize::MAX */ pub batches_per_thread: usize /* default 1 */ }
pub const fn new() -> Self;  impl Default (= new())
pub const fn fixed(batch_size: usize) -> Self          // limits = batch_size..batch_size
pub const fn min_batch_size(self, n: usize) -> Self; pub const fn max_batch_size(self, n: usize) -> Self
pub fn batches_per_thread(self, n: usize) -> Self      // asserts n > 0
pub fn calc_batch_size(&self, max_items: impl FnOnce() -> usize, thread_count: usize) -> usize
```
```rust
q.par_iter_mut().batching_strategy(BatchingStrategy::fixed(4096)).for_each(|(mut v, p)| { .. });
```

### `QueryState` (`bevy::ecs::query::QueryState<D, F>`)
```rust
pub fn new(world: &mut World) -> Self
pub fn iter<'w,'s>(&'s mut self, world: &'w World) -> QueryIter<'w,'s,D::ReadOnly,F>
pub fn iter_mut<'w,'s>(&'s mut self, world: &'w mut World) -> QueryIter<'w,'s,D,F>
pub fn par_iter<'w,'s>(&'s mut self, world: &'w World) -> QueryParIter<'w,'s,D::ReadOnly,F>
pub fn par_iter_mut<'w,'s>(&'s mut self, world: &'w mut World) -> QueryParIter<'w,'s,D,F>
pub fn query<'w,'s>(&'s mut self, world: &'w World) -> Query<'w,'s,D::ReadOnly,F>; query_mut(&mut World)
pub fn get(..) / get_mut(..) / single(..) / single_mut(..) / update_archetypes(&mut self, &World)
```

### Sorted iteration (`QueryIter`, also on `QueryManyIter`)
All take a *lens* `L: ReadOnlyQueryData + SingleEntityQueryData + 'w` (a subset/transmute of the original query; the filter of the original query is kept) and return `QuerySortedIter<'w,'s,D,F, impl ExactSizeIterator<Item=Entity> + DoubleEndedIterator + FusedIterator>`:
```rust
pub fn sort<L>(self) where for<'lw,'ls> L::Item<'lw,'ls>: Ord
pub fn sort_unstable<L>(self)              // same bound
pub fn sort_by<L>(self, compare: impl FnMut(&L::Item<'_,'_>, &L::Item<'_,'_>) -> Ordering)
pub fn sort_unstable_by<L>(self, compare: ..)
pub fn sort_by_key<L, K: Ord>(self, f: impl FnMut(&L::Item<'_,'_>) -> K)
pub fn sort_unstable_by_key<L, K: Ord>(self, f: ..)
pub fn sort_by_cached_key<L, K: Ord>(self, f: ..)
```
```rust
for (e, chunk) in q.iter().sort::<&ChunkCoord>() { .. }          // deterministic order
let v: Vec<_> = q.iter().sort_by_key::<Entity, _>(|e| e.to_bits()).collect();
```
(Sorting allocates a keyed `Vec` each call; for hot paths keep your own index `Vec<Entity>`.)

---

## 4. bevy_tasks (`bevy::tasks`)

- `TaskPoolBuilder::new().num_threads(n: usize).stack_size(n).thread_name(String).on_thread_spawn(impl Fn()+Send+Sync+'static).on_thread_destroy(..).build() -> TaskPool`. Without `multi_threaded`, `num_threads` is a no-op and `thread_num()` returns 1.
- `TaskPool::new() -> Self` (= builder default), `thread_num(&self) -> usize`, `spawn<T: Send+'static>(&self, fut: impl Future<Output=T>+Send+'static) -> Task<T>`, `spawn_local(..)`, `with_local_executor(..)`.
- **scope**:
  ```rust
  pub fn scope<'env, F, T>(&self, f: F) -> Vec<T>
      where F: for<'scope> FnOnce(&'scope Scope<'scope, 'env, T>), T: Send + 'static
  pub fn scope_with_executor<'env, F, T>(&self, tick_task_pool_executor: bool, external_executor: Option<&ThreadExecutor>, f: F) -> Vec<T>
  impl Scope { pub fn spawn<Fut: Future<Output=T> + 'scope + Send>(&self, f: Fut); spawn_on_scope(..); spawn_on_external(..) }
  ```
  **Result order (verified from source)**: `Scope::spawn` pushes each task onto a FIFO `ConcurrentQueue`; results are collected by popping that queue in order and awaiting each. Doc asserts `assert_eq!(&results[..], &[0, 1])` for two direct spawns and states: "The ordering is deterministic if you only spawn directly from the closure function"; ordering is **non-deterministic if you spawn from within tasks**. Spawn order == result order for top-level spawns. Panics inside tasks are caught and re-raised on the scope.
- Pools: `ComputeTaskPool`, `AsyncComputeTaskPool`, `IoTaskPool` (`bevy::tasks::{..}`; generated by one macro in usages.rs):
  ```rust
  pub fn get_or_init(f: impl FnOnce() -> TaskPool) -> &'static Self
  pub fn try_get() -> Option<&'static Self>
  pub fn get() -> &'static Self            // panics ("expect") if uninitialized
  // Deref<Target = TaskPool>
  ```
  ```rust
  ComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().num_threads(n).thread_name("Compute".into()).build());
  let out: Vec<u64> = ComputeTaskPool::get().scope(|s| { for c in chunks { s.spawn(async move { step(c) }); } }); // in chunk order
  ```
- `pub fn available_parallelism() -> usize` — `std::thread::available_parallelism().unwrap_or(1)`; `1` without std.
- `bevy::tasks::{Task, ParallelIterator, ParallelSlice, ParallelSliceMut, block_on, futures_lite, poll_once}`.
- **`Parallel<T>` is NOT in bevy_tasks**: it is `bevy::utils::Parallel<T: Send>` (bevy_utils/parallel_queue.rs, behind bevy_utils feature `parallel`, enabled by `bevy_ecs/std`):
  ```rust
  pub fn scope<R>(&self, f: impl FnOnce(&mut T) -> R) -> R                 // T: Default
  pub fn borrow_local_mut(&self) -> impl DerefMut<Target = T> + '_          // T: Default
  pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T>
  pub fn clear(&mut self)
  pub fn drain(&mut self) -> impl Iterator<Item = T> + '_                    // T: IntoIterator... (flattens); "The ordering is not guaranteed."
  pub fn drain_into(&mut self, out: &mut Vec<T>)                            // "The ordering is not guaranteed."
  ```
  Deterministic merge pattern: `let mut v: Vec<_> = par.drain().collect(); v.sort_unstable_by_key(|x| x.key);` then reduce in sorted order.

---

## 5. Scheduling (`bevy::ecs::schedule`)

- `Schedule::new(label: impl ScheduleLabel) -> Self`; `Schedule::default()` uses `DefaultSchedule`. `label() -> InternedScheduleLabel`.
- `add_systems<M>(&mut self, systems: impl IntoScheduleConfigs<ScheduleSystem, M>) -> &mut Self`
- `configure_sets<M>(&mut self, sets: impl IntoScheduleConfigs<InternedSystemSet, M>) -> &mut Self`
- `ignore_ambiguity(a, b)`, `run(&mut self, world: &mut World)`, `initialize(&mut self, world) -> Result<..>`, `apply_deferred(&mut self, world)`, `graph()/graph_mut()`, `systems()`, `systems_len()`.
- **`ExecutorKind` was REMOVED in 0.19.** Replacement:
  ```rust
  pub fn set_executor(&mut self, executor: impl SystemExecutor + 'static) -> &mut Self
  pub fn set_apply_final_deferred(&mut self, apply_final_deferred: bool) -> &mut Self   // default true
  // executors: bevy::ecs::schedule::{SingleThreadedExecutor, MultiThreadedExecutor, MainThreadExecutor}
  // SingleThreadedExecutor::new() is const fn; MultiThreadedExecutor::new() (cfg(feature="std"))
  // SimpleExecutor: deprecated 0.17, removed 0.18.
  pub trait SystemExecutor: Send + Sync { fn init(&mut self, &SystemSchedule); fn run(&mut self, &mut SystemSchedule, &mut World, skip: Option<&FixedBitSet>, error_handler: fn(BevyError, ErrorContext)); fn set_apply_final_deferred(&mut self, bool); }
  pub fn default_executor() -> Box<dyn SystemExecutor>   // MultiThreaded iff cfg(not wasm32, feature std, feature multi_threaded), else SingleThreaded
  ```
  ```rust
  app.edit_schedule(SimTick, |s| { s.set_executor(SingleThreadedExecutor::new()); });
  ```
- `ScheduleBuildSettings` (exact fields, defaults from `new()`):
  ```rust
  pub struct ScheduleBuildSettings {
      pub ambiguity_detection: LogLevel,   // Ignore
      pub hierarchy_detection: LogLevel,   // Warn
      pub auto_insert_apply_deferred: bool,// true
      pub use_shortnames: bool,            // true
      pub report_sets: bool,               // true
  }
  pub enum LogLevel { Ignore, Warn, Error }
  schedule.set_build_settings(ScheduleBuildSettings { ambiguity_detection: LogLevel::Error, ..Default::default() });
  ```
- `IntoScheduleConfigs<T, Marker>` (renamed from `IntoSystemConfigs` in 0.16; in prelude) methods: `in_set(impl SystemSet)`, `before(impl IntoSystemSet)`, `after(..)`, `before_ignore_deferred`, `after_ignore_deferred`, `run_if(impl SystemCondition<M>)`, `distributive_run_if`, `ambiguous_with(set)`, `ambiguous_with_all()`, `chain()`, `chain_ignore_deferred()`. `Condition` trait was renamed `SystemCondition` (0.17).
- Exclusive systems: `fn f(world: &mut World)` plain; `ApplyDeferred` is a unit-struct system (`bevy::ecs::schedule::ApplyDeferred`, in prelude); lowercase `apply_deferred` fn: **not found in 0.19.1 source** (use `ApplyDeferred`).
- **Determinism**: the `MultiThreadedExecutor` runs systems concurrently only when their access sets are disjoint ("Non-conflicting systems can run in parallel"); conflicting systems without explicit ordering are *ambiguities* whose relative order is unspecified. Set `ambiguity_detection: LogLevel::Error` so schedule build fails on any unordered conflicting pair, then order with `.chain()` / `.before/.after` / sets. With all conflicts ordered, MultiThreaded output == SingleThreaded output. For a bit-exact fallback/testing path, `set_executor(SingleThreadedExecutor::new())`. Deferred commands: applied at explicit `ApplyDeferred` points (auto-inserted between ordered systems when `auto_insert_apply_deferred`) and at end if `set_apply_final_deferred(true)`.
- **Main schedule** (`bevy::app::*`; `MainScheduleOrder` default):
  `Main` runs once: `startup_labels = [PreStartup, Startup, PostStartup]`, then every update `labels = [First, PreUpdate, RunFixedMainLoop, Update, SpawnScene, PostUpdate, Last]`. `StateTransition` is inserted by `bevy_state::StatesPlugin` (feature `bevy_state`) between `PreUpdate` and `RunFixedMainLoop`. `RunFixedMainLoop` runs `FixedMain` = `[FixedFirst, FixedPreUpdate, FixedUpdate, FixedPostUpdate, FixedLast]` zero or more times. All labels are `#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)] pub struct X;`.
- **Time** (`bevy::time`):
  - `pub struct Time<T: Default = ()>`; `Res<Time>` = `Time<()>`, which is `Time<Virtual>::as_generic()` normally and `Time<Fixed>::as_generic()` while inside `FixedMain` (verified in `run_fixed_main_schedule`). Accessors: `delta() -> Duration`, `delta_secs() -> f32`, `delta_secs_f64()`, `elapsed()`, `elapsed_secs()`, `elapsed_secs_f64()`, `advance_by(Duration)`, `advance_to(Duration)`, `context()`.
  - `Time<Fixed>`: `DEFAULT_TIMESTEP = 15625µs` (64 Hz). `Time::<Fixed>::from_duration(Duration)`, `from_seconds(f64)`, `from_hz(f64)`; `timestep()`, `set_timestep(Duration)`, `set_timestep_seconds(f64)`, `set_timestep_hz(f64)`, `overstep() -> Duration`, `overstep_fraction() -> f32`, `overstep_fraction_f64()`, `discard_overstep(Duration)`. `app.insert_resource(Time::<Fixed>::from_hz(8.0));`
  - `Time<Virtual>`: `DEFAULT_MAX_DELTA = 250ms`; `from_max_delta(Duration)`, `max_delta()`, `set_max_delta(Duration)` (asserts non-zero), `relative_speed() -> f32`, `relative_speed_f64()`, `set_relative_speed(f32)`, `set_relative_speed_f64(f64)`, `effective_speed()`, `pause()`, `unpause()`, `is_paused()`, `was_paused()`.
  - `TimeUpdateStrategy` resource: `Automatic` (default), `ManualInstant(Instant)`, `ManualDuration(Duration)`, `FixedTimesteps(u32)` — use `ManualDuration` for deterministic headless stepping. `TimePlugin`, `TimeSystems` set. `Timer::new(Duration, TimerMode)`, `from_seconds(f32, TimerMode)`, `tick(Duration)`, `just_finished()`, `is_finished()` (renamed from `finished` in 0.17), `is_paused()`, `reset()`.

---

## 6. bevy_app (`bevy::app`)

```rust
App::new() -> App; App::empty() -> App
pub fn add_plugins<M>(&mut self, plugins: impl Plugins<M>) -> &mut Self
pub fn add_systems<M>(&mut self, schedule: impl ScheduleLabel, systems: impl IntoScheduleConfigs<ScheduleSystem, M>) -> &mut Self
pub fn configure_sets<M>(&mut self, schedule: impl ScheduleLabel, sets: impl IntoScheduleConfigs<InternedSystemSet, M>) -> &mut Self
pub fn add_schedule(&mut self, schedule: Schedule) -> &mut Self
pub fn init_schedule(&mut self, label: impl ScheduleLabel) -> &mut Self
pub fn get_schedule(&self, label) -> Option<&Schedule> / get_schedule_mut
pub fn edit_schedule(&mut self, label: impl ScheduleLabel, f: impl FnMut(&mut Schedule)) -> &mut Self
pub fn configure_schedules(&mut self, settings: ScheduleBuildSettings) -> &mut Self
pub fn insert_resource<R: Resource>(&mut self, r: R) -> &mut Self; init_resource<R: Resource + FromWorld>()
pub fn add_message<M: Message>(&mut self) -> &mut Self          // was add_event
pub fn add_observer<M>(&mut self, observer: impl IntoObserver<M>) -> &mut Self
pub fn register_system<I,O,M>(..) -> SystemId<I,O>
pub fn world(&self) -> &World; world_mut(&mut self) -> &mut World; main()/main_mut() -> SubApp
pub fn update(&mut self)                     // runs Main once (+ finish/cleanup of plugins)
pub fn run(&mut self) -> AppExit
pub fn set_runner(&mut self, f: impl FnOnce(App) -> AppExit + 'static) -> &mut Self
pub fn should_exit(&self) -> Option<AppExit>
pub fn allow_ambiguous_component<T: Component>() / allow_ambiguous_resource<T: Resource>() / ignore_ambiguity(..)
pub fn set_error_handler(&mut self, handler: ErrorHandler) -> &mut Self
```
- `AppExit` is a **Message**: `#[derive(Message, Debug, Clone, Default, PartialEq, Eq)] pub enum AppExit { #[default] Success, Error(NonZero<u8>) }`. Send with `MessageWriter<AppExit>` / `world.write_message(AppExit::Success)`.
- `pub trait Plugin: Downcast + Any + Send + Sync { fn build(&self, app: &mut App); fn ready(&self, _: &App) -> bool; fn finish(&self, _: &mut App); fn cleanup(&self, _: &mut App); fn name(&self) -> &str; fn is_unique(&self) -> bool; }`
- `pub trait PluginGroup: Sized` → `PluginGroupBuilder` with `set<T: Plugin>(self, plugin: T) -> Self`, `add`, `add_group`, `add_before<Target>`, `add_after<Target>`, `enable<T>`, `disable<T>`, `finish(self, &mut App)`.
- `MinimalPlugins` = `TaskPoolPlugin, FrameCountPlugin (bevy_diagnostic), TimePlugin, ScheduleRunnerPlugin` (+ `CiTestingPlugin` under `bevy_ci_testing`).
- `DefaultPlugins` (feature-gated members, in order): `PanicHandlerPlugin, LogPlugin, TaskPoolPlugin, FrameCountPlugin, TimePlugin, TransformPlugin, DiagnosticsPlugin, InputPlugin, InputFocusPlugin, InputDispatchPlugin, ScheduleRunnerPlugin, WindowPlugin, TerminalCtrlCHandlerPlugin, WebAssetPlugin, AssetPlugin, WorldSerializationPlugin, ScenePlugin, WinitPlugin, RenderPlugin, ImagePlugin, MeshPlugin, CameraPlugin, LightPlugin, PipelinedRenderingPlugin, CorePipelinePlugin, PostProcessPlugin, AntiAliasPlugin, SpritePlugin, SpriteRenderPlugin, ClipboardPlugin, TextPlugin, UiPlugin, UiRenderPlugin, GltfPlugin, PbrPlugin, AudioPlugin, GilrsPlugin, AnimationPlugin, GizmoPlugin, GizmoRenderPlugin, StatesPlugin, HotPatchPlugin, UiWidgetsPlugins, DefaultPickingPlugins`.
- `TaskPoolPlugin { pub task_pool_options: TaskPoolOptions }`:
  ```rust
  pub struct TaskPoolOptions { pub min_total_threads: usize /*1*/, pub max_total_threads: usize /*usize::MAX*/, pub io: TaskPoolThreadAssignmentPolicy, pub async_compute: TaskPoolThreadAssignmentPolicy, pub compute: TaskPoolThreadAssignmentPolicy }
  pub struct TaskPoolThreadAssignmentPolicy { pub min_threads: usize, pub max_threads: usize, pub percent: f32, pub on_thread_spawn: Option<Arc<dyn Fn() + Send + Sync + 'static>>, pub on_thread_destroy: Option<Arc<dyn Fn() + Send + Sync + 'static>> }
  // defaults: io {1,4,0.25}, async_compute {1,4,0.25}, compute {1,usize::MAX,1.0 /* "whatever is left" */}
  pub fn with_num_threads(thread_count: usize) -> Self   // sets min_total == max_total == n, rest default
  pub fn create_default_pools(&self)                      // IoTaskPool/AsyncComputeTaskPool/ComputeTaskPool::get_or_init
  ```
  ```rust
  app.add_plugins(MinimalPlugins.set(TaskPoolPlugin { task_pool_options: TaskPoolOptions {
      compute: TaskPoolThreadAssignmentPolicy { min_threads: n, max_threads: n, percent: 1.0, on_thread_spawn: None, on_thread_destroy: None },
      ..TaskPoolOptions::with_num_threads(n + 2) } }));
  ```
- `ScheduleRunnerPlugin { pub run_mode: RunMode }`; `RunMode::{ Loop { wait: Option<Duration> }, Once }`; `ScheduleRunnerPlugin::run_once()`, `::run_loop(wait_duration: Duration)`. `MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0/60.0)))`.

---

## 7. Events vs Messages (0.17 split, names confirmed in 0.19.1)

- **Message** = buffered, double-buffered `Messages<M>` resource: `pub trait Message: Send + Sync + 'static {}`; `#[derive(Message)]`; `app.add_message::<M>()`; `MessageWriter<'w, M>::{write(M) -> MessageId<M>, write_batch(impl IntoIterator<Item=M>), write_default()}`; `MessageReader<'w,'s, M>::{read() -> MessageIterator, read_with_id(), len(), is_empty(), clear()}`; `MessageMutator`; `World::write_message`, `Commands::write_message`. Cleared by `message_update_system` (in `First`).
- **Event** = observer-triggered, immediate: `pub trait Event: Send + Sync + Sized + 'static { type Trigger<'a>: Trigger<Self>; }`; `#[derive(Event)]` (attr `#[event(trigger = ..)]`). Fire: `world.trigger(ev)` / `commands.trigger(ev)`. Observe: `world.add_observer(|ev: On<Speak>| ..)`, `app.add_observer(..)`, `commands.add_observer(..)`, `entity_commands.observe(..)`.
- **`Trigger` (the observer param) was renamed `On`** in 0.17: `pub struct On<'w, 't, E: Event, B: Bundle = ()>`; `Deref/DerefMut<Target = E>`; methods `event()`, `event_mut()`, `trigger()`, `trigger_mut()`, `observer() -> Entity`, `original_event_target() -> Entity`, `propagate(bool)`, `get_propagate()`. (`Trigger<E>` now names the low-level trait `bevy::ecs::event::Trigger<E>` — do not confuse.)
- **EntityEvent**: `pub trait EntityEvent: Event { fn event_target(&self) -> Entity; }`; `#[derive(EntityEvent)]` auto-uses a field named `entity`, or mark with `#[event_target]`; `#[entity_event(propagate)]`, `#[entity_event(propagate = &'static ChildOf)]` (relationship to walk), `#[entity_event(propagate, auto_propagate)]`. `EntityCommands::trigger<E: EntityEvent>(..)`.
  ```rust
  #[derive(EntityEvent)] #[entity_event(propagate)] struct Click { entity: Entity }
  #[derive(EntityEvent)] struct Explode(#[event_target] Entity);
  world.add_observer(|mut c: On<Click>| { c.propagate(false); });
  ```
- Lifecycle events live in `bevy::ecs::lifecycle::{Add, Insert, Replace, Remove, Despawn, RemovedComponents}` (the `removal_detection` module is gone).

---

## 8. Change detection (`bevy::ecs::change_detection`, now a module dir)

- Filters (`bevy::ecs::query`, in prelude except `Spawned`): `Changed<T>`, `Added<T>`, `Spawned`, `With<T>`, `Without<T>`, `Or<(..)>`, `Allow<T>`.
- `Ref<'w, T>` (read + ticks; `Clone/Copy` returns `Ref<T>` as of 0.19), `Mut<'w, T>`, `MutUntyped<'w>`; `Mut::{into_inner, reborrow, map_unchanged, filter_map_unchanged, as_deref_mut}`.
- ```rust
  pub trait DetectChanges { fn is_added(&self) -> bool; fn is_changed(&self) -> bool; fn is_added_after(&self, Tick) -> bool; fn is_changed_after(&self, Tick) -> bool; fn last_changed(&self) -> Tick; fn added(&self) -> Tick; fn changed_by(&self) -> MaybeLocation; }
  pub trait DetectChangesMut: DetectChanges { type Inner: ?Sized; fn set_changed(&mut self); fn set_added(&mut self); fn set_last_changed(&mut self, Tick); fn set_last_added(&mut self, Tick); fn bypass_change_detection(&mut self) -> &mut Self::Inner; fn set_if_neq(&mut self, value: Self::Inner) -> bool; fn replace_if_neq(&mut self, value: Self::Inner) -> Option<Self::Inner>; fn clone_from_if_neq<T>(&mut self, value: &T) -> bool; }
  ```
  Implemented for `Mut`, `ResMut`, `NonSendMut`, `MutUntyped`. `*res.bypass_change_detection() = x;` skips marking changed.
- `RemovedComponents<'w, 's, T: Component>` (`bevy::ecs::lifecycle`, in prelude): `read() -> RemovedIter`, `read_with_id()`, `len()`, `is_empty()`, `clear()`.
- `Tick`, `ComponentTicks`, `ComponentTickCells` moved from `component` to `change_detection` (0.18). `World::clear_trackers()`.

---

## 9. Migration highlights 0.16 → 0.19 (old → new)

1. `Events<E>` → `Messages<M>`; `EventWriter/EventReader` → `MessageWriter/MessageReader`; `app.add_event::<E>()` → `add_message::<M>()`; `#[derive(Event)]` for buffered → `#[derive(Message)]`; `send_event` → `write_message` (World/Commands) (0.17).
2. Observer param `Trigger<E>` → `On<E>`; `Trigger` is now the low-level trait (0.17).
3. `ExecutorKind::{MultiThreaded,SingleThreaded,Simple}` + `Schedule::set_executor_kind` → `Schedule::set_executor(SingleThreadedExecutor::new() | MultiThreadedExecutor::new())`; `SimpleExecutor` removed (0.18/0.19).
4. `Resource` is now `: Component`; resources are entities; `insert_non_send_resource/init_non_send_resource/non_send_resource_mut` → `insert_non_send/init_non_send/non_send_mut` (0.19). `Query<Entity>` sees resource entities (`Without<IsResource>`).
5. `bevy` default features → `["2d","3d","ui","audio"]`; `ui` not implied by `2d/3d` (0.19).
6. `Query::single()/single_mut()` return `Result<_, QuerySingleError>` (0.16); `get_single` removed.
7. `Entity::from_raw(u32)` gone → `Entity::from_raw_u32(u32) -> Option<Entity>` / `from_index(EntityIndex)`; `row()` → `index() -> EntityIndex`, `index_u32()`; `EntityRow` → `EntityIndex` (0.18). `Entity::PLACEHOLDER` still exists.
8. `World::get_entity` returns `Result<_, EntityNotSpawnedError>` (not `Option`); `EntityDoesNotExistError` → `InvalidEntityError`/`EntityNotSpawnedError` (0.18).
9. `bevy::utils::HashMap/HashSet` → `bevy::platform::collections::{HashMap, HashSet, HashTable}` (0.16). `EntityHashMap` → `bevy::ecs::entity::EntityHashMap`. `SyncCell` → `bevy::platform::cell::SyncCell` (0.17). `HashMap::get_many_*` → `get_disjoint_*` (0.18).
10. `Parallel<T>` → `bevy::utils::Parallel` (not `bevy::tasks`).
11. `IntoSystemConfigs`/`IntoSystemSetConfigs` → `IntoScheduleConfigs` (0.16). `Condition` → `SystemCondition` (0.17). System sets `*Set` → `*Systems` (e.g. `TransformSystem` → `TransformSystems`) (0.17).
12. `apply_deferred` fn → `ApplyDeferred` unit struct.
13. `EntityCommands::despawn_recursive()`/`despawn_descendants()` → `despawn()` is recursive; `despawn_children()`. `Parent`/`Children` → `ChildOf(pub Entity)` + `Children(Vec<Entity>)` via `#[relationship(relationship_target = Children)]` (0.16). `clear_children/remove_children/remove_child` → `detach_all_children/detach_children/detach_child` (0.18).
14. `RemovedComponents` moved to `bevy::ecs::lifecycle`; `OnAdd/OnInsert/OnRemove` → `Add/Insert/Remove/Replace/Despawn` lifecycle events (0.17).
15. `run_system_once` is `bevy::ecs::system::RunSystemOnce` trait, returns `Result<Out, RunSystemError>`.
16. `App::run()` returns `AppExit` (enum `Success | Error(NonZero<u8>)`), a `Message` (0.14+, still true).
17. `Timer::finished()` → `is_finished()`, `paused()` → `is_paused()` (0.17).
18. `Name` is `pub struct Name(pub HashedStr)`; `Name::new(impl Into<Cow<'static, str>>)`, `as_str()`, `set()`.
19. `Handle::Weak` → `Handle::Uuid` + `uuid_handle!`; `Handle::clone_weak()` → `clone()`; `Assets::insert` returns `Result` (0.17). **UNVERIFIED beyond guide text.**
20. Camera/render types moved: `Camera2d/Camera3d/Camera/Projection` → `bevy_camera`; `Camera.hdr` → `Hdr` component; `Sprite` requires `Anchor` component; `Text2d` in `bevy_sprite`; `JustifyText` → `Justify`; `TextFont::font_size: f32` → `FontSize::Px(f32)`, `TextFont::font: Handle<Font>` → `FontSource`; `LineHeight` separate component; `BorderRadius` now a `Node` field (0.17–0.19). **UNVERIFIED beyond guide text.**
21. `StateScoped` → `DespawnOnExit` (0.17). `StateTransition` schedule comes from `bevy_state::StatesPlugin`.
22. `Tick/ComponentTicks` moved `component` → `change_detection`; `TickCells` → `ComponentTickCells` (0.18).
23. `Access::add_component_read/write` → `add_read/write`; `System::type_id()` → `system_type()` (0.19).
24. `DefaultErrorHandler` → `FallbackErrorHandler` (0.19); `System::run` returns `Result` (0.17).
25. `bevy_scene::Scene` → `WorldAsset` in `bevy_world_serialization` (0.19). `Task<T>` dropped on web now cancels (`detach()` for old behavior) (0.19).
26. `SystemParam` validation moved to fetch time (0.19) — missing `Res<T>` errors surface when the system runs, not at schedule init.

---

## 10. EntitySet / unique iteration (`bevy::ecs::entity`)

- `pub unsafe trait EntityEquivalent: ContainsEntity + Eq {}` (impl for `Entity`, `&Entity`, ...).
- `pub trait EntitySet: IntoIterator<IntoIter: EntitySetIterator> {}` — blanket impl for any `IntoIterator` whose iterator is an `EntitySetIterator` (guaranteed-unique). Sources: `Query::iter()` over `Entity`, `UniqueEntityVec`, `EntityHashSet`, `Children` iterators.
- `pub type UniqueEntityVec = UniqueEntityEquivalentVec<Entity>` (`entity/unique_vec.rs`): `new()`, `unsafe from_vec_unchecked(Vec<T>)`, `unsafe push(T)`, `len()`, `as_slice() -> &UniqueEntityEquivalentSlice<T>`, `into_inner() -> Vec<T>`. Also `UniqueEntitySlice`, `UniqueEntityArray<N>`, `UniqueEntityIter<I>`.
- Build safely: `EntitySetIterator::collect_set::<UniqueEntityVec>()` (`pub trait FromEntitySetIterator<A>: FromIterator<A> { fn from_entity_set_iter<T: EntitySet<Item = A>>(T) -> Self }`).
  ```rust
  let ids: UniqueEntityVec = q_ids.iter().collect_set();          // q_ids: Query<Entity>
  for (mut a, b) in q.iter_many_unique_mut(&ids) { .. }          // &UniqueEntityVec: EntitySet
  q.par_iter_many_unique_mut(&ids).for_each(|item| ..);         // EntityList: EntitySet<Item: Sync>
  ```
- Exact names present in 0.19.1: `Query::{iter_many_unique, iter_many_unique_mut, par_iter_many_unique, par_iter_many_unique_mut, get_many_unique, get_many_unique_mut}`.
