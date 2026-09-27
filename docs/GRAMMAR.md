# The rules grammar

The formal grammar of `*.rules` files: what the parser in
`crates/sim-core/src/rules/compile.rs` accepts today, token by token. `RULES.md` is the
author's reference and says what each form means; this file says which texts parse, and
the static rules a program that parses must also keep to compile. The generators in
`rules/gen_rules.rs` are built from it: when the parser changes, change both.

Notation: `a = ...` defines `a`; `|` separates alternatives; `[ x ]` is optional; `{ x }`
repeats zero or more times; `"if"` is that word or symbol as one token; UPPER names are
tokens of the lexical grammar (§1). A note in *italics* after a production is a condition the
parser checks that the EBNF alone does not say.

## 1. Tokens

A file is a sequence of tokens separated by whitespace (space, tab, CR, LF) and comments.
Any other character outside a string is an error: `!`, `&`, `|`, `'`, `$`, a NUL byte and
every non-ASCII character among them.

```
comment  = "#" { any character but LF }                    (to the end of the line)
NAME     = letter_ { letter_ | digit }                     letter_ = A-Z | a-z | _
INT      = digit { digit }                                 (decimal, 0 to 2147483647)
TIME     = digit { digit } unit                            unit = "min" | "h" | "d"
STRING   = '"' { any byte but '"' and LF } '"'             (no escapes)
SYMBOL   = "=>" | "==" | "!=" | "<=" | ">=" | "+=" | "-="
         | "{" | "}" | "(" | ")" | "," | ":" | "=" | "<" | ">"
         | "+" | "-" | "*" | "/" | "%" | ";" | "."
```

- **Longest match.** A two-character symbol wins over its first character: `<=` is one
  token, `< =` two. A NAME takes every letter, digit and `_` that follows.
- **Numbers.** Only decimal integers: no sign (a `-` is the operator), no fraction, no hex.
  A value above 2147483647 is an error ("number too large"), so the smallest `i32` is
  written `-2147483647 - 1`. Letters right after the digits are a unit: `min` (15 ticks),
  `h` (900), `d` (21600); any other letters are an error ("unknown unit"), and so is a time
  above 2147483647 ticks ("time too long", past about 99 420 days). A number may not run
  into a name: `5_m` and `3h2` are errors.
- **Strings** appear only after `glyph` and `color`. A `#` inside one is not a comment.
- The file ends with an EOF token; `EOF` below.

**Reserved words** are never a NAME where the grammar says `IDENT`:

```
kind trait sub const extends inherit state when
glyph color cover tags cadence sight fuel bite place need max decay vital mem
if else while repeat for each choose let return
and or not true false nearest sniff count within as only is free
idle die become spawn with move drink eat hit graze take give next look signal mark
here attacker toward away random at dir north east south west
min abs sign clamp rand chance dist look_of signal_of scent pack hi lo
blocked missed refused
x y age light hour day kind look signal state hurt hurt_dir result taken trapped
```

(`look`, `signal`, `kind` and `state` are senses as well as keywords; `place` is refused
everywhere with a pointer to scenarios.)

```
IDENT = NAME that is not a reserved word
```

**Contextual words** are ordinary IDENTs except in one position:

| word | special where | elsewhere |
|---|---|---|
| `food` | starts a declaration at the top of a kind or trait body | a need, mem, local, kind... name |
| `water`, `soil`, `rock`, `bare` | a predicate (`pred`), and a `pred` argument of a call | a need, mem, local... name; never a kind or a tag |
| `target`, `pred` | a sub parameter's type, after `:` | any name |
| `dx`, `dy` | a field, after `.` | any name |

## 2. Files and items

