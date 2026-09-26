# Step 8 build plan: traits, scenarios, packs, author tooling

**Temporary.** This file is the working plan for step 8 of `docs/ACTORS.md` §11. Delete it
in the last commit of step 8, after moving the durable parts into `ACTORS.md`,
`ARCHITECTURE.md` and `RULES.md`. Tick the checklist at the end as commits land.

Approved by the owner on 2026-09-25. The goal, in the owner's words: a base language and
framework so that *other people* can write their own entities in isolated rules files, the
world handles entities nobody anticipated, and the save defines only the physical map and
where each kind starts. Everything else is input.

Read `CLAUDE.md` first (priorities, commands, workflow rules), then `docs/ACTORS.md` §1–§6
(the actor model and the language as built) and `docs/RULES.md` (the language as an author
sees it). This plan assumes both.

## 0. Decisions already taken (do not reopen)

| # | Decision | Consequence |
|---|---|---|
| D1 | A kind's name in a predicate matches the kind **and every kind that extends it** (family matching). `only NAME` matches the kind exactly. | Kinds are numbered so a family is a contiguous id range; a kind test is a range check. |
| D2 | Traits are a separate `trait` keyword, never a kind. A kind extends **at most one concrete kind** plus any number of traits. Traits extend traits. | Traits have no id, no rows, cannot be spawned, become or matched by name. Roles are tags. |
| D3 | Placement leaves the rules entirely. The scenario file says where each kind starts, by name. | `place` is removed from the grammar. A default scenario is built into the binary. |
| D4 | Opening a save whose rows use a kind the loaded rules lack is **refused**, with the list. | Hot reload keeps dropping rows of a kind you deleted (an explicit action while iterating). |
| D5 | Everything loaded is global: one namespace across all files and packs. | Duplicate names are compile errors naming both files. No `include`, no `use`, no `pub`. |
| D6 | Rules are inherited only where `inherit` is written, per rule list. A list with no `inherit` gets every ancestor's rules appended. | A recoloured `kind wolf extends fox { color ... }` still behaves like a fox; a chick can take the chicken's drinking without the hen's egg rule. |
| D7 | Traits take parameters: `trait drinker(thirsty)`, `extends drinker(90min)`. Arguments are constant expressions. | One `drinker` serves chicken, fox and bee with different thresholds. |

## 1. Ground truth: where things are today

Commit `b2b0ac4` (2026-09-25) is the starting point. Steps 1–7 of §11 are done.

