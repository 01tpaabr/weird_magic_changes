# Worked examples

Each: the target, the draft, what was changed and why, the result with its numbers.
Scores are `wmc sprite score` against the target with the pack's `--rules`.

## The sheep (`packs/pasture/pasture.sprite`, 2026-11-13)

- **Target:** `pasture/sheep.png`, drawn with `pasture/draw_targets.py` (PIL): side view
  facing left, a fleece of seven white circles, a dark head and ear, four dark legs, on a
  blue ground (a white subject on white would key out).
- **Draft:** `wmc sprite fit sheep.png --kind sheep --stamps 10 --colors 3 --rules
  packs/pasture`, seeds 1 to 3: shape 0.768, 0.852, 0.884; kept seed 3
  (`pasture/sheep_draft.sprite`). At 24 px a white blob with a dark dot; the legs were one
  `*` smear. It found `*` for the fleece and a big `.` for the head: both kept.
- **Edits:** legs as two `n` at 0.6, low (0.83); the fleece an `O` turned 90 filled with
  `#` and `=`, two `*` on the back at 0.7 to 0.8 (bigger ones stuck out as spikes: 0.668
  with spikes, 0.652 without, kept without); the head a `.` at 1.5, an ear `^` turned -30.
  A try with `@` as the body (0.509) was far too big.
- **Result:** shape 0.638, colour 0.06; reads as a sheep at 24 px. Animated: the leg pairs
  swing `~x 0.02` in opposite phase. Pose 1 (grazing, `look = 1` added to the grazing
  rule): the head and ear down to 0.64.

## The wolf (`packs/pasture/pasture.sprite`, 2026-11-13)

- **Target:** `pasture/wolf.png`: grey body, darker back, raised tail, snout, an ear, four
  legs, on white.
- **Draft:** seeds 1 to 3: 0.792, 0.803, 0.754; seed 2 (`pasture/wolf_draft.sprite`) was a
  grey blob with spikes.
- **Edits:** built from the vocabulary instead: `n` legs, `/` tail, `O` turned 90 body,
  `-` darker back, `<` snout, a `.` at 1.7 head, `^` ear, a dark `.` eye: 0.417, reads as a
  dog. Filling the body (`#`, then `=` turned 90 and `#` turned 45, the mammoth's trick)
  and a bigger head: 0.493, 0.544.
- **Result:** shape 0.545. Pose 1 (hunting, `look = 1` on the rules that eat or walk
  toward a sheep): the head lower, the ear turned back, the tail straight, the legs
  swinging twice as wide at 120 ms.

A draft scored higher than either result (0.88 and 0.80 against 0.64 and 0.55) and read as
neither: the draft fills the silhouette with whatever overlaps it best; a picture that
reads needs the parts that name the thing (a head, legs, an ear), which cost overlap.

## The crab (`packs/crabs/crabs.sprite`, by hand, 2026-11-12)

The plan's first crab (`c` claws, an `O` body, two `.` eyes) read as a face with ears: the
claws were as big as the body and there were no legs. Rebuilt: a wide shell (`O` turned 90,
filled with `=`), forked claws (`Y` at 0.65, turned out 25 degrees, swinging 15), eyes on
stalks (`'`), legs (`<` `>`). Pose 1, tucked in: the shell alone, the eyes lower.

## The mammoth (`packs/giants/giants.sprite`, by hand, 2026-11-13)

3 x 3 cells. A body of an `O` turned 90 at scale 3.2 filled with `=`, `=` turned 90, two
`#` and a `#` turned 45; a head `O` with a `#`; a trunk `J` swinging 12 degrees; a tusk `(`
in ivory turned -60; legs as two `n` at 1.5 stepping in opposite phase; a `/` tail; a dark
`.` eye. Thin `|` legs were a pixel wide at 24 px a cell: `n` fixed it.