```
file      = { item | sub | const } EOF
const     = "const" IDENT "=" expr
item      = kind | trait
kind      = "kind" IDENT [ extends ] body_
trait     = "trait" IDENT [ "(" [ IDENT { "," IDENT } ] ")" ] [ extends ] body_
extends   = "extends" parent { "," parent }
parent    = IDENT [ "(" [ expr { "," expr } ] ")" ]
body_     = "{" { decl } { sub } rule_list { state } "}"
state     = "state" IDENT "{" rule_list "}"
rule_list = { rule | inherit }
inherit   = "inherit" [ IDENT ]
rule      = "when" cond "=>" ( block | stmt )
sub       = "sub" IDENT "(" [ param { "," param } ] ")" block
param     = IDENT [ ":" ( "target" | "pred" ) ]
```

- *A kind's name is not `water`, `soil`, `rock` or `bare`. A kind takes no parameter list
  (`kind k(a)` is an error); a trait's parameters are distinct. A parent is listed once.*
- *In a body: declarations, then member subs, then reflex rules and `inherit`s, then states,
  in that order. Anything out of order is an error naming the order. A state name is given
  once, at most 64 states per body; a member sub name once per body.*
- `inherit` takes the IDENT that follows it, if any, even on the next line: what follows an
  `inherit` in a valid list is `when`, `inherit`, `state` or `}`, all reserved.
- There are no trailing commas anywhere.

## 3. Declarations

```
decl = "glyph" STRING
     | "color" STRING
     | "cover"
     | "tags" { TAG }
     | "cadence" additive | "sight" additive | "fuel" additive | "food" additive | "bite" additive
     | "need" IDENT "max" additive [ "decay" INT ] [ "vital" ]
     | "mem" IDENT { "," IDENT }
TAG  = IDENT that is not "food", "water", "soil", "rock" or "bare"
```

- *`glyph`: a string of one printable ASCII character (`!` to `~`). `color`: `"#rrggbb"`,
  six hex digits. Neither in a trait.*
- *`glyph`, `color`, `cover`, `cadence`, `sight`, `fuel`, `food` and `bite` are given at
  most once per body; `tags`, `need` and `mem` repeat, each name once (a mem may not repeat
  a need of the same body).*
- *`decay` is followed by the INT `0` (points) or `1` (per tick, the default); nothing else.*
- **Where a tag list ends.** It takes NAMEs until one that starts something else: a
  declaration word (`food` included), `sub`, `when`, `inherit`, `state`, or any non-NAME
  token (`}`). A reserved word or a predicate word inside the list is an error. An empty
  `tags` is allowed.
- The numbers are `additive`, not `expr`: a comparison cannot follow them (§6).

## 4. Statements

```
block = "{" { stmt [ ";" ] } "}"
stmt  = "if" cond block [ "else" ( if_stmt | block ) ]
      | "while" cond block
      | "repeat" expr block
      | "for" "each" pred "within" additive "as" IDENT block
      | "choose" "{" arm { arm } "}"
      | "let" IDENT "=" expr
      | "return" [ expr ]
      | "idle" | "die"
      | "become" IDENT
      | "spawn" IDENT "at" target [ "with" "(" IDENT "=" expr { "," IDENT "=" expr } ")" ]
      | ( "take" | "give" ) target IDENT expr
      | ( "move" | "drink" | "eat" | "hit" | "graze" ) target
      | "look" "=" expr
      | "signal" "=" expr
      | "mark" IDENT expr
      | "next" IDENT
      | IDENT args
      | IDENT ( "=" | "+=" | "-=" ) expr
arm   = expr ":" ( block | stmt ) [ ";" ]
```

(`if_stmt` is the first alternative: `else if` chains.)

- `if`, `while`, `repeat` and `for each` bodies are always a block; a rule body and a
  `choose` arm are a block or one statement.