| Area | File | What to know |
|---|---|---|
| Lexer, parser, codegen | `crates/sim-core/src/rules/compile.rs` | `compile_files(&[(name, text)])` → `Kinds`. `Items { kinds, subs, consts }`; `KindAst` (glyph, tags, cadence_shift, sight, fuel, food, bite, place, color, cover, needs, mems, rules, states); `Rule { at, arrow_line, cond, body }`; `Cond`, `Stmt`, `Pred`, `Expr`, `Target`, `Arg`. `Parser::kind()` parses decls then `rules()` then `state` blocks. `Gen::generate()` checks duplicates, assigns tags, emits per kind: reflex rules, then each state behind a `Sense::State == s` dispatch, then a `Halt`; file subs once, shared. `Gen::rules()` also fills `DebugInfo.rules`. `KEYWORDS` is the reserved-word list; `sense_named()` the sense names. `fold()` evaluates constant expressions. |
| Kind table | `crates/sim-core/src/rules/mod.rs` | `KindDef` (id, name, glyph, tags: u64 bitset, cadence_shift, sight, fuel, food, bite, needs: Vec<NeedDef>, mems: Vec<String>, states: u8, entry, place, color, cover). `Kinds` (defs, glyphs, colors, tag_bits, placement, code, consts, subs, scents, debug, hash, remaps). `hash_all()` mixes every def field, the code, consts and subs: it is the rules hash folded into the world checksum. `DebugInfo { files, rules: Vec<RuleInfo>, states: Vec<Vec<String>>, subs }` is **not** hashed. `Remap::between` maps needs/mems by name for `become`. |
| VM | `crates/sim-core/src/rules/vm.rs` | `OpCode`, `Sense`, `Action`, `pred` (FREE, BARE, GROUND_BASE, FEATURE_BASE, TAG_BASE = 0x1_0000, LOOK_BASE = 0x0100_0000, `kind_look()`). `matches(pred, cells, actors, i, tags)` is the one place a predicate is tested against a cell. `Halo { chunks: [Option<(&ChunkCells, &ChunkActors)>; 9], tags: &[u64] }`. `think` / `think_traced` / `run::<TRACE>`. |
| Built-in rules | `crates/sim-core/src/rules/builtin.rs`, `rules/*.rules` | `FILES` (animals, bees, grass, plants, `include_str!`), constants CHICKEN=0, EGG=1, CHICK=2, FOX=3, FLOWER=4, HIVE=5, BEE=6, GRASS=7, SEED=8, TREE=9, `ORACLE_PLANTS` (frozen plants text for the compiler oracle test). |
| Actor phases | `crates/sim-core/src/actors/systems.rs` | `halo_of()` builds the `Halo` per chunk per tick. `think_one::<TRACE>`, `explain`, `Explained`. Nothing else here changes in step 8. |
| Rows | `crates/sim-core/src/actors/mod.rs` | `ActorPub` (kind: u16, ...), `ActorMind` (needs[4], mem[12], state, ...), `NEED_SLOTS = 4`, `MEM_SLOTS = 12`, `ActorsMut`, `validate()`. |
| World | `crates/sim-core/src/sim.rs` | `WorldConfig { seed, width, height, params }`, `SimConfig { seed, params, initial_width, initial_height }` (Copy), `install_with(world, kinds)`, `create`, `new_world_with`, `open(world, store)` (refuses when `meta.kinds != ours`), `open_world_with`, `meta()`, `save()`, `ensure_loaded()`, `load_chunks()` (reads a chunk file, validates, shifts frozen clocks, inserts; else generates), `checksum()` = splitmix(splitmix(stage checksum ^ tick) ^ rules hash), `place_actor`, `explain`, `find_uid`. |
| Worldgen | `crates/sim-core/src/stage/worldgen.rs` | `GenParams { water_scale, water_level, rock_on_soil, rock_on_water }` (f32). `generate_chunk(seed, params, kinds, coord, out)`: terrain per cell from `gen_cell` (private), then for each walkable cell `kinds.placed(placement_hash(seed, x, y))`. `generate_many`. Streams: STREAM_GROUND, STREAM_ROCK, STREAM_UID, STREAM_PLACE. |
| Store | `crates/sim-core/src/store.rs` | `FORMAT_VERSION = 8`. `WorldMeta { seed, tick, initial_width, initial_height, params, kinds: Vec<String>, rules_hash }`, `read_meta`/`write_meta` (hand-written LE layout: bump the version when it changes), `read_chunk`/`write_chunk` (cell layers: ground, feature, occupant, cover, then `SCENT_CHANNELS` scent layers; then rows), `saved_chunks()`, `has_chunk`. |
| Reload | `crates/sim-core/src/reload.rs` | `plan(old: &Kinds, new: &Kinds) -> Plan` (per old kind: new id, need/mem slot maps, state map; per new scent channel: old channel), `remap(plan, cells, pubs, minds, dropped)`, `reload_rules(world, store, new)` (remaps loaded chunks, rewrites unloaded chunk files, writes meta, resets `Tally`). |
| Stage | `crates/sim-core/src/stage/mod.rs` | `SCENT_CHANNELS = 2`, `ChunkCells { ground, feature, occupant, cover, scent: [[u8; 4096]; SCENT_CHANNELS] }`, `Stage`, `checksum()` (pure state, no rules hash). |
| App | `crates/app/src/main.rs`, `lib.rs`, `play.rs`, `why.rs` | Commands `show`, `play`, `run`, `why`, `lint`; `config(args)` turns `[w h seed]` into a `WorldConfig`; `open_or_new(dir, cfg)`. `app::rules()` (WMC_RULES dir, else builtin), `app::rules_dir()`. `play::reload()` (the `r` key), `Notice` (status-row message). `why::report`. Palette: `crates/app/src/render/palette.rs` has `SCENT: [Color; SCENT_CHANNELS]`. |
| Tests | `crates/app/tests/determinism.rs` | Runs the release-style binary at 1, 3 and 8 threads (`WMC_THREADS`) and compares `checksum:` lines of `run` and `show`. sim-core unit tests live in each module's `mod tests`. |
| Benches | `crates/sim-core/benches/tick.rs` | Groups `generate_many`, `stage_checksum`, `ensure_loaded`, `step/16x16 chunks`. |
| Docs | `docs/ACTORS.md`, `docs/ARCHITECTURE.md` (decisions table, 34 so far), `docs/RULES.md`, `docs/PERF.md` (one row per measured change) | Keep all four current in the same commit as the code. |

Invariants that must survive every commit:
- Determinism: bit-identical checksums on 1 and 8 threads (`WMC_THREADS`). Every new order (kind numbering, ancestor linearization, explicit starts) is a pure function of the inputs, never of a `HashMap` or directory order.
- `sim-core` depends on `bevy_ecs` + `bevy_tasks` only. No `unsafe`. No new dependencies without adding them to `[workspace.dependencies]` first.
- The `SimTick` schedule keeps ambiguity detection at error.
- `make ci` green before a commit; a `docs/PERF.md` row for anything that touches the tick, with an interleaved A/B (old binary, new binary, three pairs) because absolute numbers on this machine drift with load.

## 2. Gates: how to prove a commit changed only what it meant to

Before starting 8a, record the **baseline** at `b2b0ac4` and write it here:

```bash
cargo build --release
WMC_THREADS=8 ./target/release/wmc run /tmp/wmc-base 43200 1024 1024 12   # 2 game days
WMC_THREADS=1 ./target/release/wmc run /tmp/wmc-base 43200 1024 1024 12
```

Baseline, recorded 2026-09-25 at `b2b0ac4` plus the `state:` line (equal at 1 and 8 threads):

