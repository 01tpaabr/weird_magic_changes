# Sprites: pictures made of letters (design and build plan)

Status: **proposed, not built** (2026-11-12). The owner reads this first; an agent builds it
step by step once it is approved. Decisions the owner still has to make are in §9.

## 1. Goal

A kind can be drawn as a **sprite**: a small picture composed in code from stamps, each a
character placed, turned, scaled and coloured. No image files ship with a pack: a sprite is
text, written by hand or by a tool, and drawn by the app at whatever size a cell has.

Four things, in this order of importance:

1. a sprite is **a text file** in a pack;
2. it can be **animated**;
3. it has **several colours**;
4. an agent can take **an image** and fit a sprite to it, with tools that render, score and
   draft, and a skill that says how to use them.

The rule for every choice below is **readability over likeness** (owner, 2026-11-12): a
sprite of a dozen stamps on a 24-pixel cell can't copy an image; it must read as the thing.

## 2. What stays as it is

- **The sim never sees a sprite.** Everything here is in `crates/app` (priority 6). No
  change to `sim-core`, the rules language, the rules hash or the save format.
- **`glyph` and `color` stay.** `wmc show`, the inspector, tests and cells too small for a
  sprite keep using them. A pack without sprite files draws exactly as today.
- **Locality, determinism of the world:** untouched; a sprite reads only what the renderer
  already reads (the kind on a cell, and its public `look`).

## 3. The sprite file

`*.sprite` files sit beside the `*.rules` files of a pack (the built-in kinds': `rules/`,
embedded in the app as the rules are in the sim). The app reads them from the packs a world
was made with (`Kinds::debug.packs`). A sprite names the kind it draws; nothing in the rules
changes.

```sprite
# packs/shore/shore.sprite
palette {
  shell "#d9623b"
  eye   "#101010"
}

sprite crab {
  frames 4 every 200ms            # 4 frames, each shown 200 ms
  "c" at 0.30 0.55 turn 0   ~turn 20      # a claw that swings 20 degrees each way
  "c" at 0.70 0.55 turn 180 ~turn 20 phase 180
  "O" at 0.50 0.50 scale 0.8 color shell
  "." at 0.42 0.38 scale 0.5 color eye
  "." at 0.58 0.38 scale 0.5 color eye

  pose 1 {                        # drawn while the crab's `look` is 1
    "O" at 0.50 0.60 scale 0.6 color shell
  }
}
```