- **`;`** may follow a statement inside a block and a `choose` arm, once. It is not accepted
  after a rule's single-statement body, a declaration, a `const` or an item (`when c =>
  idle;` is an error).
- **Newlines** matter in one place: **a `return`'s value starts on the `return` line.** A
  `return` followed on its line by nothing, a comment, `;` or `}` returns nothing, and the
  next line is the next statement or arm. Everywhere else a newline is whitespace.
- **An expression runs as far as it can.** A statement that ends in an expression (`x = y`,
  `mark t 5`, `take f n 1`) absorbs a following `-` or `(`: in `choose { 1: x = y  -1: idle }`
  the arm is `x = y - 1`, and `1: x = g  (2): idle` calls `g(2)`. No statement starts with
  `-` or `(`, so this only bites a `choose` weight: separate such an arm with `;`.
- *`with` names distinct memory slots, at most two.*
- *`choose` has at least one arm.*

## 5. Conditions

```
cond     = and_cond { "or" and_cond }
and_cond = not_cond { "and" not_cond }
not_cond = "not" not_cond
         | "nearest" pred "within" additive "as" IDENT
         | "sniff" IDENT "within" additive "as" IDENT
         | "(" cond ")"                (when no binary operator follows the ")")
         | expr
```

Conditions are the only place `and`, `or`, `not`, `nearest` and `sniff` appear: after `when`,
`if` and `while`. An expression is a condition (nonzero is true), but a condition is not an
expression: `let b = x > 0 and y > 0` is an error.

A `(` at the start of a condition is tried first as a parenthesised condition; if that fails
or a binary operator follows its `)`, it is parsed again as an expression. So `(a and b) or
c` groups conditions, and `(a + 1) * 2 > b` is a comparison.

## 6. Expressions

```
expr     = additive { ( "<" | "<=" | "==" | "!=" | ">=" | ">" ) additive }
additive = term { ( "+" | "-" ) term }
term     = unary { ( "*" | "/" | "%" ) unary }
unary    = "-" unary | primary
primary  = INT | TIME
         | "true" | "false" | "blocked" | "missed" | "refused"
         | sense
         | "(" expr ")"
         | "count" pred "within" unary
         | ( "rand" | "chance" ) "(" expr ")"
         | ( "dist" | "free" | "look_of" | "signal_of" ) "(" target ")"
         | "is" "(" target "," pred ")"
         | "scent" "(" IDENT [ "," target ] ")"
         | ( "min" | "max" | "abs" | "sign" | "clamp" | "pack" | "hi" | "lo" ) "(" expr { "," expr } ")"
         | IDENT args
         | IDENT "." ( "dx" | "dy" )
         | IDENT
sense    = "x" | "y" | "age" | "light" | "hour" | "day" | "kind" | "look" | "signal"
         | "state" | "hurt" | "hurt_dir" | "result" | "taken" | "trapped"
args     = "(" [ arg { "," arg } ] ")"
arg      = target        (if it starts with here, attacker, toward, away, random, north,
                          east, south, west, or at or dir followed by "(")
         | pred          (if it starts with only, or with an IDENT followed by ":")
         | "free"        (followed by "," or ")": the predicate)
         | IDENT         (followed by "," or ")": typed by the parameter)
         | expr
```

- *Function arity: `abs`, `sign`, `hi`, `lo` take 1 argument; `min`, `max`, `pack` 2;
  `clamp` 3.*
- `blocked`, `missed` and `refused` read as `result == BLOCKED` (and so on).
- `count`'s radius is one `unary`: `count fox within 3 - 1` is the count minus one. The
  radii of `nearest`, `sniff` and `for each` are an `additive`.
- A bare IDENT argument is an int, a target or a predicate as the called sub's parameter
  says; `water`, `soil`, `rock` and `bare` passed to a `pred` parameter are the predicates
  unless a local of that name is in scope.

**Precedence**, loosest first; every binary operator is left-associative:

| level | operators | |
|---|---|---|
| 1 | `or` | conditions only |
| 2 | `and` | conditions only |
| 3 | `not` | conditions only, prefix: `not a == b` is `not (a == b)`, `not a and b` is `(not a) and b` |
| 4 | `< <= == != >= >` | chains: `a < b < c` is `(a < b) < c` |
| 5 | `+ -` | |
| 6 | `* / %` | |
| 7 | `-` | prefix; `-5` is folded to the constant |

## 7. Targets and predicates

```
target = "here" | "attacker" | "north" | "east" | "south" | "west"
       | "dir" "(" expr ")"
       | "at" "(" expr "," expr ")"
       | "toward" target | "away" target
       | "random" "free"
       | IDENT
pred   = "water" | "soil" | "rock" | "free" | "bare"
       | [ "only" ] IDENT [ ":" INT ]
```

- *`only` does not go before `water`, `soil`, `rock`, `free` or `bare`. The look after `:`
  is an INT from 0 to 255 (not a TIME).*

## 8. Nesting

One tree (a rule, a sub, a `const`, a declaration's number) may be at most **128 levels**
deep: each nested statement, parenthesis, operand, call argument, prefix `-`, `not`,
`toward` and `away` is one level, and so is each binary operator in a chain (`a + b + c`
is two levels above its operands). Deeper is an error ("nested too deep"). This bounds every
recursion over the tree: the parser, the code generator, the lint and dropping the tree.

## 9. Static rules

A program that parses compiles only if it also keeps these. Each points at the section of
`RULES.md` that says more.

**Names and scopes.**
- *Global, across every file and pack* (§1, §15): kinds and traits share one namespace;
  file subs, consts and tags are global too, and none of them may reuse a kind's, trait's
  or each other's name. Scent names are a namespace of their own, at most 4 (§9).
- *Per kind or trait:* needs and mems share one namespace (a name is a need or a mem, not
  both); states and member subs have their own. A member sub may not share a name with a
  file sub, a kind or a trait (§15).
- *Per rule or sub:* locals (`let`), bindings (`as v`, `for each ... as v`) and sub
  parameters. A local lives until the end of its block (§7). A local may not reuse the name
  of another local in scope ("already bound"), nor of a need or mem the code can name, but
  may hide a trait parameter or a constant.
- *Lookup of a name in an expression* (§7): local, then need, then mem, then trait
  parameter, then constant. A file sub sees only its parameters, locals and constants
  (§12); a trait's rules and member subs only what the trait and its own parents declare
  (§6).
- A trait parameter may not share a name with a constant, or with a need or mem of its
  trait.
- Constants and every number in a declaration or an `extends` argument are folded at
  compile time: numbers, constants declared above, trait parameters in scope, arithmetic,
  comparisons and `min max abs sign clamp pack hi lo` (§12).

**Types.** Every value is one of three types, never converted:
- `int`: literals, senses, needs, mems, lets, int parameters, constants, and every
  expression.
- `target`: a binding and a `: target` parameter. Read through `v.dx`, `v.dy`, `dist(v)` or
  passed on; never stored.
- `pred`: a `: pred` parameter. Used where a predicate goes (`count p`, `nearest p`, `is(t,
  p)`, `for each p`) or passed on; never with `only` or `:look`.
- A sub that has a `return expr` anywhere is a function: all its `return`s carry a value,
  and it can be called in an expression. Otherwise it is a procedure: a statement only
  (§12). `return` is only allowed in a sub.

**Actions** (§2, §4, §8, §12).
- One action per think. A statement that acts on every path followed by another action in
  the same block is an error; so is a second `next` in one block. What the compiler cannot
  see traps at run time.
- A `when` condition may call only subs that never act or `next`, through every sub they
  call (8 deep). `if` and `while` conditions in a body have no such rule.
- `take`, `give` and `next` belong to a kind: allowed in its rules and member subs, not in
  a file sub. `next` names a state of the code's owner; `take` and `give` one of its needs.
- `spawn` and `become` name a kind, never a trait; `spawn ... with` names mems of that kind,
  and all the spawns of one kind name at most two distinct ones.
- `nearest ... as` and `sniff ... as` bind only as a top-level conjunct of a `when`, `if` or
  `while` condition (under `and` or parentheses, not under `or` or `not`), and the binding
  is visible in that rule's or statement's body only.

**Limits** are in `RULES.md` §18: needs 4 and mems 12 per kind after merging, 64 tags and 4
scents per rule set, 64 states, `sight` 0 to 16, `fuel` 1 to 4096, `cadence` a power of two,
`bite` 0 to 255, 16 local slots and a 64-value stack per rule or sub, 8 nested calls,
32767 ops per rule, state or sub.