| run | `checksum:` | `state:` | population line |
|---|---|---|---|
| `run <dir> 43200 1024 1024 12` | `19cc0641245304b9` | `b4d70180c11c4739` | `332434 actors: chicken 1895, egg 0, chick 1553, fox 160, flower 8723, hive 52, bee 1111, grass 317404, seed 1064, tree 472` |
| `run <dir> 3000 256 256 12` (quick gate) | `9002ed1c26bab221` | `9be341adc187c931` | `5903 actors: chicken 140, egg 68, chick 0, fox 6, flower 277, hive 2, bee 5, grass 4823, seed 582, tree 0` |

`sim::checksum` mixes the rules hash, and 8a/8b/8c legitimately change the rules hash
(`parent` mixed in; `place` removed) without changing any state. So the first task of 8a
adds a second line to `wmc run`, `state:     <hex>` = `stage::checksum(world)` (pure state,
no rules hash), and every gate below compares **`state:`** and the population line, not
`checksum:`. Record the baseline `state:` too.

| Commit | Gate |
|---|---|
| 8a | `state:` and populations identical to baseline (no built-in kind extends anything yet, so family = exact). `step/16x16` A/B: the family range check replaces an equality; expect no change, record it. |
| 8b | identical to 8a (the default scenario carries exactly today's shares, cut in kind-id order as today). **Done**, in two local stages: with 2 scent channels, `state:` and populations identical to 8a at 1 and 8 threads (`b4d70180c11c4739`, `9be341adc187c931`); with 4 channels the scent layers hash differently, populations identical, 1 = 8 threads. **New reference for 8c:** full gate `checksum 9a55d72a7081adcb`, `state 4833c3a5f6dea6b2`; quick gate `checksum 2a242e3e8362499b`, `state c11c0f6d5a472758`; population lines unchanged from the baseline. |
| 8c | identical to 8b when the packs match. |
| 8d | `state:` moves (content rewritten). Gate: 1 vs 8 threads equal; 16-day populations on `256 256 12` comparable to `docs/PERF.md` actors-6d; `step/16x16` A/B against 8c. |
| 8e | a drawn-map scenario reproduces `a_fox_eats_a_cornered_chicken_in_its_chunk_and_across_a_border` (sim.rs) as data. |
| 8f | `make ci`; `wmc lint rules/` on the built-in content reports no warnings (fix the content if it does, those are real). |
| 8g | every `scenarios/tests/*.scenario` passes at 1 and 8 threads. |

## 3. Commit 8a: traits, `extends`, `inherit`, family matching, `only`, diagnostics

Engine and compiler only. The built-in `rules/` files do not change in 8a.

### 3.1 Grammar (delta to `ACTORS.md` §5)

```ebnf
item     := trait | kind | sub | const
trait    := "trait" NAME [ "(" NAME ("," NAME)* ")" ] [ "extends" parents ]
            "{" decl* sub* rule* state* "}"
kind     := "kind" NAME [ "extends" parents ] "{" decl* sub* rule* state* "}"
parents  := parent ("," parent)*
parent   := NAME [ "(" expr ("," expr)* ")" ]          # args: constant expressions
rule     := "when" cond "=>" body | "inherit" [ NAME ]
state    := "state" NAME "{" rule* "}"                  # `inherit` allowed inside
pred     := [ "only" ] NAME [ ":" INT ] | "water" | "soil" | "rock" | "free" | "bare"
```

New reserved words: `trait`, `inherit`, `only`. Inside a body the order is declarations,
then member subs, then reflex rules, then states; anything else is a parse error that says
so. `place` stays in the grammar until 8b.

### 3.2 Semantics

**Names.** Kinds and traits share one namespace (a trait and a kind cannot share a name).
Trait parameters are constants visible inside the trait; a parameter that shadows a global
`const` is an error. A `sub` inside a trait or kind is a *member sub*.

**Parents.** A kind lists at most one concrete kind among its direct parents; a trait lists
only traits. Unknown parent, a cycle, two concrete parents, wrong argument count for a
trait, `extends` of a trait that takes no arguments with arguments: all errors at the
`extends`.

**Linearization.** `ancestors(X)` = for each direct parent P in listed order: `ancestors(P)`
followed by P; then remove repeats keeping the first occurrence. A trait reached twice with
the *same* folded arguments is one ancestor; with *different* arguments it is an error
("trait `drinker` reached with (90min) and (6h)"). Everything below merges in linearized
order, the kind's own declarations last.

**Declaration merge.**
- Scalars (`glyph`, `color`, `cover`, `cadence`, `sight`, `fuel`, `food`, `bite`, and until
  8b `place`): the kind's own declaration wins; else the value declared by ancestors. If
  two ancestors declare different values and the kind does not declare it: error naming
  both.
- `tags`: union, first-appearance order along the linearization, then the kind's.
- `need NAME ...`: by name. Slot order = first appearance along the linearization, then
  the kind's new needs. A descendant redeclaring a need it inherits overrides it in place
  (same slot). Two ancestors declaring the same name with different (max, decays, vital),
  neither an ancestor of the other, and no redeclaration by the kind: error. Identical
  declarations merge silently. More than `NEED_SLOTS` (4) after merging: error naming the
  kind and every source.
- `mem`: union by name, same order rule (parents' first: this keeps `spawn ... with (a, b)`
  filling the same two slots in every kind of a family). More than `MEM_SLOTS` (12): error.
- Member subs: union by name; a descendant's definition overrides an ancestor's. A member
  named like a file-scope sub: error at the member.
- States: union by name; order = first appearance along the linearization, then the
  kind's new ones. See rule inheritance for what a redeclared state means.

**Rule inheritance (D6).** Each *rule list* (the reflex list, and each `state` block) is
resolved on its own:
- A list that contains no `inherit` ends with every ancestor's resolved rules for that
  list, in linearized order.
- A list that contains `inherit` contains exactly what it splices. `inherit` alone splices
  every ancestor not yet spliced in this list, in linearized order. `inherit NAME` splices
  that ancestor's resolved list. `NAME` must be an ancestor (direct or indirect); the same
  ancestor twice in one list, or an ancestor already covered by an earlier splice of one of
  its descendants ("`drinker` is already spliced through `chicken`"): errors.
- "Resolved list" of an ancestor = the list as that ancestor runs it, with its own
  `inherit`s already resolved. So `inherit chicken` from a chick gives the chick every rule
  the chicken runs; `inherit drinker` gives the trait's rules alone.
- A state block the kind does not declare is inherited whole (its resolved list), whatever
  the kind's other lists do.
- A trait's rules and member subs may name only needs, mems and states that the trait or
  its ancestors declare (plus consts, its parameters, kinds, tags, scents, file subs).
  Error at the trait otherwise. This is what guarantees a trait compiles for any kind that
  includes it.

**Family matching (D1).** Concrete kinds are numbered in **pre-order over the inheritance
forest**: roots (kinds without a concrete parent) in file-name then declaration order; a
kind's children directly after it, in file-name then declaration order. Descendants are
therefore the id range `[k, family_end[k])`. A predicate naming a kind matches the family;
`only kind` matches the kind alone; `kind:look` and `only kind:look` likewise. A predicate
naming a trait is an error ("`drinker` is a trait: match a tag instead"). `spawn` and
`become` name a concrete kind exactly and refuse a trait. The `kind` sense stays the own
id.

**Member subs.** Compiled once per concrete kind that ends up with them (a template), with
that kind's need, mem and state tables. Inside a member sub `next`, `take`, `give` are
allowed. File-scope subs are unchanged: shared bytecode, parameters only.

### 3.3 Code changes

`rules/compile.rs`
- `Items` gains `traits: Vec<TraitAst>`. `TraitAst` = `KindAst` fields minus glyph/color/
  place plus `params: Vec<String>`; give both a shared `BodyAst { decls..., members:
  Vec<SubAst>, rules, states, parents: Vec<ParentRef { name, args: Vec<Expr>, at }> }`.
- `Rule` becomes an enum: `When { at, arrow_line, cond, body }` | `Inherit { name:
  Option<String>, at }`.
- `Pred::Kind`/`KindLook` gain an `only: bool`.
- Parser: `trait`, `extends`, `inherit`, `only`, member `sub` inside bodies, parent args.
- A resolution pass before codegen (new `struct Resolved` per concrete kind): linearize,
  merge declarations, resolve every rule list into `Vec<ResolvedRule { cond, body, origin:
  Option<String> }>`, collect member subs after override, check the trait-scoping rule.
  Instantiate a trait's parameters by substituting folded constants (extend `Gen::
  const_value` with a per-instantiation scope).
- Numbering: build the forest, pre-order, fill `KindDef.parent` and `Kinds.family_end`.
- Codegen per kind: resolved reflex list, states, then its member sub instances (append to
  the sub table; a `Call` in that kind's code uses the instance index). `Gen.kind` already
  scopes need/mem lookups to the current kind; member subs compile inside that scope.
- `RuleInfo` gains `via: Option<String>` (the ancestor the rule came from) so `wmc why`
  can print `via drinker`. `DebugInfo` gains `traits: Vec<String>` and `parents: Vec<Vec<
  String>>` (per kind, direct parents with args as written) for `wmc lint`.
- Diagnostics: `pub struct Diagnostic { level: Level::{Warning, Note}, file, line, col,
  msg }` with `Display` as `file:line:col: warning: msg`; `DebugInfo.diagnostics: Vec<
  Diagnostic>`. 8a produces: (error) an action or `next` in straight-line code after a
  statement that always ends the think; (warning) a rule after one whose condition folds
  to nonzero and whose body always ends the think; (note) an ancestor whose reflex rules
  are neither spliced nor appended anywhere in the kind. "Always ends the think" is
  conservative: a top-level action or `next`; an `if` with `else` whose both branches do;
  a `choose` whose every arm does and whose weights fold to positive constants; a call to
  a sub that does (recursion = no). Loops never count.

`rules/mod.rs`
- `KindDef.parent: Option<u16>`; `Kinds.family_end: Vec<u16>`; `hash_all` mixes `parent`
  when `Some` (bit 45 marker), so flat rule sets keep their hash.
- `pred::ONLY: i32 = 0x0200_0000` (a flag OR'd onto a kind or `kind:look` value).

`rules/vm.rs`
- `Halo` gains `family_end: &'a [u16]`; `halo_of` (actors/systems.rs) passes `&kinds.
  family_end`; the two test halos in compile.rs/sim.rs tests pass a slice too.
- `matches()`: strip `ONLY` into an `exact` flag; a kind test is `exact ? kind == k :
  (kind >= k && kind < family_end[k])`, for both plain kinds and `kind:look`.

`app/src/main.rs`: `wmc lint` prints traits, each kind's parents, the family ranges, then
the diagnostics and a summary line. `wmc run` prints the new `state:` line (§2).
`app/src/why.rs`: rule lines print `via NAME` for inherited rules.

`reload.rs`: nothing; it maps by name. `DebugInfo.states` stays per concrete kind.

### 3.4 Tests

compile.rs: `traits_merge_declarations_in_linearized_order`, `inherit_splices_where_written_
and_appends_when_absent`, `state_blocks_merge_by_name`, `trait_parameters_fold_per_
instantiation`, `member_subs_see_their_kinds_needs_and_compile_per_kind`, `family_
numbering_is_preorder_by_file_then_declaration`, `extends_errors_name_the_problem` (every
error in §3.2), `straight_line_second_action_is_an_error`, `unreachable_rule_warning_is_
conservative`. vm.rs: `family_and_only_matching`. sim.rs: `a_chick_built_with_extends_
walks_the_same_path_as_the_flat_one` (two rule sets, actors placed by `place_actor` with
the same uids in a bare world, identical row tables after a game day). `why.rs`: an
inherited rule prints `via`.

### 3.5 Docs

`ACTORS.md` §5 grammar and semantics (a "Traits and inheritance" bullet list mirroring
§3.2), §2 the numbering rule, §11 step 8a. `RULES.md`: new sections "Traits", "Extending a
kind", "Family matching and `only`", member subs under "Subs". `ARCHITECTURE.md` decision
35: family = contiguous id range by pre-order numbering; why single concrete inheritance.
`PERF.md`: the `step/16x16` A/B row.

## 4. Commit 8b: the scenario file; `place` leaves the rules

### 4.1 Format

A scenario is a text file, own parser (`crates/sim-core/src/scenario.rs`), one statement
per line, `#` comments. Not the rules lexer: it needs decimals.

```
seed 12
size 256 256                              # cells; rounded up to whole chunks as today
terrain water_level 0.18 rock_on_soil 0.02   # any GenParams field by name; others default
start chicken 1 / 400                     # this share of walkable cells, by kind name
start grass   1 / 20
start hive at (77, 103)                   # exactly there
```

Defaults: `seed 42`, `size 80 24`, `GenParams::default()`, no starts. Errors at parse: an
unknown statement or terrain field, a share `<= 0` or `> 1`, two `start K a / b` lines for
one kind, shares summing above one. `start K at` may repeat.

Types: `Scenario { seed, width, height, params, starts: Vec<Start> }`, `enum Start {
Share { kind: String, num: u32, den: u32 }, At { kind: String, x: i32, y: i32 } }`,
`Scenario::parse(name, text)`, `Scenario::DEFAULT: &str = include_str!("../../../
scenarios/default.scenario")`. The default carries exactly today's `place` values (verify
with `grep -n place rules/*.rules` before removing them): chicken 1/400, fox 1/5000,
flower 1/250, hive 1/20000, grass 1/20, seed 1/100.

