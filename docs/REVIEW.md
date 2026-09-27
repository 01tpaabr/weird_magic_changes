# Review 2026-09-26: decisions for the owner

Temporary: delete when every item below is decided and done.

A full review found 200 issues across the compiler, linter, VM, actors, sim, stage, store,
scenarios and CLI. Independent agents checked each one, and most were reproduced with a
small input. 113 were plain bugs, edge cases, wrong docs or cleanups, and are fixed on `main`
(`b6fe7dd..5f8f299`), each with a test. The items below change the language, the saves or
the code's shape, or have two reasonable answers, so they are yours to decide. Each has a
recommendation. Answer with the ids, e.g. "L1 a, L2 rec, E3 no".

Already settled by the fixes, no decision needed: the lint radius check now respects
shadowing (was lint-10); the compiler refuses more than 65 534 kinds (vm-14, §18 row); saved
starts follow the rules they open under (sim-19, via scenario-2).

## Language: what rule authors see

**L1. A sub that acts, called from a `when` condition** (compiler-40, vm-11). `when step() > 0 => idle`
with an acting `step`: the action happens while the condition is evaluated. If the condition
is then false, the action stays pending and carries into later rules.
- (a) Compile error when a condition calls a sub that may act or `next`. Conditions are pure. **Recommended.**
- (b) The VM rolls the action back when the condition fails: one extra op per rule, every think.
- (c) Define it as a feature ("an action in a condition commits").

**L2. Several `mark`s in one think** (vm-10). Only the last one counts: other channels are
dropped and marks on one channel don't add up, though the docs and the opcode comment say they add.
- (a) Accumulate per channel, saturating, as documented. No shipped content marks twice. **Recommended.**
- (b) One mark per think: a second one traps.

**L3. The result of a `take`/`give` that moves nothing** (actors-8). Today it is `OK`. The
fix already on `main` stops it from waking the target, but `result` doesn't change.
- (a) Keep `OK`, and document "compare your need".
- (b) `BLOCKED` when nothing moved, like `move`. **Recommended.**
- (c) `MISSED` when the source is empty, `BLOCKED` when the receiver is full.

**L4. The `ground` and `feature` senses** (compiler-41, vm-13). They return internal numbers
that nothing in the language can name. `ground == water` compiles, and compares with the
*need* `water`.
- (a) Delete both senses; `is(here, water)` / `is(here, rock)` already say it. **Recommended.**
- (b) Make them comparable: `ground == water` resolves at compile time.
- (c) Keep them, documented as opaque.

**L5. `let`, sub parameters and `as` bindings may hide a need or mem** (compiler-42).
`let food = 5  food += 1h` silently misses the need. A trait parameter with that name is
already an error.
- (a) Error when a local, binding or member-sub parameter hides a need or mem; const shadowing stays allowed. **Recommended.**
- (b) Refuse all shadowing, consts included.
- (c) Warning only, and document the precedence.

**L6. A bare `return` swallows the next token** (compiler-38). `choose { 20: return  80: move north }`
fails, and `return` followed by `h()` on the next line returns `h()`'s value.
- (a) `return` takes a value only on its own line. **Recommended.**
- (b) Fix only the `choose` case.
- (c) Require `return;` for a bare return.

**L7. `count P within r` takes an additive radius** (compiler-37). `count a within 3 - 1 > 2`
means a radius of 2, not the count minus 1.
- (a) The radius is one term (a literal, a name or a parenthesis), so operators apply to the count. **Recommended.**
- (b) Keep it, document it, and lint a binary radius.

**L8. `random free` is capped by `sight`** (compiler-39). A kind with `sight 0` never finds a
free neighbour, though `free(east)` works at sight 0.
- (a) `random free` always looks at the 8 neighbours (a VM change). **Recommended.**
- (b) Document it, and lint `random free` in a sight-0 kind.

**L9. Start state when a kind redeclares inherited states** (compiler-36). Today the parents'
states come first, so a kind listing `state B` then an inherited `state A` starts in A.
- (1) Keep parents first, the same rule as needs and mems, and document it. **Recommended.**
- (2) The kind's own order wins. This changes existing kinds' start states.

