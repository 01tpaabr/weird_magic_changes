# weird_magic_changes

A deterministic, massively parallel world simulation in Rust on
[Bevy](https://bevyengine.org) 0.19. Everything that lives in the world is content:
creatures are written in a small rules language, worlds in scenario files, and both are
loaded as input. The aim is a base on which other people write their own kinds, and on
which quite different simulations can run on the same engine.

## Status

The build order in `docs/ACTORS.md` §11 is complete (steps 1 to 8). What works today:

- **The world.** An unbounded grid of 64 x 64-cell chunks: soil, water, rock, ground
  cover and four scent layers. It is generated deterministically and streamed around the
  camera, and a save writes only the chunks that changed.
- **Time.** Integer ticks, 8 a second at 1x (a game day is 21,600 ticks), day and night,
  pause, single step, and 1x to 16x speed.
- **Actors.** Rows inside their chunk, each thinking on its own cadence. A tick is a fixed
  sequence of phases (simulate, think, resolve, exchange, apply, migrate, compact) that run in
  parallel across chunks and give **bit-identical results on 1 thread or 64**. The test
  suite checks this on every run.
- **The rules language.** Kinds with needs, memory and states, `when condition => action`
  rules, subs, and traits with inheritance, compiled to a fuel-bounded bytecode VM.
- **A built-in ecosystem.** Hens graze, drink, lay eggs and flee foxes. Chicks follow the
  hens and grow up. Foxes hunt, sleep and raise kits. Flowers make nectar, hives raise
  bees, and bees find flowers, dance the way home and lay scent trails. Grass spreads and
  regrows, seeds grow into trees, and trees drop seeds.
- **Content as input.** Rule packs (`--rules`), scenario files (seed, size, terrain or a
  hand-drawn map, who starts where), and saves that reopen under other packs, with kinds
  matched by name.
- **Author tooling.** `wmc lint` (with `--strict` for CI), `wmc why` (one actor's next
  think, explained rule by rule), hot reload (`r` in the game), and scenario tests
  (`wmc scenario`).

**Performance.** A 1024 x 1024-cell world (256 chunks) holding about 333,000 actors, most of
them grass, steps in about 0.9 ms per tick on 8 threads, or 2.4 ms on one, on an 8-core
Apple Silicon machine (`docs/PERF.md`).

**Not there yet.** You can watch, pause, speed up, save and reload rules, but players
cannot act on the world yet: input events and a replay log are open questions. Cell
systems (scent fading) work chunk by chunk, and anything that spreads across chunk
borders, such as diffusing smell, is still an open question. A hot reload is not
recorded, so a reloaded world cannot be replayed from its seed. The renderer draws one
glyph per cell.

## Quick start

A recent stable Rust (edition 2024; `rust-toolchain.toml` picks the channel). The first
Bevy build takes a few minutes.

```
make setup     # Rust components and git hooks, then make ci
make run ARGS="show 80 24 42"                  # print a new world once
make run ARGS="play saves/dev"                 # the window: create or reopen a world
make run ARGS="run saves/dev 1000"             # headless: N ticks, µs per tick, checksums
make run ARGS="why saves/dev 77 103 3000"      # after 3000 ticks, why the actor at (77, 103) does what it does
make run ARGS="lint rules/"                    # compile the rules, print the kind table and the author lint
make run ARGS="scenario scenarios/tests/fox_pen.scenario"   # a scenario test
make ci        # fmt, clippy, and every test (determinism at 1 and 8 threads included)
```

`play` and `run` open the world saved in the directory, or create one from the built-in
`scenarios/default.scenario`. `[width height seed]` after the directory, or
`--scenario <file>`, choose another. `WMC_THREADS=1` (or `--threads 1`) and the default
must print the same checksum.

In the window: `w a s d` or the arrows move (Shift for 4x), `space` pauses, `.` steps one
tick, `[` and `]` change the speed, `+` and `-` zoom, `p` saves, `r` reloads the rules,
and `q` saves and quits.

## Writing your own kinds

A pack is a directory of `*.rules` files. This one adds sheep, built on the traits every
pack can use (`drinker`, `mortal`, `wander()`, `forage`, `flee`, from `rules/lib.rules`):

```
# my_pack/sheep.rules
kind sheep extends drinker(6, 2h, 3), mortal(8d, 5) {
  glyph "S"
  color "#e8e8e8"
  tags animal meat                  # foxes hunt `meat`
  sight 6
  food 1d                           # what a fox gains from one
  need food   max 1d vital
  need health max 30 decay 0 vital

  inherit mortal                    # old age
  when nearest fox within 6 as f => flee(f)
  inherit drinker                   # remembers water, walks round rocks, drinks
  when food < 20h => forage(grass, 6)
  when true       => wander()
}
```

A scenario says where they start, and can end in a test:

```
# meadow.scenario
seed 7
size 128 128
start grass 1 / 10
start sheep 1 / 300
start fox   1 / 20000

run 2d
expect count sheep >= 30          # the flock drinks, grazes and mostly survives
expect min food of sheep > 12h    # nobody goes hungry
```

```
make run ARGS="lint rules my_pack"
make run ARGS="scenario meadow.scenario --rules rules --rules my_pack"
make run ARGS="play saves/meadow --rules rules --rules my_pack --scenario meadow.scenario"
```

`--rules rules` is the built-in content as a pack; packs compile together as one rule set,
so the sheep can use the built-in fox and grass. A save remembers its packs. The full
language, scenarios, packs, tests and the lint are in `docs/RULES.md`.

## Layout

```
crates/sim-core    the simulation, on bevy_ecs and bevy_tasks (no renderer, no clock):
                   chunks, actors, the rules VM and compiler, scenarios, worldgen, saves, time
crates/app         the `wmc` binary: the window, camera, clock and renderer, and the
                   show / play / run / why / lint / scenario commands
rules/             the built-in kinds (compiled into the binary) and the shared trait library
scenarios/         the default world; tests/ holds the scenario tests
docs/              the documentation below
```

## Documentation

- `docs/RULES.md`: the author's reference. How to write kinds, traits, scenarios, packs
  and tests, plus the shared vocabulary and the lint.
- `docs/ACTORS.md`: the actor and rules design, and the build order with what each step did.
- `docs/ARCHITECTURE.md`: the decisions, with why and when to revisit each.
- `docs/PERF.md`: measured baselines, before and after every change to the hot path.
- `CLAUDE.md`: the working rules for anyone changing the code, human or agent:
  determinism first, data layout, parallelism by structure, measure before optimizing.