### 4.2 Resolution and worldgen

`Placement` (resolved against a `Kinds` at create/open): `bounds: Vec<(u32, u16)>` cumulative
upper bounds in **kind-id order** out of `PLACE_ONE` (the cut order today, so the built-in
world is unchanged), and `explicit: Vec<(ChunkCoord, u16 cell, u16 kind)>` sorted by
(chunk key, cell). Resolution errors (refuse the open): a start naming a kind the rules do
not define (list them), a start naming a trait, an explicit start on a cell that
`gen_cell` says is not walkable ("start hive at (77, 103) is on water"). Make `gen_cell`
`pub` for this and for lint.

`SimConfig` gains `placement: Placement` and `scenario_hash: u64` (for the header) and
loses `Copy` (keep `Clone`; fix the two `let (seed, params) = ...` copies in sim.rs).
`generate_chunk(seed, params, kinds, placement, coord, out)`: explicit starts inside the
chunk first (push standing or cover per `kinds.def(kind).cover`), then the share draw per
walkable cell as today, skipping cells an explicit start took. `Kinds` loses `place`,
`placement`, `placed()`, `places_any()`, `without_placement()`, `PLACE_ONE` moves to
`scenario.rs`; `KindDef.place` and its hash mix go; the `ORACLE_PLANTS` text and its
hand-built defs drop `place`. Tests that used `without_placement()` use a scenario with no
starts (`Scenario::empty()`).