- **A stamp** is `"C" at X Y [turn DEG] [scale S] [color NAME|"#rrggbb"]`.
  - `C` is one printable ASCII character (the atlas's set).
  - `X Y` is the stamp's centre, in cells from the sprite's top-left: `0.5 0.5` is the middle
    of a one-cell sprite.
  - `turn` is whole degrees clockwise (default 0); `scale` is the character's height in
    cells (default 1.0, 0.1 to 4.0).
  - `color` is a palette name or a literal; without one, the kind's `color`.
- **Order is depth:** stamps are drawn in file order, later over earlier.
- **Animation:** `frames N every T` (N 1 to 16; T as times are written in rules, at least
  50 ms). A stamp value may swing: `~turn A`, `~x A`, `~y A`, `~scale A` add
  `A * sin(2 pi (f / N) + phase)` at frame `f`, with `phase DEG` shared by a stamp's
  swings. This is where the sine earns its place: one line gives a walk cycle. Frames are
  worked out when the file loads, not per rendered frame.
- **Poses:** `pose V { ... }` (V 0 to 255) is the stamp list drawn while the actor's `look`
  is V; the sprite's own list is the pose for every other value. `look` is already set by
  rules and read by nobody's eyes: a rabbit that flees sets `look = 1` and changes pose
  with no new sim code. A pose may carry its own `frames` line.
- **Big actors:** for a kind declared `size W H`, coordinates run 0 to W and 0 to H; the
  sprite is drawn across the footprint (one picture over W x H cells).
- **Which kind:** `sprite NAME` is the kind NAME; a kind without one uses its nearest
  parent's (`KindDef::parent`), so `quick_rabbit` wears `rabbit`'s. In a player's pack the
  name is the player's kind, as the compiler prefixes it; a namespaced kind is written
  `weather.cloud`.
- **Errors** are `file:line:col: message`, as the rules compiler's. A sprite for a kind no
  loaded pack has is a warning, not an error (packs load in many combinations).
- **Limits** (checked at load, with the count in the message): 32 stamps a pose, 16 frames,
  8 poses a sprite, and a budget of tiles a rule set (§4).

The grammar goes in this file (§10, written with step 1), with a generator for the fuzzer,
as `GRAMMAR.md` and `gen_rules.rs` are for rules.

## 4. Drawing it

Today (`render/`): `atlas` rasterizes every ASCII character once per cell size into a
tileset (an array texture: RGBA, white, coverage in alpha); `cells` fills a `CellFrame`
(one glyph byte and two colours a cell); `grid` uploads it as two `TilemapChunk`s, the
backgrounds under the glyphs, each tile tinted.

- **Rasterizing a sprite** (`sprite/raster.rs`, pure, no Bevy): for each pose and frame,
  draw the stamps into an RGBA box of `W*cell x H*cell` pixels: rasterize the character
  with `fontdue` at `scale * cell` (supersampled 2x), map it through the turn, and blend it
  over what is there (straight alpha). Angles are whole degrees through an integer sine
  table, so a picture is the same on every machine, as the bundled font is for glyphs.
  A big sprite is then cut into its W x H cell tiles.
- **A second tileset** holds the sprite tiles (colour in RGB, coverage in alpha), rebuilt
  with the glyph atlas when the cell size changes. The glyph atlas and its tests stay as
  they are.
- **A third `TilemapChunk`** per map layer (above the glyphs) draws sprite tiles, tinted
  white scaled by daylight, so day and night dim a sprite as they dim a glyph.
  `CellFrame` gets `sprite: Vec<u16>` (0: none); a cell with a sprite has a blank glyph.
- **Choosing the tile** (`cells::render_row`): a table by kind (`Looks::sprites`) says
  whether a kind has a sprite, so kinds without one pay one load and a branch.
  With one: the tile is `base + pose * frames + frame`, plus the part's offset for a big
  actor (`ActorId::part_of` gives `dx, dy` with no lookup).
  - *Pose* needs the row's `look`, so the chunk lookup hands the renderer `ChunkActors`
    beside `ChunkCells`; a part finds its anchor's row through the lookup.
  - *Frame* is `(clock / every + offset) % frames`: the clock is passed in (a test passes
    a number); the offset is a hash of the cell, so a herd doesn't step in unison.
- **Small cells:** below `SPRITE_MIN` (16 physical pixels) a kind is drawn by its glyph,
  below `GLYPH_MIN` (8) as a square, as today.
- **Tile budget:** the sum over sprites of `poses * frames * W * H`. Array textures have a
  layer limit that depends on the device (256 is the floor wgpu guarantees). Step 3 reads
  the device's limit, and a rule set over the budget is an error that says its count.
- **Reload:** `r` in `wmc play` reads the sprite files again with the rules; a sprite error
  shows on the status bar and keeps the old sprites.
- **Cost:** `render_cells/160x90` (`crates/app/benches/render.rs`) before and after, in the
  commit and in `PERF.md`; a world without sprites must not slow by more than noise.

## 5. Tools: `wmc sprite`

All three are headless, deterministic, and share the rasterizer with the game, so what an
agent scores is what a player sees.

- **`wmc sprite render <file.sprite> --png out.png [--sprite NAME] [--pose V] [--cell PX]
  [--bg "#rrggbb"]`**: a sheet with every frame of every pose, each twice: at `--cell`
  size (24 by default, what a player sees) and enlarged 8x (what an author edits). This is
  the agent's eyes.
- **`wmc sprite score <file.sprite> --sprite NAME --target image.png [--diff out.png]`**:
  how close the first frame is to an image, as numbers an agent can compare between tries:
  - `shape`: overlap of the two silhouettes (intersection over union), at 24 px;
  - `colour`: mean colour distance inside the silhouette, at 24 px;
  - `detail`: the same two at 96 px (a tie-breaker, weighted low: readability first);
  - `stamps`: how many it took.
  The target's background is its alpha channel, else the colour of its corners (`--key`
  to name it). `--diff` writes target, sprite and difference side by side.
- **`wmc sprite fit <image.png> --kind NAME -o out.sprite [--stamps 8] [--colors 3]
  [--chars "..."] [--size W H] [--seed S]`**: a first draft from an image, by the method of
  Fogleman's Primitive with letters for shapes:
  1. reduce the target to `--colors` colours (they become the palette);
  2. add one stamp at a time: try a few hundred random stamps (character, place, turn,
     scale, palette colour), keep the best, then nudge it while the score improves;
  3. a last pass nudges every stamp again with the others in place;
  4. stop early when a new stamp no longer improves the score by a set margin: fewer
     stamps read better.
  The score it climbs is `score`'s, weighted toward `shape` at 24 px. Seeded (the same
  image and seed give the same file), and fast: 8 stamps on a 32-pixel box are a few
  hundred thousand tiny rasters. It writes a still; animation and poses are added by hand
  or by the agent.
- **`wmc lint`** also reads a pack's sprite files: parse errors, kinds that don't exist,
  the tile budget.

New dependency (owner agreed, 2026-11-12): `image`, with only its `png` and `jpeg` features,
in `[workspace.dependencies]`, used by `app` alone.

## 6. The skill: `/sprite-fitting`

`.claude/skills/sprite-fitting/SKILL.md`, on demand, as `/world-tuning` is. What it teaches
an agent that is handed an image and a kind:

1. **Prepare the target:** one subject, cropped tight, plain background, few colours. A
   busy image is simplified first (say so to the user), not fitted as it is.
2. **Draft:** `wmc sprite fit`, with 6 to 10 stamps and 2 to 4 colours.
3. **Look:** `wmc sprite render`, and read the PNG at the small size first. Name what it
   reads as. If it doesn't read as the thing at 24 px, more detail won't fix it: change the
   big shapes.