**L10. What `look` is** (docs-4). The docs call it "the palette variant", but the renderer never reads it.
- (a) Fix the docs: a public byte other actors read, not drawn. **Recommended now.**
- (b) Draw it (per-look colours in the rules), when a kind needs a visible state.

## Engine and saves

**E1. Hot reload drops a removed kind's starts for good** (sim-18). Later shares then move to
other cells, and `world.wmc` loses the starts.
- (a) Keep dropping, but report it in play's notice and document it.
- (b) Keep the full start list; a save resolves starts leniently (a missing kind's share places nobody but keeps its slot), with a report. **Recommended.**

**E2. One unreadable chunk file stops the whole streaming batch** (sim-20). Nothing
generates, far chunks stop unloading, and every save can fail. The file is now named in the error.
- (a) Keep aborting.
- (b) Handle each chunk on its own: load the rest, leave the bad ones unloaded (never generated over), and report them. **Recommended.**
- (c) Quarantine: rename it `.bad`, regenerate, and report.

**E3. Target reach differs between senses and actions** (vm-12). Senses stop at the 3x3 chunk
halo (beyond reads as rock), `spawn` reaches ±127 into any loaded chunk, and beyond that it traps.
- (a) The halo is the one reach: a spawn beyond it is `BLOCKED`, not a trap, and the reach is documented. **Recommended.**
- (b) Same, and point senses beyond the halo read as "unknown".
- (c) Keep it as is.

**E4. A save whose packs are missing** (app-12). It falls back to the built-in rules, and the
next save forgets its packs.
- (a) Refuse to open without `--rules`.
- (b) Remember the original packs for the session and keep writing them.
- (c) Keep it, and document it. **Recommended** (the usual case is a deleted worktree).

**E5. `water_scale` below 1** (the part of stage-store-1 left out). It no longer overflows,
but values in (0, 1) give white-noise terrain.
- (a) Refuse `water_scale < 1` in scenarios and saves.
- (b) Allow it, and document that small scales are noise. **Recommended**, unless you never want noise terrain.

## Code shape and tooling

**T1. Split `compile.rs`** (compiler-43), now ~6k lines: lexer, AST, parser, inheritance, codegen and tests.
- (a) Full split into `compile/{lex,ast,parse,resolve,gen}.rs`.
- (b) Only move the tests to `compile/tests.rs` now; do (a) when the next big compiler feature starts. **Recommended.**
- (c) Leave it.

**T2. Split `sim.rs`** (sim-22): schedule, lifecycle, streaming, dev tools and the `expect` evaluator in one file.
- (a) Full split: `sim/{world,tools,tests}.rs`.
- (b) Only move the `expect` evaluator next to its parser (`scenario/expect.rs`). **Recommended.**
- (c) Leave it.

**T3. Dead `rng_for` and `par_for`** (sim-21). Nothing calls them, but the skills prescribe them. `rng_for` is the only user of the `rand` and `rand_xoshiro` crates.
- (a) Delete both, drop the two crates from sim-core, and point the skills at `hash_cell`/`splitmix64`/`vm::rng_base`, which the VM uses. **Recommended.**
- (b) Keep them, and say they are unused.

**T4. Eleven hand-built flat-world tests in `sim.rs`** (sim-23).
- (a) Convert all to scenario text.
- (b) Convert each one when it's next touched. **Recommended.**
- (c) Leave them.

**T5. Life-counter names** (actors-14). `play` says `grew`, `run` and scenarios say `became`, and `expect traps` isn't accepted.
- (a) Rename play's word only.
- (b) (a), plus `expect thinks` / `expect traps` in scenarios. **Recommended.**

**T6. Scenario legend entries the map never uses** (scenario-12) are never checked, so a misspelled kind passes `lint --strict`.
- (a) Parse error: "`F` is in the legend but not on the map". **Recommended.**
- (b) Check their names against the rules, but allow them.
- (c) A lint warning only.

**T7. Placement errors** (scenario-13) name no line, print `with` values in raw ticks, and quote map-drawn starts as `start` lines that aren't in the file.
- (a) Keep a line per start, not saved, and report `file:line`, with units in values. **Recommended.**
- (c) Only improve the text (units, both starts named).