The world checksum does **not** mix the scenario: rows already carry placement, and the
`state:` gate must hold across 8b.

### 4.3 Store v9

Bump `FORMAT_VERSION` to 9 and lay out everything this step needs, so 8c and 8e bump
nothing:
- `WorldMeta.kinds` becomes `Vec<SavedKind { name, cover: bool, needs: Vec<String>, mems:
  Vec<String>, states: Vec<String> }>` (what 8c needs to remap by name).
- `WorldMeta.starts: Vec<Start>` (by kind name; regenerating a clean chunk needs them at
  every open), `WorldMeta.scenario_hash: u64`.
- `WorldMeta.packs: Vec<String>` (8c; written empty in 8b).
- `WorldMeta.map: Option<DrawnMap>` (8e; `None` in 8b): `width, height, cells: Vec<u8>` with
  one byte per cell packing ground and feature.
- `SCENT_CHANNELS` 2 → 4 (`stage/mod.rs`); `palette::SCENT` gets four colours; scent
  tests that assert `[0, 0]` become `[0; SCENT_CHANNELS]`.
Old saves are refused as always; move `saves/dev` aside (`saves/dev-format8-old`).

### 4.4 CLI

`--scenario <file>` accepted by `show`, `play`, `run`, `why` (parse flags out of `args`
before the positional ones). `[w h seed]` keep their meaning as overrides of the scenario's
`size` and `seed`, so `make run ARGS="run saves/x 1000 256 256 12"` and the determinism
tests keep working. `wmc lint <rules> [--scenario <file>]` resolves the scenario against the
rules and reports the §4.2 errors without creating a world. New `scenarios/` directory
holds `default.scenario` and later `tests/`.

