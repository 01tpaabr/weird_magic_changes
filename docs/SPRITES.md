# Sprites: pictures made of letters (design and build plan)

Status: **approved** (owner, 2026-11-12: every proposed choice in §9); **built**, steps 1
to 8 (§8, 2026-11-12/13; ARCHITECTURE decision 75). Step 8's test passed: the owner
accepted `packs/pasture`'s sheep and wolf, made with the skill from drawings (2026-11-13).
Waiting on the owner: §9.3 (a filled disc stamp, now that step 7's numbers are in).
`packs/crabs` is the example pack (`scenarios/crabs.scenario`), `packs/pasture` the one
made from images with `/sprite-fitting`, `packs/giants` draws its mammoth as one 3 x 3
picture.

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
# packs/crabs/crabs.sprite
palette {
  shell "#d9623b"
  dark  "#8a2e16"
  eye   "#101010"
}

sprite crab {
  frames 4 every 200ms                                # 4 frames, each shown 200 ms
  "<" at 0.12 0.68 scale 0.75 color dark ~x 0.03      # legs that shuffle
  ">" at 0.88 0.68 scale 0.75 color dark ~x 0.03
  "Y" at 0.18 0.31 scale 0.65 turn -25 ~turn 15 color shell   # claws that swing 15 degrees
  "Y" at 0.82 0.31 scale 0.65 turn 25 ~turn 15 phase 180 color shell
  "O" at 0.50 0.66 scale 0.95 turn 90 color shell     # the shell: an O on its side,
  "=" at 0.50 0.66 scale 0.8 color shell              # filled
  "'" at 0.40 0.42 scale 0.7 color eye                # eyes on stalks
  "'" at 0.60 0.42 scale 0.7 color eye

  pose 1 {                        # drawn while the crab's `look` is 1: tucked in
    "O" at 0.50 0.62 scale 0.95 turn 90 color shell
    "=" at 0.50 0.62 scale 0.8 color shell
    "'" at 0.40 0.50 scale 0.5 color eye
    "'" at 0.60 0.50 scale 0.5 color eye
  }
}
```

- **A stamp** is `"C" at X Y [turn DEG] [scale S] [color NAME|"#rrggbb"]`.
  - `C` is one printable ASCII character (the atlas's set).
  - `X Y` is the centre of the character's ink (its glyph's box, not its line box), in
    cells from the sprite's top-left: `0.5 0.5` is the middle of a one-cell sprite, and a
    `"."` there is a dot in the middle, wherever the font's baseline is.
  - `turn` is whole degrees clockwise about that centre (default 0); `scale` is the font
    size in cells (default 1.0, 0.1 to 4.0): a capital is about 0.7 of it tall, an `O`
    about half of it wide (the font is monospaced), so a wide body is an `O` turned 90.
  - `color` is a palette name or a literal; without one, the kind's `color`.
- **Order is depth:** stamps are drawn in file order, later over earlier.
- **Animation:** `frames N every T` (N 1 to 16; T a real time, `200ms` or `1.5s`, from
  50 ms to 60 s: the rules' units are sim time, these are not). A stamp value may swing: `~turn A`, `~x A`, `~y A`, `~scale A` add
  `A * sin(2 pi (f / N) + phase)` at frame `f`, with `phase DEG` shared by a stamp's
  swings. This is where the sine earns its place: one line gives a walk cycle. Frames are
  worked out when the file loads, not per rendered frame.
- **Poses:** `pose V { ... }` (V 0 to 255) is the stamp list drawn while the actor's `look`
  is V; the sprite's own list is the pose for every other value. `look` is already set by
  rules and read by nobody's eyes: a rabbit that flees sets `look = 1` and changes pose
  with no new sim code. A pose may carry its own `frames` line.
- **Big actors:** for a kind declared `size W H`, coordinates run 0 to W and 0 to H; the
  sprite is drawn across the footprint (one picture over W x H cells). Each part shows its
  slice of the anchor's pose and frame, read from the anchor's row in the same layer, in
  its chunk or the next; while the anchor's chunk is away, the parts left show their
  slices of the own stamps from offset 0, all alike (`packs/giants/giants.sprite`, the
  mammoth).
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
  With one: the tile is `pose.first + frame * W * H`, plus the part's place for a big
  actor (`ActorId::part_of` gives `dx, dy` with no lookup). Poses may have different
  frame counts, so each pose keeps its first tile (`Looks::poses`).
  - *Pose* needs the row's `look`, so the chunk lookup hands the renderer `ChunkActors`
    beside `ChunkCells`; a part finds its anchor's row through the lookup.
  - *Frame* is `(clock / every + offset) % frames`: the clock is real time in milliseconds,
    passed in (a test passes a number); the offset is the actor's `stagger` (the low bits
    of its uid, public in its row), so a herd doesn't step in unison and an animal keeps
    its frame as it walks (a hash of the cell, as first planned, would jump a frame at
    every step). Without its row (the anchor's chunk not loaded), the offset is 0 and the
    pose its own stamps.
- **Small cells:** below `SPRITE_MIN` (16 physical pixels) a kind is drawn by its glyph,
  below `GLYPH_MIN` (8) as a square, as today.
- **Who shows:** the kind whose glyph would show (who is on top of a cell, else its ground
  cover) shows its picture instead, over the cell's background. A child that wears its
  parent's sprite wears the parent's picture, colours and all (the tiles are drawn once a
  sprite); give it a sprite of its own to tell them apart.
- **Tile budget:** the sum over sprites of `poses * frames * W * H`. Array textures have a
  layer limit that depends on the device (256 is the floor wgpu guarantees). Step 3 reads
  the device's limit, and a rule set over the budget is an error that says its count.
- **Reload:** `r` in `wmc play` reads the sprite files again with the rules; a sprite error
  shows on the status bar and keeps the old sprites.
- **Cost:** `render_cells/160x90` (`crates/app/benches/render.rs`) before and after, in the
  commit and in `PERF.md`; a world without sprites must not slow by more than noise. (Built:
  the sprite tiles are a second pass over a span that runs only when some kind wears one.)

## 5. Tools: `wmc sprite`

All three are headless, deterministic, and share the rasterizer with the game, so what an
agent scores is what a player sees.

- **`wmc sprite render <file.sprite> --png out.png [--sprite NAME] [--pose V] [--cell PX]
  [--bg "#rrggbb"]`**: a sheet with every frame of every pose, each twice: at `--cell`
  size (24 by default, what a player sees) and enlarged 8x, pixel for pixel (what an author
  edits), over `--bg` (the soil's colour by default); a row per pose, and a legend on
  stdout. This is the agent's eyes. With `--rules` (or `WMC_RULES`) each sprite takes its
  kind's colour and size; without, the colour of a kind that declares none, over the cells
  its stamps reach. `--pose V` shows the pose drawn while `look` is V.
- **`wmc sprite score <file.sprite> --sprite NAME --target image.png [--diff out.png]`**:
  how close the first frame is to an image, as numbers an agent can compare between tries:
  - `shape`: overlap of the two silhouettes (intersection over union), at 24 px;
  - `colour`: mean colour distance inside the silhouette, at 24 px;
  - `detail`: the same two at 96 px (a tie-breaker, weighted low: readability first);
  - `stamps`: how many it took.
  The target's background is its alpha channel, else the colour of its corners (`--key`
  to name it), with the colours within 48 of it (RGB distance, of 441). The image is
  fitted into the sprite's box (its kind's footprint with `--rules`, else the cells its
  stamps reach), its aspect kept, centred. A pixel is in a silhouette from alpha 128.
  `colour` is 0 for the same colours, 1 for black against white. It also prints the one
  number `fit` climbs, `value = 0.9 (shape - colour / 4) + 0.1 (the same at 96 px)`.
  `--diff` writes target, sprite and difference side by side at 96 px (grey where both
  are, red where only the image is, blue where only the sprite is). (Built.)
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
  (Built: it climbs `shape - colour / 4` at 24 px; each new stamp is the best of 400
  random ones, nudged 250 times; a last pass nudges each stamp 80 times with the others in
  place; a stamp that adds less than 0.005 ends it. About 6000 small rasters, a tenth of a
  second a draft in a dev build. Palette names are `c1`, `c2`, ... most used first; the
  file starts with a comment naming the image and seed. `--key` names the background as
  for `score`; without `--size`, the kind's size comes from `--rules`, else one cell. On
  three simple test images (a disc, a tree, a mushroom) the draft's `shape` is 0.876,
  0.924 and 0.808 against 0.456, 0.449 and 0.396 for the best single character. A draft
  is shape first and blobby; readability comes from the edits the skill teaches.)
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

## 9. Decided (owner, 2026-11-12)

1. **Where a kind gets its sprite:** the sprite file names the kind (no change to the rules
   language or the sim). Not chosen: a `sprite NAME` line in the kind, which would put looks
   in one place with `glyph` and `color` but change the compiler and the rules hash.
2. **The animation clock:** real time, so a paused world still breathes and a fast one
   doesn't flicker. Not chosen: sim ticks, where animation would stop on pause and speed up
   with the world.
3. **Only letters, at first.** Letters are strokes, so a solid body takes dense characters
   (`@ # O 0`). After step 7's numbers, the owner decides whether a filled disc stamp is
   worth adding.
4. **Kinds only.** Grounds, features and fields are not in this plan: water that ripples
   and grass that sways are a separate, smaller change to how a cell's colour is chosen.
5. **No sound** in this plan.

## 10. Grammar

What the parser in `crates/app/src/sprite/parse.rs` accepts, token by token, in the
notation of `GRAMMAR.md` (`[ x ]` optional, `{ x }` zero or more, `"w"` that word or
symbol). The generator `sprite/gen_sprites.rs` and the fuzzer `sprite/fuzz.rs` are built
from it: when the parser changes, change all three.

```ebnf
comment = "#" { any character but LF }               (to the end of the line)
NAME    = letter_ { letter_ | digit | "." }          (no empty part: `a.`, `a..b` are errors)
NUMBER  = [ "-" ] digit { digit } [ "." digit [ digit [ digit ] ] ]
TIME    = digit { digit } [ "." digit { digit } ] ( "ms" | "s" )
STRING  = '"' { any byte but '"' and LF } '"'
        | "'" { any byte but "'" and LF } "'"       (no escapes: "\" is a backslash)

file    = { palette | sprite } EOF
palette = "palette" "{" { NAME STRING } "}"
sprite  = "sprite" NAME "{" { frames | stamp | pose } "}"
pose    = "pose" NUMBER "{" { frames | stamp } "}"
frames  = "frames" NUMBER [ "every" TIME ]
stamp   = STRING "at" NUMBER NUMBER { option }
option  = "turn" NUMBER | "scale" NUMBER | "phase" NUMBER
        | "color" ( NAME | STRING )
        | "~" ( "turn" | "x" | "y" | "scale" ) NUMBER
```

- **Tokens.** Whitespace (space, tab, CR, LF) and comments separate tokens; any other
  character outside a string is an error. A `#` inside a string is not a comment. Letters
  right after a number are its unit: `ms` or `s`, anything else is an error ("unknown
  unit"). A number has at most 3 decimals (they are kept exactly, in thousandths, so a
  picture is the same on every machine). A NAME may hold dots: `weather.cloud`.
- **The palette.** A colour is `"#rrggbb"` (six hex digits). Names are unique in a file,
  hold no dot, and number at most 64; a stamp may use one declared anywhere in the file.
  Several `palette` blocks add to one palette.
- **A sprite** names a kind; a name appears once in a file. Its body holds at most one
  `frames` line, its stamps, and its poses; a pose holds at most one `frames` line and its
  stamps, no pose. A pose without a `frames` line takes the sprite's; a sprite without one
  is a still (`frames 1`).
- **Ranges** (checked by the parser, with the range in the message):

  | value | range | default |
  |---|---|---|
  | stamp character | one printable ASCII character, `" "` to `"~"` | |
  | `at X Y` | 0 to 16, in cells | |
  | `turn` | -360 to 360, whole degrees | 0 |
  | `scale` | 0.1 to 4, in cells | 1 |
  | `phase` | 0 to 360, whole degrees | 0 |
  | `~turn` | 0 to 180, whole degrees | 0 |
  | `~x`, `~y` | 0 to 16, in cells | 0 |
  | `~scale` | 0 to 4, in cells | 0 |
  | `frames N` | 1 to 16; `every` needed when N > 1 | 1 |
  | `every T` | 50ms to 60s; milliseconds are whole | |
  | `pose V` | 0 to 255, each once in a sprite | |

- **Options** of a stamp come in any order, each at most once.
- **Limits:** 32 stamps a pose (the sprite's own list is one), 8 poses a sprite (its own
  list counted), 64 colours a palette.
- **Loading** (`wmc lint`, the game): a sprite's name is a kind of the loaded rules (in a
  player's pack, that player's: `red.ant` for `ant`), else a warning; two sprites for one
  kind, in any files, are an error. A kind without a sprite wears its nearest parent's of
  the same size. A stamp centred outside its kind's footprint is a warning.

Errors read as the rules compiler's, `file:line:col: message`:

```sprite error
palette { shell "#d9623b" }
sprite crab {
  "O" at 0.5 0.5 scale 5     # error: scale must be from 0.1 to 4
}
```

```sprite error
sprite crab {
  frames 4                   # error: `frames 4` needs `every T`
}
```
