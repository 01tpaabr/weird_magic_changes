# Lessons

Dated, one paragraph each; add yours at the end.

**2026-11-12, letters are strokes.** One `O` is a ring and one `#` a grid: every solid
mass in the first sprites took two to five overlapping stamps. Plan for it: a body is
about four stamps, so a dozen-stamp sprite has room for a body, a head and the one or two
features that name the thing, not more.

**2026-11-12, the plan's crab did not read.** A sprite designed on paper, without
rendering it, came out as a face with ears. Render after every few edits and read the 24
px copy first; the 8x copy flatters.

**2026-11-13, a swing's frames repeat.** `A sin(360 f / N + phase)` with N = 4 and phase
0 gives 0, A, 0, -A: frames 0 and 2 are the same picture, and with N = 2 the swing does
nothing. Fine for a step (rest, left, rest, right); for four distinct frames use a phase
that is not a multiple of 45, or an odd N.

**2026-11-13, the draft's score is not the goal.** `fit` maximises silhouette overlap and
returns blobs that score 0.8 to 0.9 and read as nothing; the hand-built sheep and wolf
score 0.64 and 0.55 and read at a glance. Use the draft for the palette and where the
masses go; judge the result by eye at 24 px, with `score` to compare edits of the same
design, not designs with each other.

**2026-11-13, overlapping limbs merge when they swing.** Two `n` leg pairs whose inner
legs nearly touch become one block on the frame where they swing toward each other. Leave a
gap of a tenth of a cell.