### 4.5 Tests, docs, gate

scenario.rs: parser round trips, every parse error. sim.rs: `explicit_starts_are_placed_
and_a_non_walkable_one_is_refused`, `unknown_start_kinds_are_refused`, `the_default_
scenario_reproduces_the_old_shares` (state checksum equal to the recorded 8a value for
`run 43200 1024 1024 12`). store.rs: v9 round trip, v8 refused. Docs: `ACTORS.md` (§5 drops
`place`; §2 worldgen paragraph; §11 8b), `RULES.md` (remove `place`; new "Scenarios"
section with the format), `ARCHITECTURE.md` decision 36 (placement belongs to the
scenario; amends 31) and the store v9 note, `CLAUDE.md` (layout: `scenarios/`; commands:
`--scenario`). Gate per §2.

### 4.6 As built (deviations from the text above)

- `WorldConfig` is gone: `Scenario` is the world config (`sim::create(world, &Scenario) ->
  Result<(), String>`, `new_world_with(&Scenario, Kinds) -> Result<World, String>`,
  `new_world` panics on a scenario that does not fit the built-in rules). `Scenario::BUILTIN`
  / `Scenario::builtin()` instead of `DEFAULT`; `Scenario::default()` is the empty scenario.
- Shares are cut **in the order written**, not in kind-id order: the default scenario lists
  them in kind order (so the built-in world is unchanged), and a world's starting map no
  longer depends on how a pack numbers its kinds (8c ids move, 8d renumbers).
- `Placement` carries `Placed { kind, cover }`, so `generate_chunk(seed, params, placement,
  coord, out)` takes no `Kinds`. Explicit starts are merged into the cell walk: rows stay in
  cell order.
