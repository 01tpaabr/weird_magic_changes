# The stamp vocabulary

Which letters make which shapes, in the bundled font (JetBrains Mono), as used in the
sprites built so far. Sizes are at `scale 1` (the font size is one cell); everything scales
with `scale`. `X Y` is the centre of the letter's ink.

| shape | letters | notes |
|---|---|---|
| a body, wide | `O` turned 90 | an `O` is about 0.5 wide and 0.7 tall: on its side it is 0.7 wide, 0.5 tall. A ring: fill it |
| fill | `=` (horizontal bars), `=` turned 90 (vertical), `#` (a grid), `#` turned 45 | two or three over each other close most holes at 24 px; the gaps left read as texture (fleece, fur) |
| a solid round mass | `@`, `.` at a big scale | `@` is big and tall at scale 1 (about 0.75); a `.` at scale 1.5 is a round dot about 0.25 across: a head |
| fluff, curls | `*` | the fit found it for a sheep's fleece; at 0.7 to 0.8 it rounds a back without spikes |
| legs | `n` (two legs in one), `\|`, `/`, `\` | `n` at 0.6 to 0.75 reads as a pair of legs at 24 px; a `\|` at scale 1 is a pixel wide. Space two `n`s so their inner legs don't touch (a swing merges them) |
| a limb, an arm with a fork | `Y` | claws, antlers, a raised branch |
| curves | `(` `)` `c` `J` `S` | `(` turned -60 is a tusk; `J` hanging is a trunk or a tail |
| eyes | `.` `'` `:` | dark, scale 0.5 to 0.7; `'` is a stalk, two of them eyes on stalks; below 0.4 an eye is a smudge |
| ears, beaks, snouts | `^` `v` `<` `>` | `^` an ear (turn it back for a running animal), `<` a snout facing left |
| a back, a stripe | `-` `_` | `-` at 1.1 in a darker colour over a body is a back |
| a tail | `/` `\` `-` | `/` turned 20 raised, `-` straight back |

Characters that look alike at 24 px: `O`/`0`, `l`/`|`/`I`, `.`/`'`/`` ` ``. Characters that
draw nothing: the space.