4. **Edit by hand:** move, turn, swap characters (a table of which characters make which
   shapes: `O 0 @` bodies, `( ) c` curves, `/ \ |` limbs, `. '` eyes, `^ v < >` beaks and
   ears), remove stamps that don't change the read.
5. **Score** after each change, keep the better file, stop when two rounds don't improve
   `shape` or the picture reads right.
6. **Animate and pose:** swings on the limbs, a pose per `look` the rules set.
7. **See it in the world:** a small scenario, a screenshot (the window recipe in
   `/bevy-dev`), at two zooms.

With `references/`: the stamp vocabulary above, worked examples (an image, its draft, the
edits, the result), and the lessons as they come, as `/world-tuning` keeps them.

## 7. Prior art this stands on

- [Primitive](https://github.com/fogleman/primitive) (Fogleman): add one shape at a time,
  keep a random nudge only if the error falls. The fit's method.
- [Typographic portraits](https://cdv.dei.uc.pt/wp-content/uploads/2020/06/martinsRebelo2019a.pdf)
  (University of Coimbra): letters positioned, rotated and scaled to recreate a photo.
- [Typewriter art by optimization](https://graphicsinterface.org/proceedings/gi2021/gi2021-13/)
  (Graphics Interface 2021): overlapping characters beat a plain grid of them.
- [Structure-based ASCII art](https://www.cg.tuwien.ac.at/courses/CA/material/papers/AsciiArt.pdf)
  (Xu, Zhang, Wong 2010): match a character's shape to the image's lines, not its tone.
- [See it. Say it. Sorted.](https://arxiv.org/pdf/2508.15222): an agent that renders,
  compares and revises with a small set of shapes and a palette. The skill's loop.

Read as search summaries, not in full; the build should not depend on any detail of them.

## 8. Build steps

Each step is one or more small commits on `main` with `make ci` green; `make fuzz` before
the last. An agent building this reads `CLAUDE.md`, `/bevy-dev` and this file first.

| # | Step | Done means |
|---|---|---|
| 1 | **File format:** `crates/app/src/sprite/` with the parser (palette, sprites, stamps, frames, swings, poses, limits), errors with positions, `wmc lint` reading `*.sprite`, §10 grammar, a generator and a never-panics fuzzer in `make fuzz`, ` ```sprite ` fences in this file run by a docs test | every example here parses; bad files give `file:line:col` errors; fuzz passes |
| 2 | **Rasterizer and `wmc sprite render`:** stamps to RGBA boxes, the integer sine table, frames from swings, the `image` dependency, the sheet PNG | golden tests on small boxes (exact bytes); the crab above renders and reads as a crab at 24 px (look at it) |
| 3 | **In the game, still sprites:** the sprite tileset, the third chunk, `CellFrame::sprite`, `Looks::sprites`, parent fallback, small-cell fallback, daylight, the tile budget against the device limit, reload with `r`, the menu and gallery | a pack with a sprite shows it in `wmc play` (screenshot at two zooms); a pack without one is pixel-identical to before; `render_cells` bench before/after in `PERF.md` |
| 4 | **Animation and poses:** the frame clock and per-cell offset, `look` through `ChunkActors` | a kind that sets `look` changes pose; frames advance; a test with a passed-in clock |
| 5 | **Big actors:** one sprite over a footprint, parts pick their slice | `packs/giants`' mammoth is one 3 x 3 picture, whole across a chunk border and while a neighbour chunk is away |
| 6 | **`wmc sprite score`** | the same file and image give the same numbers; a sprite scores 1.0 `shape` against its own render; CLI tests and the CLI fuzzer know the command |
| 7 | **`wmc sprite fit`** | seeded and repeatable; on three simple test images the draft's `shape` beats a single best character by a clear margin (numbers in the commit) |
| 8 | **The skill and the docs:** `/sprite-fitting`, a `RULES.md` section pointing here, an `ARCHITECTURE.md` decision, `CLAUDE.md` (layout, commands, skills), one showcase pack with sprites made by the skill from images | an agent given only the skill and an image produces a sprite the owner accepts |

Steps 1 to 3 give sprites written by hand; 4 and 5 make them move and grow; 6 to 8 are the
image-fitting tools. Each group is useful if the work stops after it.

## 9. For the owner to decide

1. **Where a kind gets its sprite.** Proposed: the sprite file names the kind (no change to
   the rules language or the sim). The other way, a `sprite NAME` line in the kind, puts
   looks in one place with `glyph` and `color` but changes the compiler and the rules hash.
2. **The animation clock.** Proposed: real time, so a paused world still breathes and a
   fast one doesn't flicker. The other way: sim ticks, so animation stops on pause and
   speeds up with the world.
3. **Only letters, or one filled shape too.** Letters are strokes, so a solid body takes
   dense characters (`@ # O 0`). Proposed: letters only at first; after step 7's numbers,
   decide whether a filled disc stamp is worth adding.
4. **Grounds, features and fields.** Proposed: not in this plan (kinds only). Water that
   ripples and grass that sways are a separate, smaller change to how a cell's colour is
   chosen.
5. **Sound.** Not in this plan.

## 10. Grammar

Written with step 1.