- No `scenario_hash`: the header stores the scenario itself (seed, size, params, starts).
  `WorldMeta` also got `scents: Vec<String>` (8c remaps scent layers by name), and
  `DrawnMap` has `outside: Option<u8>` (8e's `outside`; `None` = noise).
- Hot reload re-resolves the starts against the new rules (ids may move) and drops the
  starts of kinds that are gone (`scenario::present`).
- `scent_decay` fades only the channels that hold scent (bit-identical; `fade(0) == 0`).
- `place` in a rules file is a compile error pointing to the scenario.

## 5. Commit 8c: packs, open by name

### 5.1 Open by name (D4)

`sim::open`: if the saved kind list equals the loaded one by name, need names, mem names,
state names and cover flag, proceed as today. Otherwise build a plan from the saved
`SavedKind`s to the loaded `Kinds`. Any saved kind absent from the rules: refuse with
`save has rows of kinds [hive, bee] that the loaded rules do not define`. A kind whose
`cover` flag differs: refuse. Refactor `reload::plan` to take the old side as `&[SavedKind]`
(a `Kinds` converts to that view trivially); the plan needs no old maxima, only the new.

The plan is kept in a new resource `PendingRemap(Option<Plan>)`, applied in `load_chunks`
to every chunk read from disk before `validate`, and on `save()` used to rewrite every
saved chunk file that is not loaded (factor `reload::rewrite_unloaded(store, plan,
loaded)` out of `reload_rules`), after which the meta is written with the new kinds and
the pending plan is cleared. Opening never writes; `run` stays side-effect free.

### 5.2 Packs (D5)

`compile_dirs(dirs: &[&Path])`: within a directory files sort by name; directories keep the
given order; one global namespace. `WMC_RULES` accepts several paths separated by `:`;
`--rules <dir>` is accepted, repeatable, by `show`, `play`, `run`, `why`, `lint`. Duplicate
kind, trait, sub or const names name both positions (`fox declared at animals.rules:107,
already at wild.rules:3`).

A save remembers its packs: `WorldMeta.packs` = the pack paths as given, made absolute, set
at create and refreshed on every save. With no `--rules` and no `WMC_RULES`, `play`, `run`
and `why` use the saved packs if every path still exists, else the built-in rules, and say
which on stderr. A saved-pack rules hash different from the current compile is reported
(one line), not refused: rules may be tuned between sessions.

### 5.3 Tests, docs, gate

sim.rs: `a_save_opens_with_a_superset_pack_and_keeps_every_actor` (pack A, then A+B with a
new kind and B's file sorting first, so ids move; positions, uids, needs identical),
`a_save_missing_a_kind_is_refused_with_the_list`, `a_remapped_save_is_rewritten_on_save_
and_reopens_with_the_new_pack_only`. compile.rs: `packs_merge_in_order_and_duplicates_
name_both_files`. Docs: `ACTORS.md` §2 save paragraph and §11 8c; `ARCHITECTURE.md`
decision 34 amended (open by name, rewrite on save); `RULES.md` "Packs"; `CLAUDE.md`
commands. Gate per §2.

## 6. Commit 8d: the built-in content on traits; the vocabulary

`rules/lib.rules` (new; compiles before `plants.rules`, after `grass.rules`; names resolve
after all files are parsed, so order is only about kind numbering):
- `trait walker { mem heading, detour  when blocked => ...  when detour > 0 => ...  sub
  wander() { heading = turn(heading)  move dir(heading) } }`
- `trait drinker(thirsty, desperate) { need water ...  mem knows_water, water_x, water_y
  ... }` with today's chicken rules parameterized (chicken 90min/30min, fox 6h/6h, chick
  as chicken).
- `trait rooted(reach, sip, full) { need water max full vital  when count water within
  reach > 0 => water = min(water + sip, full) }` for seed (2, 6h, 1d), tree (2, 12h, 3d),
  grass (8, 2d, 2d), flower (6, 1d, 1d). Note `need water max full` takes a parameter:
  declarations may use trait parameters where an `INT | TIME` is expected.
- `trait mortal(after, odds) { when age > after and rand(100000) < odds => die }` for
  chicken, fox, flower, bee.
- The file subs `turn`, `flee`, `peck`, `forage` move here from `animals.rules`.

`animals.rules`: `kind chicken extends drinker(90min, 30min), walker, mortal(5d, 6)` with
explicit `inherit` placement matching today's order; `kind chick extends chicken` with its
own glyph, color, food, health, cadence, `when age > 2d => become chicken`, its follow rule
as `nearest only chicken within 6 as m`, `inherit drinker`, `inherit walker`, and its own
wander; the fox's litter rule keeps `count chicken within 16` (chicks now count). `bees.
rules`: bee extends `walker`; flower extends `rooted(6, 1d, 1d)`, `mortal(4d, 3)`.
`grass.rules`, `plants.rules`: `rooted`. Keep every threshold as it is today; the point is
identical behaviour from shared code, then `only` where family matching would change it.

`builtin.rs`: `FILES` gains lib.rules; constants renumbered by pre-order: CHICKEN=0,
CHICK=1, EGG=2, FOX=3, FLOWER=4, HIVE=5, BEE=6, GRASS=7, SEED=8, TREE=9 (chick moves under
chicken). `rules/mod.rs` builtin table test updated.

`RULES.md` gains "The vocabulary": tags (`animal`, `plant`, `meat`, `feed`), need names
the engine reads (`health`, `water`, `food`) and the ones content shares (`nectar`), looks
(`flower:1` rich, `bee:2` dancing, hive `look` 1 = stores, 2 = full), scents (`trail`), and
the trait library with each parameter's meaning. `ACTORS.md` §11 8d, `PERF.md` rows
(`run 43200 1024 1024 12` at 1 and 8 threads, `step/16x16` A/B). Gate per §2; also run
`wmc why` on a chick and confirm `via drinker` lines.

## 7. Commit 8e: drawn maps in scenarios

```
map {
  ~~~~....####....
  ~~~.....C...F...
  ....'''''.......
}
legend { ~ water  . soil  # rock  ' grass  C chicken  F fox }
outside noise                             # or: soil | rock | water; default noise
```

- Rows are the map from `(0, 0)` at the top-left; every row must have the same length.
  `size` may be omitted (then it is the map's size); if given it must match. A legend
  character is one printable ASCII byte, unique; it names a terrain (`water`, `soil`,
  `rock`, meaning rock on soil) or a kind. A kind character puts soil under it and adds a
  `start K at (x, y)`; a cover kind's character puts that cover on soil.
- Stored resolved in `WorldMeta.map` (terrain bytes) and `WorldMeta.starts` (the kinds),
  so a clean chunk regenerates identically. Chunks intersecting the rectangle take their
  terrain from the map; outside it, `outside` decides: `noise` = `gen_cell` from the seed
  as today, or a constant.
- `wmc lint --scenario` and `show` accept it; `show` of a small drawn map prints it back.
- Tests: parser (ragged rows, unknown char, duplicate char, size mismatch), worldgen
  (map cells and starts land; outside fill), and the gate scenario: a walled pen with two
  chickens and a fox reproducing the sim.rs cornered-chicken test as `scenarios/tests/
  fox_pen.scenario` (asserted in 8g; in 8e a Rust test loads it and checks the pen).
- Docs: `RULES.md` scenarios section; `ACTORS.md` §11 8e; `ARCHITECTURE.md` a note on
  decision 13 (worldgen stays a pure function of the inputs; the map is one of them).

## 8. Commit 8f: the author lint

A pass in `rules/lint.rs` (sim-core) over the parsed AST plus the compiled `Kinds`,
returning `Vec<Diagnostic>`, run by `wmc lint`, printed once on stderr when a world opens
or is created, and after hot reload as `reloaded ... | 3 warnings (log)` on the status row
with the text in the log. `wmc lint --strict` exits 1 on any warning (for CI).

| Check | Level | Message shape |
|---|---|---|
| A predicate tag that no loaded kind carries | warning | `wolf looks for meat, but no kind in this rule set is tagged meat` |
| `sniff` or `scent()` of a channel nobody `mark`s; `signal_of` when no rule sets `signal`; `K:n` when no rule of K's family assigns `look` | warning | |
| `eat`/`hit`/`graze` whose target predicate is known statically and some kind matching it has no `health`; an eater with no `food` need; `drink` in a kind with no `water` need | warning | `fox eats meat; egg is meat but has no health need` |
| A search radius that folds to a constant above the kind's `sight` | warning | `radius 10 exceeds sight 8: clamped` |
| A decaying vital need whose max is below the kind's cadence | warning | `need water (max 30min) empties before the first think (cadence 900)` |
| An action inside `for each` | warning | `an action inside for each traps when the loop finds a second cell` |
| Never used: a mem; a need the engine does not read (`health`, `water`, `food` are read); a file or member sub; a const; a state no `next` reaches (except the first); a tag no predicate names | warning; the tag case a note | |
| Scenario: a kind that never appears (no start, no `spawn`, no `become` of it) | warning | needs `--scenario`; silent without one |

"Target predicate known statically": the binding of an `eat s` is followed back to the
`nearest P within r as s` that bound `s` in the same rule body, or, for a sub, to the
`pred` argument at each call site (each call site is checked separately). Nothing is
reported when the target cannot be traced. Every check is structural; none names the
built-in vocabulary.

Tests: one fixture per check, positive and negative, in `lint.rs`; `wmc lint rules/` on the
built-in content is clean. Docs: `RULES.md` "Debugging" lists the checks; `CLAUDE.md`
mentions `--strict`.

## 9. Commit 8g: scenario tests

Scenario files gain two statements, after the world description:

```
run 2d                        # a time or a tick count
expect count chicken == 0     # alive now
expect eaten chicken >= 2     # from the tally: born | became | eaten | died
expect at (77, 103) hive      # the standing actor there, else the cover; `nobody` allowed
expect checksum 8e1fd4fd7f84a868
```

Operators `== != < <= > >=`. `run` and `expect` may alternate. `wmc scenario <file>
[--rules ...] [--threads N]` creates the world from the file (no save directory), runs the
statements in order, prints every failed expectation with the actual value, exits 1 if
any failed. `make test` gains a `scenario-test` target running every `scenarios/tests/
*.scenario` at `WMC_THREADS=1` and `8`, so each is a determinism check too.

Move to text the sim.rs tests that are content checks, keeping their Rust twins only where
they test engine mechanics: `fox_pen` (8e), `chickens_graze_grass`, `eggs_hatch`,
`bees_find_flowers` (a drawn hive and patch). Keep in Rust: bites by share, take/give,
migrate by key, reload, explain, counters, the invariants.

Docs: `RULES.md` "Testing a kind"; `CLAUDE.md` commands; `ACTORS.md` §11 8g and the step-8
closing paragraph. Then delete this file.

## 10. Out of scope for step 8

- Removing an inherited rule (labelled rules and `drop`). The escape is a more specific
  rule before the inherited block, or not inheriting that ancestor.
- Recording hot reload as a replayable input.
- Cross-chunk cell systems (scent diffusion); still an open question in `ARCHITECTURE.md`.
- Widening `NEED_SLOTS`, `MEM_SLOTS` or the 64-tag budget. Do it if the built-in content
  needs it in 8d, as its own small commit with a store bump.

## 11. Working notes for whoever picks this up

- The formatter reflows Rust and the pre-commit hook runs `make check`; after `cargo fmt`
  re-grep before editing by string.
- `make ci` is the definition of done. Paste failures verbatim if you cannot fix them.
- Benchmarks on this machine drift with load: only interleaved A/B pairs of two binaries
  are meaningful; record the load average with the numbers.
- `WMC_THREADS=1` and default must agree on `state:` and `checksum:` for every `wmc run`.
- The window opened by `wmc play` must be closed after a capture, and never brought to the
  front by script: it steals the owner's keystrokes. Verify status text through the log
  and unit tests instead.
- The owner's `saves/dev` is disposable: set it aside as `saves/dev-format<N>-old` when
  the format changes and say so in the report.

## 12. Checklist

- [x] baseline recorded in §2
- [x] 8a traits, extends, inherit, family, only, member subs, diagnostics (also: `vm::Want`, a predicate decoded once per search, -12% on the tick)
- [x] 8b scenario file, place removed, store v9, four scent channels (see §4.6)
- [ ] 8c packs, open by name, rewrite on save
- [ ] 8d content on traits, vocabulary in RULES.md
- [ ] 8e drawn maps
- [ ] 8f author lint, --strict, reload surfacing
- [ ] 8g scenario tests, make test runs them, content tests moved
- [ ] ACTORS.md §11 step 8 written from this plan; ARCHITECTURE decisions 35, 36 (and 34 amended); this file deleted
