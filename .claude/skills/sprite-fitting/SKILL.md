---
name: sprite-fitting
description: Make a sprite (a kind drawn as a picture made of letters, docs/SPRITES.md) from an image, or by hand, that reads as the thing at 24 pixels a cell. Prepares the image, drafts with `wmc sprite fit`, looks with `wmc sprite render`, edits by hand with a vocabulary of letter shapes, measures with `wmc sprite score`, animates and poses it, and checks it in the window. Use for any request to draw, redraw, animate or improve how a kind looks as a sprite.
---

# sprite-fitting

A sprite is a dozen letters on a 24-pixel cell. It can't copy an image; it must **read as
the thing** (owner, 2026-11-12: readability over likeness). Work by looking, not by
numbers alone: the score tells you whether a change helped the outline, your eyes tell you
whether it still reads as a sheep. Read `docs/SPRITES.md` §3 (the file) and §5 (the tools)
first; `references/vocabulary.md` is the table of which letters make which shapes,
`references/examples.md` the worked examples, `references/lessons.md` what went wrong
before.

## The loop

1. **Prepare the target.** One subject, cropped tight, on a plain background, few colours.
   A busy image (a photo, a scene, a texture) is simplified first: say so to the user, and
   draw or ask for a simpler one (a silhouette with its two or three colours is best). The
   background is the image's alpha, else its corners' colour: a white animal on white
   loses its body, so give it a contrasting ground (or `--key`). An image you draw
   yourself (PIL is installed) is fine: shapes in the kind's own colours, side view for
   animals, the footprint's aspect (1:1 for a one-cell kind, 3:3 for `size 3 3`).
2. **Draft.** `wmc sprite fit <image> --kind NAME -o draft.sprite --stamps 8 --colors 3`
   (6 to 10 stamps, 2 to 4 colours; `--rules <pack>` gives the kind's size, else
   `--size W H`; try two or three `--seed`s and keep the best `shape`). A draft is shape
   first and blobby: use it for the palette and where the masses go, not as the answer.
3. **Look.** `wmc sprite render draft.sprite --png sheet.png --rules <pack>` and read the
   PNG, **the small copy first** (top row, 24 px: what a player sees). Name what it reads
   as. If it does not read as the thing at 24 px, more detail won't fix it: change the big
   shapes (body, head, the one feature that names it: a mammoth's trunk, a crab's claws).
4. **Edit by hand.** Rebuild the masses with the vocabulary: a body is an `O` turned 90
   (wide), filled with `=` and `#`; limbs are `|` `/` `\` or `n` (two legs in one); eyes
   are a dark `.` at scale 0.5 to 0.7; ears, beaks and snouts `^ v < >`. Move, turn, swap
   characters; remove stamps that don't change the read. Order is depth: legs first, body,
   then head and face over it.
5. **Score** after each change: `wmc sprite score file.sprite --sprite NAME --target
   image.png --rules <pack> --diff diff.png` (red in the diff: the image where the sprite
   is not; blue: the sprite where the image is not). Keep the better file; stop when two
   rounds don't improve `shape` or the picture reads right. A hand edit that reads better
   at a slightly lower `shape` is the better file.
6. **Animate and pose.** Swings on the limbs and the feature that names it (`~turn`
   on a claw or trunk, `~x` on legs with `phase 180` on every other one), `frames 4 every
   200ms` to `300ms`. A pose for each `look` the rules set (`pose 1 { ... }`): if the
   rules set none and the kind has a state worth showing (grazing, hunting, asleep), a
   `look = 1` in that rule is a one-line change to the rules that the sim never reads.
7. **See it in the world.** `wmc lint <pack>` (no warnings), a scenario of the pack, and a
   screenshot of `wmc play` at two zooms (the window recipe in `/bevy-dev`; kill the window
   after): sprites show from 16 px a cell up, glyphs below.

## Facts that bite

- **Letters are strokes.** One `O` is a ring; a solid body takes two or three overlapping
  stamps (`O` + `=` + `#`, or `@`), and holes at 24 px read as spots.
- **`scale` is the font size**, not the letter's height: a capital is about 0.7 of it, an
  `O` about half of it wide (the font is monospaced). `X Y` is the ink's centre.
- **A swing's frames 0 and 2 are the same** (sin 0 = sin 180): two frames are a still,
  four repeat frames 0 and 2 (fine for a step: rest, left, rest, right). For four
  distinct pictures give the swing a `phase` that is not a multiple of 45 (`phase 22`), or
  use an odd number of frames (3, 5); six repeat too.
- **Below 2 px a detail is gone** at 24 px: an eye at scale 0.4 is a smudge; thin `|`
  legs at scale 1 are one pixel, `n` at 1.5 reads as two legs.
- **A child wears its parent's picture in the parent's colours**: give it a sprite of its
  own to tell them apart.
- **Coordinates run 0 to W** for a kind of `size W H`; a stamp centred outside is a lint
  warning.

Add what you learn to `references/lessons.md`, dated, one paragraph each.
