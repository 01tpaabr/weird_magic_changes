use super::*;
use crate::rules::vm::Op;

fn compile_ok(text: &str) -> Kinds {
    compile("t.rules", text).unwrap_or_else(|e| panic!("{e}"))
}

fn compile_err(text: &str) -> String {
    compile("t.rules", text)
        .err()
        .map(|e| e.to_string())
        .expect("should not compile")
}

/// One think of `kind` in a halo over one chunk (the world's first),
/// standing at local `(x, y)`.
fn run_think_in(
    k: &Kinds,
    kind: &str,
    mind: &mut crate::actors::ActorMind,
    cells: &crate::stage::ChunkCells,
    actors: &crate::actors::ChunkActors,
    (x, y): (usize, usize),
    tick: u64,
    rng: u64,
) -> crate::rules::vm::Outcome {
    use crate::rules::vm::{self, Ctx, Halo};
    let mut chunks = [None; 9];
    chunks[4] = Some((cells, actors));
    let halo = Halo {
        flies: 0,
        rests: 0,
        chunks,
        tags: &k.tag_bits,
        family_end: &k.family_end,
        materials: &k.materials.table,
    };
    let ctx = Ctx {
        halo: &halo,
        kind: k.by_name(kind).unwrap(),
        cell: y * 64 + x,
        pos: crate::stage::Pos::new(x as i32, y as i32),
        tick,
        rng,
        look: 0,
        signal: 0,
        heads: 1,
    };
    vm::think(k, ctx, mind)
}

/// [`run_think_in`] a bare chunk.
fn run_think(
    k: &Kinds,
    kind: &str,
    mind: &mut crate::actors::ActorMind,
) -> crate::rules::vm::Outcome {
    let cells = crate::stage::ChunkCells::default();
    let actors = crate::actors::ChunkActors::default();
    let rng = crate::rules::vm::rng_base(1, 5, 9);
    run_think_in(k, kind, mind, &cells, &actors, (36, 1), 5, rng)
}

#[test]
fn plants_file_compiles_to_the_hand_assembled_program() {
    let text = crate::rules::builtin::ORACLE_PLANTS;
    let compiled = compile("plants.rules", text).unwrap_or_else(|e| panic!("{e}"));
    let expected = crate::rules::builtin::hand_assembled();
    let ops = |k: &Kinds| {
        k.code
            .iter()
            .map(|o| format!("{o:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(ops(&compiled), ops(&expected));
    assert_eq!(compiled.consts, expected.consts);
    assert_eq!(compiled.defs, expected.defs);
    assert_eq!(compiled.hash, expected.hash);
}

#[test]
fn lexer_handles_times_strings_symbols_and_comments() {
    let toks = Lexer::new(
        "t",
        "kind x { # c\n glyph \"T\" 6h 30min 2d 7 => >= += c.dx }",
    )
    .lex()
    .unwrap();
    let kinds: Vec<Tok> = toks.into_iter().map(|t| t.tok).collect();
    assert_eq!(
        kinds,
        vec![
            Tok::Name("kind".into()),
            Tok::Name("x".into()),
            Tok::Sym("{"),
            Tok::Name("glyph".into()),
            Tok::Str("T".into()),
            Tok::Time(hours(6) as i32),
            Tok::Time(minutes(30) as i32),
            Tok::Time(days(2) as i32),
            Tok::Int(7),
            Tok::Sym("=>"),
            Tok::Sym(">="),
            Tok::Sym("+="),
            Tok::Name("c".into()),
            Tok::Sym("."),
            Tok::Name("dx".into()),
            Tok::Sym("}"),
            Tok::Eof,
        ]
    );
    assert!(compile_err("kind a { glyph \"a\" 3w }").contains("unknown unit"));
    // A number does not run into a name: `5_m` is not `5` then `_m`.
    let glued = "kind a { mem m, _m\n when true => { m = 5_m = 2 } }";
    assert_eq!(
        compile_err(glued),
        "t.rules:2:21: `5_m`: a number runs into a name"
    );
    assert_eq!(
        compile_err("kind a { mem m\n when true => { m = 3h2 } }"),
        "t.rules:2:21: `3h2`: a number runs into a name"
    );
    assert!(compile_err("kind a { glyph \"ab\" }").contains("one printable"));
    assert!(compile_err("kind a { glyph \"a\" when 1 => x = $ }").contains("unexpected character"));
    // A NUL byte is a character like any other, not the end of the file.
    assert_eq!(
        compile_err("kind a { glyph \"a\" }\n\0kind b { garbage garbage }"),
        "t.rules:2:1: unexpected character `\\0`"
    );
    assert!(compile_err("kind a { when true => idle  é }").contains("unexpected character `é`"));
    // Columns count characters, not bytes.
    assert_eq!(
        compile_err("# é\nkind a { glyph \"é\" $ }"),
        "t.rules:2:20: unexpected character `$`"
    );
}

#[test]
fn errors_carry_positions_and_name_the_problem() {
    assert_eq!(
        compile_err("kind a {\n  need water max 1h\n  when watter > 0 => idle\n}"),
        "t.rules:3:8: unknown name `watter` (not a need, mem slot or binding of this kind)"
    );
    assert!(compile_err("kind a { cadence 3 }").contains("power of two"));
    assert!(compile_err("kind a { sight 65 }").contains("0 to 64"));
    assert!(compile_err("kind a { need w max 1h need w max 2h }").contains("declared twice"));
    assert!(compile_err("kind a { mem m, m }").contains("declared twice"));
    assert!(compile_err("kind a { when 1 => become b }").contains("unknown kind `b`"));
    assert!(compile_err("kind a { when 1 => spawn a at c }").contains("unknown name `c`"));
    assert!(compile_err("kind a { when 1 => light = 2 }").contains("reserved word"));
    assert!(
        compile_err("kind a { when nearest free within 1 as c or 1 => idle }")
            .contains("top-level conjunct")
    );
    assert!(
        compile_err("kind a { when not nearest free within 1 as c => idle }")
            .contains("top-level conjunct")
    );
    assert!(compile_err("kind a { when 1 => idle } kind a { }").contains("declared twice"));
    assert!(
        compile_err("kind a { when 1 => idle\n glyph \"x\" }").contains("declarations come first")
    );
    assert_eq!(
        compile_err("kind a { glyph \"a\"\n state S { when true => idle }\n when true => idle }"),
        "t.rules:3:2: reflex rules (`when`, `inherit`) come before the states"
    );
    assert!(
        compile_err("kind a { state S { when true => idle } 7 }")
            .contains("expected `state` or `}`, found number 7")
    );
    assert!(compile_err("kind a { when 1 => { idle").contains("unclosed block"));
    // At the end of the file the error names the end, not the token before it.
    assert_eq!(
        compile_err("kind a {\n need w max 5 decay"),
        "t.rules:2:20: expected 0 (points) or 1 (per tick), found end of file"
    );
    assert_eq!(
        compile_err("kind a {\n when food >"),
        "t.rules:2:13: expected an expression, found end of file"
    );
    assert_eq!(
        compile_err("kind a {\n when 1 => choose { 1: idle "),
        "t.rules:2:12: unclosed `choose`"
    );
    assert!(compile_err("kind a { when min(1) > 0 => idle }").contains("takes 2 arguments"));
    assert!(compile_err("kind a { need n max 1h mem n }").contains("declared twice"));
    let many: String = (0..5).map(|i| format!("need n{i} max 1h ")).collect();
    assert!(compile_err(&format!("kind a {{ {many} }}")).contains("has 5 needs, at most 4"));
    // Subs and targets.
    assert!(compile_err("kind a { when 1 => f(1) }").contains("unknown sub `f`"));
    assert!(
        compile_err("sub f(n) { } kind a { when 1 => f(1, 2) }")
            .contains("takes 1 arguments, 2 given")
    );
    assert!(
        compile_err("sub f(t: target) { } kind a { when 1 => f(3) }").contains("must be a target")
    );
    assert!(
        compile_err("sub f(n) { } kind a { when f(3) > 0 => idle }").contains("returns nothing")
    );
    assert!(compile_err("sub f(n) { return } sub f(m) { }").contains("sub `f` declared twice"));
    assert!(compile_err("kind a { mem m when 1 => return 3 }").contains("outside a sub"));
    assert!(
        compile_err("sub f() { water = 1 } kind a { need water max 1h }").contains("cannot assign")
    );
    assert!(
        compile_err("sub f() { let a = water } kind a { need water max 1h }")
            .contains("sees only its parameters")
    );
    assert!(
        compile_err("kind a { when nearest free within 1 as c => c = 2 }")
            .contains("not an integer local")
    );
    assert!(
        compile_err("kind a { when nearest free within 1 as c => let v = c }")
            .contains("is a target")
    );
    assert!(compile_err("kind a { when 1 => move toward 3 }").contains("expected a target"));
    assert!(compile_err("kind a { when 1 => { let v = 1  let v = 2 } }").contains("already bound"));
}

#[test]
fn defaults_and_declarations_land_in_the_def() {
    let k = compile_ok(
        "kind a { }\nkind b { glyph \"b\" cadence 16 sight 7 fuel 99 food 2h bite 3 tags meat feed need w max 3d decay 1 need h max 5 decay 0 vital mem p, q }",
    );
    let a = &k.defs[0];
    assert_eq!(
        (a.glyph, a.cadence_shift, a.sight, a.fuel, a.food, a.bite),
        (b'?', 3, 4, 512, 0, 1)
    );
    let b = &k.defs[1];
    assert_eq!(
        (b.glyph, b.cadence_shift, b.sight, b.fuel, b.food, b.bite),
        (b'b', 4, 7, 99, hours(2) as i32, 3)
    );
    assert_eq!(b.needs.len(), 2);
    assert!(b.needs[0].decays && !b.needs[0].vital);
    assert!(!b.needs[1].decays && b.needs[1].vital);
    assert_eq!(b.mems, vec!["p", "q"]);
    assert_eq!(b.entry, 1); // kind a's program is one Halt
    assert_eq!(k.code[0].code, OpCode::Halt);
}

#[test]
fn built_in_function_names_are_reserved() {
    for w in [
        "min", "abs", "sign", "clamp", "rand", "chance", "dist", "is", "free",
    ] {
        let want = format!("`{w}` is a reserved word");
        let e = compile_err(&format!("kind a {{ mem {w} }}"));
        assert!(e.contains(&want), "{e}");
        let e = compile_err(&format!("sub {w}() {{ idle }} kind a {{ }}"));
        assert!(e.contains(&want), "{e}");
    }
    // Still built-ins, and the contextual words still name needs.
    compile_ok(
        "kind a { glyph \"a\" need water max 1d need soil max 1d mem m\n when water < 1h and soil > 0 and chance(50) and free(here) and is(here, water) => m = min(abs(sign(-5)), clamp(rand(3), 0, dist(here))) }",
    );
}

#[test]
fn ground_and_feature_are_not_senses() {
    for (text, want) in [
        (
            "kind a { when ground == soil => idle }",
            "unknown name `ground`",
        ),
        (
            "kind a { when feature > 0 => idle }",
            "unknown name `feature`",
        ),
    ] {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}: {e}");
    }
    // `is` asks instead, and the words are free for a kind's names.
    compile_ok(
        "kind a { mem ground  mem feature\n \
             when is(here, water) and not is(here, rock) => { ground = 1  feature = 2 } }",
    );
}

#[test]
fn a_pred_parameter_takes_the_ground_feature_free_and_bare_words() {
    use bytemuck::Zeroable;
    for w in ["water", "soil", "rock", "free", "bare"] {
        compile_ok(&format!(
            "sub near(p: pred) {{ if count p within 2 > 0 {{ idle }} }}\n kind a {{ when true => near({w}) }}"
        ));
    }
    // The word is the ground, not a kind or tag lookup.
    let k = compile_ok(
        "sub f(p: pred) { if is(here, p) { return 1 }  return 0 }
             kind a { mem m  when true => { m = f(soil) + 2 * f(water) + 4 * f(rock)  idle } }",
    );
    let mut mind = crate::actors::ActorMind::zeroed();
    run_think(&k, "a", &mut mind);
    assert_eq!(
        mind.mem[0], 1,
        "only the soil test holds on a bare soil cell"
    );
    // An int parameter still reads the need of that name.
    compile_ok(
        "sub low(v) { return v < 100 }
             kind a { glyph \"a\" need water max 1d  when low(water) => idle }",
    );
    assert!(
        compile_err("sub f(n) { return n } kind a { when f(free) == 1 => idle }")
            .contains("argument `n` of `f` must be an integer")
    );
}

#[test]
fn a_predicate_word_names_no_kind_or_tag() {
    for w in ["water", "soil", "rock", "bare"] {
        assert_eq!(
            compile_err(&format!("kind {w} {{ glyph \"w\" }}")),
            format!("t.rules:1:6: `{w}` is a predicate word, not a kind name")
        );
        assert_eq!(
            compile_err(&format!("kind a {{ glyph \"a\" tags meat {w} }}")),
            format!("t.rules:1:30: `{w}` is a predicate word, not a tag")
        );
    }
    // Still a need, a memory, a trait and a local.
    compile_ok(
        "trait water { need water max 1d vital }
             kind a extends water { mem soil  when true => { let rock = 1  soil = rock  idle } }",
    );
}

#[test]
fn a_pred_parameter_is_not_an_integer() {
    let want = "`p` is a predicate: use it in count, nearest, is or for each";
    let e = compile_err("sub f(p: pred) { return p * 2 } kind c { mem m  when true => m = f(c) }");
    assert!(e.contains(want), "{e}");
    let e = compile_err(
        "sub g(n) { return n } sub h(p: pred) { return g(p) }
             kind c { mem m  when true => m = h(c) }",
    );
    assert!(e.contains(want), "{e}");
}

#[test]
fn food_is_zero_or_more() {
    assert!(compile_err("kind a { food -2h }").contains("food is 0 or more ticks"));
    assert_eq!(compile_ok("kind a { food 0 }").defs[0].food, 0);
    assert_eq!(
        compile_ok("kind a { food 3h }").defs[0].food,
        hours(3) as i32
    );
}

/// `decay` is a switch, not a rate (RULES §3): `1` is the default, one
/// per tick; `0` is points; nothing else.
#[test]
fn decay_is_0_or_1() {
    let decays =
        |d: &str| compile_ok(&format!("kind a {{ need n max 9{d} }}")).defs[0].needs[0].decays;
    assert!(decays("") && decays(" decay 1") && !decays(" decay 0"));
    for d in ["2", "1h", "-1"] {
        let e = compile_err(&format!("kind a {{ need n max 9 decay {d} }}"));
        assert!(e.contains("0 (points) or 1 (per tick)"), "decay {d}: {e}");
    }
}

#[test]
fn a_tags_list_ends_at_the_next_declaration() {
    // `food` is not a keyword, but it starts a declaration: not a tag.
    let k = compile_ok("trait t(f) { tags meat food f }\nkind k extends t(3h) { glyph \"k\" }");
    let d = k.defs.iter().find(|d| d.name == "k").unwrap();
    assert_eq!((d.tags, d.food), (1, hours(3) as i32));
    let k = compile_ok("kind a { glyph \"a\" tags plant\n food 3h }");
    assert_eq!((k.defs[0].tags, k.defs[0].food), (1, hours(3) as i32));
    compile_ok("kind a { tags plant when true => idle }");
    compile_ok("kind a { tags plant sub f() { idle } state S { when true => f() } }");
    assert_eq!(
        compile_err("kind a { tags plant x }"),
        "t.rules:1:21: `x` is a reserved word, not a tag"
    );
}

#[test]
fn deep_nesting_and_long_chains_are_errors_not_stack_overflows() {
    let rule = |e: String| format!("kind a {{ mem m\n when {e} > 0 => idle }}");
    let deep = |text: String, want: &str| {
        let e = compile_err(&text);
        assert!(e.starts_with("t.rules:2:") && e.contains(want), "{e}");
    };
    deep(rule("(".repeat(5000)), "nested too deep");
    deep(rule("-".repeat(20000) + "m"), "nested too deep");
    deep(rule("not ".repeat(10000) + "m"), "nested too deep");
    // Nesting and chains share one budget, so one message names both.
    let both = "nested too deep (at most 128 levels; each operator in a chain counts as one)";
    deep(rule("1 + ".repeat(70000) + "m"), both);
    deep(rule("m > 0 and ".repeat(10000) + "m"), both);
    deep(rule("(".repeat(127) + "m" + &")".repeat(127)), both);
    deep(
        format!(
            "kind a {{ mem m\n when true => {}idle{} }}",
            "if m > 0 { ".repeat(127),
            " }".repeat(127)
        ),
        both,
    );
    // Parentheses that each hold a chain: the tree is as deep as the
    // chains added up, so a chain's operators count as levels.
    let mut e = "m".to_string();
    for j in 0..100 {
        e = format!("({e}{})", " + 1".repeat(j));
    }
    deep(rule(e), both);
    deep(
        format!("kind a {{\n when true => {}", "if true { ".repeat(1000)),
        "nested too deep",
    );
    deep(
        format!(
            "kind a {{\n when true => move {}here }}",
            "toward ".repeat(5000)
        ),
        "nested too deep",
    );
    // Real rules are nowhere near.
    compile_ok(&rule("(".repeat(100) + "m" + &")".repeat(100)));
    compile_ok(&rule("1 + ".repeat(100) + "m"));
}

#[test]
fn color_takes_six_hex_digits_and_nothing_else() {
    let k = compile_ok("kind a { glyph \"a\" color \"#a0b1c2\" when true => idle }");
    assert_eq!(k.defs[0].color, 0xa0b1c2);
    for bad in ["#+fffff", "#-fffff", "#fffff", "#fffffg"] {
        let text = format!("kind a {{ glyph \"a\" color \"{bad}\" when true => idle }}");
        assert!(
            compile_err(&text).contains("color takes \"#rrggbb\""),
            "{bad}"
        );
    }
}

#[test]
fn a_parenthesised_condition_reports_its_own_error() {
    // The error is the one that got further: the condition's, not the
    // expression re-parse's at `nearest`.
    assert_eq!(
        compile_err("kind a { when (nearest a within 3 as) => idle }"),
        "t.rules:1:37: expected a binding name, found `)`"
    );
    // Likewise a condition that parses but lacks its `)`.
    assert_eq!(
        compile_err("kind a { when (nearest a within 3 as b => idle }"),
        "t.rules:1:40: expected `)`, found `=>`"
    );
    // An expression error still wins when it is the further one.
    assert!(compile_err("kind a { when (1 + 2) > => idle }").contains("expected an expression"));
    compile_ok("kind a { glyph \"a\" when (x > 1) => idle }");
    compile_ok("kind a { glyph \"a\" when (x + 1) > 2 => idle }");
    compile_ok("kind a { glyph \"a\" when (x > 1 and y > 1) or x > 2 => idle }");
}

#[test]
fn conditions_short_circuit_and_bindings_scope_to_the_rule() {
    let k = compile_ok(
        "kind a { mem m\n when (m > 1 or m < -1) and not m == 0 => m = 0\n when nearest free within 1 as c and m > 0 => spawn a at c\n when 1 => if m > 5 { m = 5 } else if m < 0 { m = 0 } else { idle } }",
    );
    let c = &k.code;
    let ops: Vec<OpCode> = c.iter().map(|o| o.code).collect();
    let mut i = 0;
    let expect = |i: &mut usize, want: &[OpCode]| {
        assert_eq!(&ops[*i..*i + want.len()], want, "at op {i}");
        *i += want.len();
    };
    expect(
        &mut i,
        &[
            OpCode::Mem,
            OpCode::Push,
            OpCode::Gt,
            OpCode::Jz,
            OpCode::Jmp,
        ],
    );
    expect(&mut i, &[OpCode::Mem, OpCode::Push, OpCode::Lt, OpCode::Jz]);
    expect(
        &mut i,
        &[
            OpCode::Mem,
            OpCode::Push,
            OpCode::Eq,
            OpCode::Jz,
            OpCode::Jmp,
        ],
    );
    expect(&mut i, &[OpCode::Push, OpCode::SetMem, OpCode::EndRule]);
    expect(
        &mut i,
        &[OpCode::Push, OpCode::Push, OpCode::Nearest, OpCode::Jz],
    );
    expect(&mut i, &[OpCode::Mem, OpCode::Push, OpCode::Gt, OpCode::Jz]);
    expect(
        &mut i,
        &[
            OpCode::Push,
            OpCode::Load,
            OpCode::Load,
            OpCode::Act,
            OpCode::EndRule,
        ],
    );
    assert_eq!(c[i - 4], Op::new(OpCode::Load, 0, 0));
    assert_eq!(c[i - 3], Op::new(OpCode::Load, 1, 0));
    expect(&mut i, &[OpCode::Push, OpCode::Jz]);
    expect(
        &mut i,
        &[
            OpCode::Mem,
            OpCode::Push,
            OpCode::Gt,
            OpCode::Jz,
            OpCode::Push,
            OpCode::SetMem,
            OpCode::Jmp,
        ],
    );
    expect(
        &mut i,
        &[
            OpCode::Mem,
            OpCode::Push,
            OpCode::Lt,
            OpCode::Jz,
            OpCode::Push,
            OpCode::SetMem,
            OpCode::Jmp,
        ],
    );
    expect(&mut i, &[OpCode::Act, OpCode::EndRule, OpCode::Halt]);
    assert_eq!(i, ops.len());
    // The radius binds tighter than a comparison.
    let k = compile_ok("kind a { when count water within 2 > 0 => idle }");
    let ops: Vec<OpCode> = k.code.iter().map(|o| o.code).collect();
    assert_eq!(
        &ops[..5],
        &[
            OpCode::Push,
            OpCode::Push,
            OpCode::Count,
            OpCode::Push,
            OpCode::Gt
        ]
    );
}

#[test]
fn subs_targets_and_loops_compile_and_run() {
    use crate::actors::{ActorMind, ChunkActors};
    use crate::stage::ChunkCells;
    use bytemuck::Zeroable;
    let k = compile_ok(
            "sub twice(n) { return n * 2 }
             sub far(t: target) { return dist(t) > 3 }
             sub count_free(r) { let n = 0  let i = 0  while i < r { n += count free within i  i += 1 }  return n }
             sub go(t: target) { if free(toward t) { move toward t } else { move random free } }
             kind a { mem a, b, c, d, e, f
               when 1 => { a = twice(21)  b = far(at(x + 5, y))  c = count_free(2)  d = 0  repeat 4 { d += 3 } }
               when nearest water within 8 as w => { e = w.dx * 100 + w.dy  f = is(w, water) + dist(w) * 10 }
               when 1 => go(north) }",
        );
    assert_eq!(k.subs.len(), 4);
    let mut cells = ChunkCells::default();
    cells.ground[10 * 64 + 13] = Ground::Water; // 2 east of the actor at (11, 10)
    let actors = ChunkActors::default();
    let mut mind = ActorMind::zeroed();
    // Rule 1 has no action: falls through; rule 2 neither; rule 3 moves.
    let rng = crate::rules::vm::rng_base(1, 5, 9);
    let out = run_think_in(&k, "a", &mut mind, &cells, &actors, (11, 10), 5, rng);
    assert_eq!(out.trap, None, "{out:?}");
    assert_eq!(out.action, Action::Move);
    assert_eq!((out.dx, out.dy), (0, -1));
    // count_free(2): radius 0 is the 1x1 square (nobody stands in this
    // bare test's occupant array, so 1), radius 1 the 3x3 (9). Total 10.
    assert_eq!(&mind.mem[..6], &[42, 1, 10, 12, 200, 1 + 20]);
}

#[test]
fn states_consts_looks_signals_and_for_each_compile_and_run() {
    use crate::actors::{ActorMind, ActorPub, ChunkActors};
    use crate::stage::{ActorId, ChunkCells};
    use bytemuck::Zeroable;
    let k = compile_ok(
            "const LOAD = 30min
             const TWICE = LOAD * 2 + min(1, 2)
             sub tally(what: pred) { let n = 0  for each what within 3 as f { n += 1 + look_of(f) }  return n }
             kind a { mem s, n, m, p, h, l
               when s == 1 => { s = 2  next B }
               state A {
                 when true => { n = tally(a:3)  m = tally(a)  p = pack(-3, 5)  h = hi(p)  l = lo(p)
                                s = TWICE  signal = p  look = 4  next B }
               }
               state B {
                 when nearest a:3 within 2 as f => { s = signal_of(f)  idle }
                 when true => die
               } }",
        );
    assert_eq!(k.defs[0].states, 2);
    // Self at (10, 10); a:3 two east, a:0 one north-west, a:3 out of reach.
    let mut cells = ChunkCells::default();
    let mut actors = ChunkActors::default();
    for (slot, (x, y, look, signal)) in [
        (10, 10, 0, 0),
        (12, 10, 3, 77),
        (9, 9, 0, 0),
        (20, 20, 3, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let cell = y * 64 + x;
        cells.occupant[cell] = ActorId::pack(0, slot as u16);
        actors.rows.push(ActorPub {
            cell: cell as u16,
            look,
            signal,
            ..ActorPub::zeroed()
        });
    }
    let rng = crate::rules::vm::rng_base(1, 5, 9);
    let think = |m: &mut ActorMind| run_think_in(&k, "a", m, &cells, &actors, (10, 10), 5, rng);
    // State A (the first): the loops, the bytes, the effects, `next`.
    let mut mind = ActorMind::zeroed();
    let out = think(&mut mind);
    assert_eq!(out.trap, None, "{out:?}");
    assert_eq!(out.action, Action::Idle);
    assert_eq!(
        (out.next, out.look, out.signal),
        (Some(1), Some(4), Some(-763))
    );
    // tally(a:3) = 1 + 3; tally(a) = (1 + 3) + (1 + 0); pack/hi/lo round-trip.
    assert_eq!(&mind.mem[..6], &[901, 4, 5, -763, -3, 5]);
    // State B: reads the neighbour's signal through `a:3`.
    mind.state = 1;
    let out = think(&mut mind);
    assert_eq!((out.trap, out.action, out.next), (None, Action::Idle, None));
    assert_eq!(mind.mem[0], 77);
    // The reflex runs first, in any state.
    mind.mem[0] = 1;
    let out = think(&mut mind);
    assert_eq!((out.next, mind.mem[0]), (Some(1), 2));
    // Nothing to see: B falls to `die`.
    let out = run_think(&k, "a", &mut mind);
    assert_eq!(out.action, Action::Die);

    let errs = [
        (
            "kind a { state S { when true => next T } }",
            "has no state `T`",
        ),
        (
            "sub f() { next S }  kind a { state S { when true => f() } }",
            "`next` inside a sub",
        ),
        (
            "kind a { mem m  when true => m = 1 }  const C = m",
            "not a constant declared above",
        ),
        (
            "const C = 1  const C = 2",
            "const `C` declared twice (first at t.rules:1)",
        ),
        (
            "kind a { tags t  when nearest t:1 within 2 as v => idle }",
            "`t` is not a kind",
        ),
        (
            "kind a { state S { } state S { } }",
            "state `S` declared twice",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}: {e}");
    }
}

#[test]
fn a_return_takes_a_value_only_on_its_own_line() {
    // A bare `return` ends an arm; the next arm's weight is not its value.
    compile_ok(
        "sub f() { choose {\n 20: return\n 80: move north } }\n\
             kind a { when true => f() }",
    );
    // The next line's call is a statement: `g` stays a procedure.
    let e = compile_err(
        "sub h() { return 1 }\nsub g() { return\n h() }\n\
             kind a { mem m  when true => m = g() }",
    );
    assert!(e.contains("sub `g` returns nothing"), "{e}");
    compile_ok("sub g(v) { return v +\n 1 }\nkind a { mem m  when true => { m = g(2)  idle } }");
}

/// `;` is optional: after a statement, a rule's single-statement body, a
/// declaration and a `const`, once. Not after a block body or an item.
#[test]
fn a_semicolon_may_end_a_rule_a_declaration_and_a_const() {
    let with = compile_ok(
        "const C = 3;\n\
         kind a { glyph \"a\"; color \"#102030\"; cover; solid; tags t u; cadence 4; sight 2;\n\
         fuel 99; food 1h; bite 2; need h max C decay 0 vital; mem m, n;\n\
         when m > 0 => idle;\n\
         when true => { m = 1; move north; }\n\
         state S { when true => if m > 0 { idle }; } }",
    );
    let without = compile_ok(
        "const C = 3\n\
         kind a { glyph \"a\" color \"#102030\" cover solid tags t u cadence 4 sight 2\n\
         fuel 99 food 1h bite 2 need h max C decay 0 vital mem m, n\n\
         when m > 0 => idle\n\
         when true => { m = 1  move north }\n\
         state S { when true => if m > 0 { idle } } }",
    );
    assert_eq!(with.hash, without.hash);
    for (text, want) in [
        ("kind a { when true => idle;; }", "1:28:"),
        ("kind a { when true => { idle }; }", "1:31:"),
        ("kind a { mem m;; }", "1:16:"),
        ("const C = 1;;", "1:13:"),
        ("kind a { };", "1:11:"),
    ] {
        let e = compile_err(text);
        assert!(e.starts_with(&format!("t.rules:{want}")), "{text}: {e}");
    }
}

/// Comparisons don't chain: `a < b < c` would compare `a < b`, 0 or 1,
/// with `c`. Parentheses say that is meant.
#[test]
fn a_chain_of_comparisons_is_an_error() {
    let chain = "comparisons don't chain";
    for (text, at) in [
        ("kind a { mem m  when 0 < m < 9 => idle }", "1:28:"),
        ("kind a { mem m  when m == 1 != 0 => idle }", "1:29:"),
        (
            "kind a { mem m  when m >= 1 > 0 and m > 0 => idle }",
            "1:29:",
        ),
        ("kind a { mem m  when (0 < m <= 9) => idle }", "1:29:"),
        (
            "kind a { mem m  when true => { m = 1 < 2 == 1  idle } }",
            "1:42:",
        ),
        ("const C = 1 < 2 < 3", "1:17:"),
        ("kind a { mem m  when (m + 1) > 0 > 1 => idle }", "1:34:"),
    ] {
        let e = compile_err(text);
        assert!(
            e.starts_with(&format!("t.rules:{at}")) && e.contains(chain),
            "{text}: {e}"
        );
    }
    let e = compile_err("kind a { mem m  when 0 < m < 9 => idle }");
    assert!(e.contains("`and`") && e.contains("parenthes"), "{e}");
    compile_ok("kind a { mem m  when (0 < m) < 9 => idle }");
    compile_ok("kind a { mem m  when 0 < (m < 9) => idle }");
    compile_ok("kind a { mem m  when 0 < m and m < 9 => idle }");
    compile_ok("const C = (1 < 2) == 1\nkind a { mem m  when m == C => idle }");
}

/// Only a file sub is refused `next`: a member sub is its kind's own.
#[test]
fn a_member_sub_may_next() {
    use bytemuck::Zeroable;
    let k = compile_ok(
        "kind k { sub go() { next B }  when true => go()
               state A { when true => idle }  state B { when true => idle } }",
    );
    let mut mind = crate::actors::ActorMind::zeroed();
    let out = run_think(&k, "k", &mut mind);
    assert_eq!((out.trap, out.next), (None, Some(1)), "{out:?}");
}

#[test]
fn an_expression_deeper_than_the_vm_stack_is_an_error() {
    // Arguments wait on the stack while the next one is computed: each
    // level of `g(1, 1, 1, 1, 1, ...)` leaves five values waiting, and
    // costs only a few of the parser's 128 nesting levels.
    let g = "sub g(a, b, c, d, e, h) { return a + h }";
    let nest =
        |n: usize, leaf: &str| format!("{}{leaf}{}", "g(1, 1, 1, 1, 1, ".repeat(n), ")".repeat(n));
    let e = compile_err(&format!(
        "{g}\nkind k {{ mem m\n when true => m = {} }}",
        nest(13, "m")
    ));
    assert!(
        e.starts_with("t.rules:3:2:") && e.contains("stack values"),
        "{e}"
    );
    let e = compile_err(&format!(
        "{g}\nsub f(v) {{ return {} }}\nkind k {{ mem m\n when true => m = f(m) }}",
        nest(13, "v")
    ));
    assert!(
        e.contains("sub `f` needs") && e.contains("stack values"),
        "{e}"
    );

    use bytemuck::Zeroable;
    let k = compile_ok(&format!(
        "{g}\nkind k {{ mem m\n when true => m = {} }}",
        nest(9, "m")
    ));
    let mut mind = crate::actors::ActorMind::zeroed();
    for _ in 0..2 {
        assert_eq!(run_think(&k, "k", &mut mind).trap, None);
    }
    assert_eq!(mind.mem[0], 18);
}

#[test]
fn more_than_65534_kinds_or_65536_subs_is_an_error() {
    let mut text = String::new();
    for i in 0..65_535 {
        text.push_str(&format!("kind k{i} {{ }}\n"));
    }
    let e = compile_err(&text);
    assert!(
        e.starts_with("t.rules:65535:") && e.contains("at most 65534 kinds"),
        "{e}"
    );

    let mut text = String::new();
    for i in 0..65_537 {
        text.push_str(&format!("sub s{i}() {{ return {i} }}\n"));
    }
    text.push_str("kind k { mem v\n when true => v = s65536() }\n");
    let e = compile_err(&text);
    assert!(
        e.starts_with("t.rules:65537:") && e.contains("at most 65536 subs"),
        "{e}"
    );
}

#[test]
fn more_than_65536_rules_files_is_an_error() {
    // A rule's and a kind's file is a u16 index: file 65536 was file 0.
    let names: Vec<String> = (0..=65_536).map(|i| format!("f{i}.rules")).collect();
    let mut files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "")).collect();
    files[65_536].1 = "kind k { mem v\n when true => v = 1 }\n";
    let e = compile_files(&files).unwrap_err().to_string();
    assert!(
        e.starts_with("f65536.rules:1:1:") && e.contains("at most 65536 rules files"),
        "{e}"
    );
    files.remove(0);
    let k = compile_files(&files).unwrap_or_else(|e| panic!("{e}"));
    let file = |i: u16| k.debug.files[usize::from(i)].as_str();
    assert_eq!(file(k.debug.rules[0].file), "f65536.rules");
    assert_eq!(file(k.debug.kind_at[0].0), "f65536.rules");
}

#[test]
fn a_choose_with_too_many_arms_is_refused() {
    compile_ok(&format!(
        "kind a {{ mem v  when true => choose {{ {} }} }}",
        "1: v = 0 ".repeat(15)
    ));
    for (arms, before) in [(16, ""), (255, "let a = 1  "), (255, ""), (256, "")] {
        let text = format!(
            "kind a {{ mem v  when true => {{ {before}choose {{ {} }} }} }}",
            "1: v = 0 ".repeat(arms)
        );
        let e = compile_err(&text);
        assert!(e.contains("too many"), "{arms} arms: {e}");
    }
}

/// RULES §7: a `let` lives until the end of its block.
#[test]
fn a_let_ends_with_its_block() {
    compile_ok("kind a { mem m\n when 1 => { let a = 3  if m == 0 { m = a } } }");
    let e = compile_err("kind a { mem m\n when 1 => { if m == 0 { let a = 3 }  m = a } }");
    assert!(e.contains("unknown name `a`"), "{e}");
}

/// RULES §18: `toward`, `away` and `random free` take 2 slots while
/// evaluated, on top of the rule's bindings.
#[test]
fn toward_takes_two_slots() {
    let binds = (0..8)
        .map(|i| format!("nearest free within 1 as t{i}"))
        .collect::<Vec<_>>()
        .join(" and ");
    compile_ok(&format!("kind a {{ when {binds} => move t0 }}"));
    for t in ["toward t0", "away t0", "random free"] {
        let e = compile_err(&format!("kind a {{ when {binds} => move {t} }}"));
        assert!(e.contains("too many bindings"), "{t}: {e}");
    }
}

#[test]
fn choose_draws_once_and_large_constants_use_the_pool() {
    let k = compile_ok(
        "kind a { mem m\n when 1 => choose { 3: m = 1  2: m = 2 }\n when m > 100000 => m = 3d }",
    );
    // The first is the weight cap of the two-armed choose.
    assert_eq!(k.consts, vec![i32::MAX / 2, 100_000, days(3) as i32]);
    let rand = k.code.iter().filter(|o| o.code == OpCode::Rand).count();
    assert_eq!(rand, 1);
    use crate::actors::ChunkActors;
    use crate::rules::vm;
    use crate::stage::ChunkCells;
    use bytemuck::Zeroable;
    let cells = ChunkCells::default();
    let actors = ChunkActors::default();
    let mut counts = [0; 3];
    for uid in 0..500u64 {
        let mut mind = crate::actors::ActorMind::zeroed();
        let rng = vm::rng_base(3, 77, uid);
        let out = run_think_in(&k, "a", &mut mind, &cells, &actors, (36, 1), 77, rng);
        assert_eq!(out.trap, None);
        counts[mind.mem[0] as usize] += 1;
    }
    assert_eq!(counts[0], 0);
    assert!(counts[1] > 240 && counts[1] < 360, "{counts:?}");
    assert!(counts[2] > 140 && counts[2] < 260, "{counts:?}");
}

#[test]
fn code_too_long_for_a_16_bit_jump_is_an_error() {
    let mut text = String::from("kind k { mem m\n state s {\n");
    for i in 0..5000 {
        text.push_str(&format!("  when m == {i} => look = 1\n"));
    }
    text.push_str(" }\n}\n");
    let e = compile_err(&text);
    assert!(e.contains("state `s` of `k`") && e.contains("32767"), "{e}");

    let mut text = String::from("kind k { mem m\n when true => {\n");
    for _ in 0..9000 {
        text.push_str("  m += 1\n");
    }
    text.push_str(" }\n}\n");
    let e = compile_err(&text);
    assert!(
        e.starts_with("t.rules:2:2:") && e.contains("rule body too long"),
        "{e}"
    );

    let mut text = String::from("sub f() {\n let a = 0\n");
    for _ in 0..9000 {
        text.push_str(" a += 1\n");
    }
    text.push_str("}\nkind k { when true => f() }\n");
    let e = compile_err(&text);
    assert!(
        e.starts_with("t.rules:1:5:") && e.contains("sub `f`"),
        "{e}"
    );
}

#[test]
fn more_than_65536_large_constants_is_an_error() {
    let mut text = String::from("kind k { mem m\n");
    let mut c = 40_000;
    for _ in 0..5 {
        text.push_str(" when true => {\n");
        for _ in 0..14_000 {
            text.push_str(&format!("  m = {c}\n"));
            c += 1;
        }
        text.push_str(" }\n");
    }
    text.push_str("}\n");
    let e = compile_err(&text);
    assert!(e.contains("too many distinct constants"), "{e}");
}

#[test]
fn choose_weights_near_i32_max_do_not_wrap_the_total() {
    let k = compile_ok(
        "kind a { mem p, q\n when true => { choose { 2000000000: p += 1  2000000000: q += 1 }  idle } }",
    );
    use crate::actors::ChunkActors;
    use crate::rules::vm;
    use crate::stage::ChunkCells;
    use bytemuck::Zeroable;
    let cells = ChunkCells::default();
    let actors = ChunkActors::default();
    let mut mind = crate::actors::ActorMind::zeroed();
    for uid in 0..64u64 {
        let rng = vm::rng_base(3, 5, uid);
        let out = run_think_in(&k, "a", &mut mind, &cells, &actors, (36, 1), 5, rng);
        assert_eq!(out.trap, None);
    }
    let (p, q) = (mind.mem[0], mind.mem[1]);
    assert!(p > 0 && q > 0, "p {p} q {q}");
}

/// Lines of kind `k`'s rules in scan order, with the trait or parent
/// each came from.
fn rule_lines(k: &Kinds, kind: &str) -> Vec<(Option<u8>, u32, Option<String>)> {
    let id = k.by_name(kind).unwrap().id;
    k.debug
        .rules
        .iter()
        .filter(|r| r.kind == id)
        .map(|r| (r.state, r.line, r.via.clone()))
        .collect()
}

/// An `idle` the compiler cannot see next to a later action (behind an
/// `if`) is still the think's one action: the move traps.
#[test]
fn a_conditional_idle_then_an_action_traps() {
    use bytemuck::Zeroable;
    let k = compile_ok("kind k { mem go  when true => { if go == 0 { idle }  move east } }");
    let mut m = crate::actors::ActorMind::zeroed();
    let out = run_think(&k, "k", &mut m);
    assert_eq!(out.trap, Some(crate::rules::vm::Trap::SecondAction));
    assert_eq!(out.action, Action::Idle);
}

/// A constant folds to what the VM computes for the same expression
/// (lets are not folded), for every operator and pure function on
/// edge operands.
#[test]
fn a_count_radius_is_one_term() {
    let code = |cond: &str| {
        compile_ok(&format!(
            "const R = 3 kind a {{ sight 8  when {cond} => idle }}"
        ))
        .code
    };
    // An operator after the radius applies to the count.
    assert_eq!(
        code("count a within 3 - 1 > 2"),
        code("(count a within 3) - 1 > 2")
    );
    assert_eq!(
        code("count a within R + count a within 1 > 0"),
        code("(count a within R) + (count a within 1) > 0")
    );
    assert_eq!(
        code("count a within -R * 2 > 0"),
        code("(count a within -R) * 2 > 0")
    );
    // Radius arithmetic takes parentheses.
    assert_ne!(
        code("count a within (3 - 1) > 2"),
        code("(count a within 3) - 1 > 2")
    );
}

#[test]
fn folded_constants_match_the_vm() {
    use bytemuck::Zeroable;
    let lit = |v: i32| {
        if v == i32::MIN {
            "(-2147483647 - 1)".to_string()
        } else {
            format!("({v})")
        }
    };
    let vals = [0, 1, -1, 7, -300, i32::MIN, i32::MAX];
    let mut cases: Vec<(String, Vec<i32>)> = Vec::new();
    for op in ["+", "-", "*", "/", "%", "<", "<=", "==", "!=", ">=", ">"] {
        for x in vals {
            for y in vals {
                cases.push((format!("$0 {op} $1"), vec![x, y]));
            }
        }
    }
    for f in ["abs", "sign", "hi", "lo"] {
        for x in vals {
            cases.push((format!("{f}($0)"), vec![x]));
        }
    }
    for f in ["min", "max", "pack"] {
        for x in vals {
            for y in vals {
                cases.push((format!("{f}($0, $1)"), vec![x, y]));
            }
        }
    }
    for x in vals {
        for y in vals {
            for z in vals {
                cases.push(("clamp($0, $1, $2)".to_string(), vec![x, y, z]));
            }
        }
    }
    for (e, args) in cases {
        let (mut folded, mut run, mut lets) = (e.clone(), e.clone(), String::new());
        for (i, &v) in args.iter().enumerate() {
            folded = folded.replace(&format!("${i}"), &lit(v));
            run = run.replace(&format!("${i}"), &format!("v{i}"));
            lets += &format!("let v{i} = {}  ", lit(v));
        }
        let k = compile_ok(&format!(
            "const C = {folded}
                 kind k {{ mem m, n  when true => {{ {lets} m = C  n = {run}  idle }} }}"
        ));
        let mut mind = crate::actors::ActorMind::zeroed();
        let out = run_think(&k, "k", &mut mind);
        assert_eq!(out.trap, None, "{e} with {args:?}");
        assert_eq!(mind.mem[0], mind.mem[1], "{e} with {args:?}: fold vs VM");
    }
}

/// `spawn K ... with` names the memory it sets. Those names take K's
/// first slots, where a spawn's two values land, whatever K inherits.
#[test]
fn spawn_with_names_the_memory_it_sets() {
    use bytemuck::Zeroable;
    let k = compile_ok(
        "trait walker { mem heading, detour }
             kind bee extends walker { mem trip, home_x, home_y  when true => idle }
             kind hive { when true => spawn bee at random free with (home_y = 5, home_x = x) }
             kind queen { when true => spawn bee at random free with (home_x = 9) }",
    );
    let bee = k.by_name("bee").unwrap();
    assert_eq!(bee.mems, ["home_y", "home_x", "heading", "detour", "trip"]);
    let mut m = crate::actors::ActorMind::zeroed();
    let out = run_think(&k, "hive", &mut m);
    assert_eq!((out.action, out.with), (Action::Spawn, [5, 36]));
    let out = run_think(&k, "queen", &mut m);
    assert_eq!(out.with, [0, 9], "home_x is slot 1; home_y stays zero");
    for (text, want) in [
        (
            "kind b { } kind h { when true => spawn b at random free with (7, x) }",
            "`with` names the memory it sets: `with (home_x = x, home_y = y)`",
        ),
        (
            "kind b { mem m } kind h { when true => spawn b at random free with (n = 1) }",
            "`spawn b ... with`: `b` has no memory `n`",
        ),
        (
            "kind b { mem m } kind h { when true => spawn b at random free with (m = 1, m = 2) }",
            "`with` sets `m` twice",
        ),
        (
            "kind b { mem p, q, r }
                 kind h { when true => spawn b at random free with (p = 1, q = 2) }
                 kind g { when true => spawn b at random free with (r = 3) }",
            "spawns of `b` set three memory slots with `with` (p, q, r): at most two per kind",
        ),
        (
            "kind b { mem p, q, r } kind h { when true => spawn b at random free with (p = 1, q = 2, r = 3) }",
            "`with` sets at most two memory slots",
        ),
    ] {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}: {e}");
    }
}

/// `solid` is drawn, not simulated: it comes from a trait or the parent
/// like `cover`, is in the kind table for the renderer, and changes the hash
/// only of rules that use it.
#[test]
fn solid_is_inherited_and_only_drawn() {
    let k = compile_ok(
        "trait paint { solid }
         kind ink extends paint { cover }
         kind red extends ink { color \"#ff0000\" }
         kind plain { }",
    );
    let solid: Vec<(&str, bool)> = k.defs.iter().map(|d| (d.name.as_str(), d.solid)).collect();
    assert_eq!(solid, [("ink", true), ("red", true), ("plain", false)]);
    assert_eq!(k.solids, [true, true, false]);
    let without = compile_ok("kind plain { }");
    let with = compile_ok("kind plain { solid }");
    assert_ne!(with.hash, without.hash);
    assert_eq!(without.hash, compile_ok("kind plain { }").hash);
}

#[test]
fn traits_merge_declarations_in_linearized_order() {
    let k = compile_ok(
        "trait mover { cadence 2  sight 6  tags animal  need food max 1d vital  mem heading }
             trait drinker(t) { tags thirsty  need water max t vital  mem knows_water }
             kind hen extends mover, drinker(4h) {
               glyph \"h\"  sight 8  need health max 20 decay 0 vital  mem last_egg }
             kind chick extends hen { glyph \"c\"  need water max 2h vital }",
    );
    let hen = k.by_name("hen").unwrap();
    assert_eq!((hen.glyph, hen.cadence_shift, hen.sight), (b'h', 1, 8));
    let needs: Vec<(&str, i32)> = hen.needs.iter().map(|n| (n.name.as_str(), n.max)).collect();
    assert_eq!(needs, [("food", 21600), ("water", 3600), ("health", 20)]);
    assert_eq!(hen.mems, ["heading", "knows_water", "last_egg"]);
    assert_eq!(hen.tags, 0b11);
    assert_eq!(hen.parent, None);
    // The chick keeps every slot where the hen has it; its water is its own.
    let chick = k.by_name("chick").unwrap();
    let needs: Vec<(&str, i32)> = chick
        .needs
        .iter()
        .map(|n| (n.name.as_str(), n.max))
        .collect();
    assert_eq!(needs, [("food", 21600), ("water", 1800), ("health", 20)]);
    assert_eq!((chick.glyph, chick.sight, chick.tags), (b'c', 8, 0b11));
    assert_eq!(chick.parent, Some(hen.id));
    assert_eq!(k.debug.traits, ["mover", "drinker"]);
    assert_eq!(
        k.debug.parents[usize::from(hen.id)],
        ["mover", "drinker(3600)"]
    );
    // A parent's tables change nothing for kinds without parents.
    assert_eq!(k.family_end[usize::from(hen.id)], chick.id + 1);
}

#[test]
fn inherit_splices_where_written_and_appends_when_absent() {
    let k = compile_ok(
        "trait t1 { when hour == 1 => idle }
             trait t2 { when hour == 2 => idle }
             kind a extends t1, t2 {
               when hour == 3 => idle
               inherit t2
               when hour == 4 => idle
             }
             kind b extends t1, t2 { when hour == 5 => idle }
             kind c extends a { when hour == 6 => idle  inherit }",
    );
    let t = |v: Option<&str>| v.map(str::to_string);
    assert_eq!(
        rule_lines(&k, "a"),
        [(None, 4, None), (None, 2, t(Some("t2"))), (None, 6, None)]
    );
    assert_eq!(
        rule_lines(&k, "b"),
        [
            (None, 8, None),
            (None, 1, t(Some("t1"))),
            (None, 2, t(Some("t2")))
        ]
    );
    // `inherit` alone splices the parent's list as the parent runs it:
    // without t1, which `a` left out.
    assert_eq!(
        rule_lines(&k, "c"),
        [
            (None, 9, None),
            (None, 4, t(Some("a"))),
            (None, 2, t(Some("t2"))),
            (None, 6, t(Some("a")))
        ]
    );
    let notes: Vec<&str> = k
        .debug
        .diagnostics
        .iter()
        .filter(|d| d.level == Level::Note)
        .map(|d| d.msg.as_str())
        .collect();
    assert_eq!(
        notes,
        [
            "`a` does not run the reflex rules of `t1` (no `inherit` splices them)",
            "`c` does not run the reflex rules of `t1` (no `inherit` splices them)"
        ]
    );
}

/// `inherit NAME` splices that ancestor's list as it runs it, with what
/// the ancestor itself inherits, and each rule once.
#[test]
fn inherit_name_splices_the_ancestors_resolved_list() {
    let k = compile_ok(
        "trait t { when hour == 1 => idle }
             kind p extends t { when hour == 2 => idle }
             kind k extends p {
               when hour == 3 => idle
               inherit p
               when true => idle
             }
             kind q extends p { inherit t  inherit p }",
    );
    let t = |v: &str| Some(v.to_string());
    assert_eq!(
        rule_lines(&k, "k"),
        [
            (None, 4, None),
            (None, 2, t("p")),
            (None, 1, t("t")),
            (None, 6, None)
        ]
    );
    assert_eq!(rule_lines(&k, "q"), [(None, 1, t("t")), (None, 2, t("p"))]);
}

#[test]
fn state_blocks_merge_by_name() {
    let k = compile_ok(
        "trait walker {
               state WALK { when hour == 1 => idle }
               state REST { when hour == 2 => idle }
             }
             kind k extends walker {
               state REST { when hour == 3 => idle  inherit }
               state EAT { when hour == 4 => next WALK }
             }",
    );
    let id = usize::from(k.by_name("k").unwrap().id);
    assert_eq!(k.debug.states[id], ["WALK", "REST", "EAT"]);
    assert_eq!(k.defs[id].states, 3);
    let w = |v: &str| Some(v.to_string());
    assert_eq!(
        rule_lines(&k, "k"),
        [
            (Some(0), 2, w("walker")),
            (Some(1), 6, None),
            (Some(1), 3, w("walker")),
            (Some(2), 7, None)
        ]
    );
    // `next WALK` in the kind's own state is state 0.
    let next = k.code.iter().find(|o| o.code == OpCode::Next).unwrap();
    assert_eq!(next.a, 0);
    // Parents' states first, whatever order the kind writes them in:
    // it starts in WALK.
    let k = compile_ok(
        "trait walker { state WALK { }  state REST { } }
             kind k extends walker { state EAT { }  state REST { }  state WALK { } }",
    );
    let id = usize::from(k.by_name("k").unwrap().id);
    assert_eq!(k.debug.states[id], ["WALK", "REST", "EAT"]);
}

#[test]
fn trait_parameters_fold_per_instantiation() {
    use crate::actors::ActorMind;
    use bytemuck::Zeroable;
    let k = compile_ok(
        "const HOUR = 1h
             trait thirsty(t) { need water max t * 2 vital  when water < t => water = t }
             kind a extends thirsty(2 * HOUR) { }
             kind b extends thirsty(6h) { }",
    );
    assert_eq!(k.by_name("a").unwrap().needs[0].max, 3600);
    assert_eq!(k.by_name("b").unwrap().needs[0].max, 10800);
    for (kind, want) in [("a", 1800), ("b", 5400)] {
        let mut m = ActorMind::zeroed();
        let out = run_think(&k, kind, &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(m.needs[0], want, "{kind}");
    }
    assert!(
        compile_err("trait t(v) { when 1 => v = 2 } kind a extends t(1) { }")
            .contains("cannot assign to `v`: it is a constant")
    );
}

#[test]
fn the_trait_check_does_not_invent_a_diamond_from_its_placeholder_arguments() {
    // Checked on its own, `t` gets n = 1 and reaches u(1) and u(2);
    // its only real use, t(2), reaches u(2) once.
    let pack = "trait u(a) { mem m }
                    trait v extends u(2) { }
                    trait t(n) extends u(n), v { }";
    compile_ok(&format!(
        "{pack} kind k extends t(2) {{ when true => idle }}"
    ));
    compile_ok(pack);
    // A kind that reaches u(1) and u(2) for real is still an error.
    let e = compile_err(&format!("{pack} kind k extends u(1), v {{ }}"));
    assert!(
        e.contains("`k` reaches trait `u` twice, with (1) and (2)"),
        "{e}"
    );
    let e = compile_err(&format!("{pack} kind k extends t(1) {{ }}"));
    assert!(
        e.contains("`t` reaches trait `u` twice, with (1) and (2)"),
        "{e}"
    );
    // So is a trait with no arguments, used or not.
    let e = compile_err(&format!("{pack} trait w extends u(1), v {{ }}"));
    assert!(
        e.contains("`w` reaches trait `u` twice, with (1) and (2)"),
        "{e}"
    );
    // And a trait that names t(1) itself, at any depth, before or after t.
    for text in [
        format!("{pack} trait w extends t(1) {{ }}"),
        format!("{pack} trait w(k) extends t(1) {{ }}"),
        format!("{pack} trait top extends w(3) {{ }} trait w(k) extends t(1) {{ }}"),
        format!("trait w extends t(1) {{ }} {pack}"),
        format!("{pack} trait w(k) extends t(k) {{ }} trait top extends w(1) {{ }}"),
        format!("{pack} trait top extends w(1) {{ }} trait w(k) extends t(k) {{ }}"),
        format!("{pack} trait w(k) extends t(k) {{ }} kind top extends w(1) {{ }}"),
    ] {
        let e = compile_err(&text);
        assert!(e.contains("`t` reaches trait `u` twice"), "{text}: {e}");
    }
    // A trait that passes its own parameter on is checked with 1s too.
    compile_ok(&format!("trait w(k) extends t(k) {{ }} {pack}"));
    compile_ok(&format!("{pack} trait w(k) extends t(k + 0) {{ }}"));
}

#[test]
fn member_subs_see_their_kinds_needs_and_compile_per_kind() {
    use crate::actors::ActorMind;
    use bytemuck::Zeroable;
    let k = compile_ok(
        "trait eater { need food max 1d vital }
             trait sipper {
               need water max 4h vital
               sub sip(n) { water = water + n }
               when water < 1h => { sip(30min)  idle }
             }
             kind a extends eater, sipper { }
             kind b extends sipper { }
             kind c extends sipper { sub sip(n) { water = 3h } }",
    );
    // Water is slot 1 in `a`, slot 0 in `b` and `c`: one sub, compiled per kind.
    assert_eq!(k.by_name("a").unwrap().need_named("water"), Some(1));
    assert_eq!(k.by_name("b").unwrap().need_named("water"), Some(0));
    for (kind, slot, want) in [("a", 1, 450), ("b", 0, 450), ("c", 0, 2700)] {
        let mut m = ActorMind::zeroed();
        m.needs = [100; 4];
        m.needs[slot] = 0;
        let out = run_think(&k, kind, &mut m);
        assert_eq!(out.trap, None, "{kind}");
        assert_eq!(m.needs[slot], want, "{kind}");
    }
    assert!(k.debug.subs.contains(&"a::sip".to_string()));
    assert!(k.debug.subs.contains(&"c::sip".to_string()));
}

#[test]
fn family_numbering_is_preorder_by_declaration() {
    let k = compile_ok(
        "kind hen extends bird { }
             kind animal { }
             kind plant { }
             kind bird extends animal { }
             kind fox extends animal { }
             kind tree extends plant { }",
    );
    assert_eq!(
        k.names().collect::<Vec<_>>(),
        ["animal", "bird", "hen", "fox", "plant", "tree"]
    );
    assert_eq!(k.family_end, [4, 3, 3, 4, 6, 6]);
    let parents: Vec<Option<u16>> = k.defs.iter().map(|d| d.parent).collect();
    assert_eq!(parents, [None, Some(0), Some(1), Some(0), None, Some(4)]);
    // `only` and families in predicates.
    let k = compile_ok(
        "kind animal { }
             kind bird extends animal {
               when nearest animal within 1 as a => idle
               when nearest only animal within 1 as a => idle
               when nearest bird:2 within 1 as a => idle
               when nearest only bird:2 within 1 as a => idle
             }",
    );
    let pushes: Vec<i32> = k
        .code
        .windows(3)
        .filter(|w| w[2].code == OpCode::Nearest)
        .map(|w| match w[0].code {
            OpCode::PushK => k.consts[w[0].imm as u16 as usize],
            _ => i32::from(w[0].imm),
        })
        .collect();
    assert_eq!(
        pushes,
        [
            0,
            pred::ONLY,
            pred::kind_look(1, 2),
            pred::kind_look(1, 2) + pred::ONLY
        ]
    );
}

#[test]
fn extends_errors_name_the_problem() {
    let errs = [
        ("kind a extends zz { }", "unknown trait or kind `zz`"),
        (
            "kind a extends b { } kind b extends a { }",
            "extends itself: a -> b -> a",
        ),
        (
            "trait a extends b { } trait b extends a { }",
            "extends itself",
        ),
        (
            "kind a { } kind b { } kind c extends a, b { }",
            "extends two kinds, `a` and `b`",
        ),
        (
            "kind a { } trait t extends a { }",
            "trait `t` can extend only traits",
        ),
        (
            "trait t(v) { } kind a extends t { }",
            "takes 1 argument, 0 given",
        ),
        (
            "kind a { } kind b extends a(1) { }",
            "kind `a` takes no arguments",
        ),
        (
            "trait t(v) { } trait u extends t(1) { } kind a extends u, t(2) { }",
            "reaches trait `t` twice, with (1) and (2)",
        ),
        (
            "trait t { } kind a { inherit t }",
            "`t` is not an ancestor of `a`",
        ),
        (
            "trait t { when true => idle } kind a extends t { inherit t  inherit t }",
            "`inherit t` twice",
        ),
        (
            "trait t { } kind a extends t { state S { inherit t } }",
            "`t` has no state `S`",
        ),
        ("trait t { glyph \"x\" }", "a trait has no glyph"),
        ("trait t { color \"#ffffff\" }", "a trait has no colour"),
        ("kind a(v) { }", "a kind takes no parameters"),
        (
            "trait t { when food < 1 => idle } kind a extends t { need food max 1d }",
            "`t` uses `food`, which it does not declare",
        ),
        ("trait t { when zz > 1 => idle }", "unknown name `zz`"),
        (
            "trait t { } kind a { when nearest t within 1 as v => idle }",
            "`t` is a trait",
        ),
        (
            "trait t { } kind a { when 1 => spawn t at north }",
            "only a kind can be spawned",
        ),
        (
            "kind a { tags z  when nearest only z within 1 as v => idle }",
            "`only` applies to a kind",
        ),
        (
            "kind a { when nearest only water within 1 as v => idle }",
            "`only` applies to a kind",
        ),
        (
            "trait t { when 1 => f() } kind a extends t { sub f() { idle } }",
            "`t` calls `f`, which it does not define",
        ),
        (
            "sub f() { } kind a { sub f() { } }",
            "has the name of a file sub",
        ),
        ("const X = 1  trait t(X) { }", "hides the constant `X`"),
        (
            "trait p { need w max 1h } trait q { need w max 2h } kind a extends p, q { }",
            "inherits need `w` from `p` and `q`",
        ),
        (
            "trait p { cadence 2 } trait q { cadence 4 } kind a extends p, q { }",
            "inherits different cadences from `p` and `q`",
        ),
        (
            "trait p { sub f() { } } trait q { sub f() { } } kind a extends p, q { }",
            "inherits sub `f` from both `p` and `q`",
        ),
        (
            "trait t { when 1 => next S } kind a extends t { state S { } }",
            "trait `t` has no state `S`",
        ),
        ("trait t { } kind t { }", "kind `t` declared twice"),
        (
            "trait t { need a max 1h need b max 1h need c max 1h need d max 1h need e max 1h }",
            "has 5 needs",
        ),
        (
            "kind a { when 1 => idle  sub f() { } }",
            "member subs and searches come before the rules",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    // A redeclaration settles parents that disagree.
    compile_ok(
        "trait p { need w max 1h } trait q { need w max 2h } kind a extends p, q { need w max 3h }",
    );
    compile_ok("trait p { cadence 2 } trait q { cadence 4 } kind a extends p, q { cadence 8 }");
}

/// A chain of `extends` is resolved by recursion, one level per link: the
/// parser's nesting limit does not bound it, so its own limit does. 3000
/// traits in a chain overflowed the stack, and 3000 kinds took most of a
/// minute (each resolves its whole ancestry).
#[test]
fn a_chain_of_extends_is_at_most_128_long() {
    let chain = |word: &str, n: usize| {
        let mut t = format!("{word} x0 {{ }}\n");
        for i in 1..n {
            t += &format!("{word} x{i} extends x{} {{ }}\n", i - 1);
        }
        t
    };
    compile_ok(&(chain("trait", 128) + "kind k extends x127 { }"));
    compile_ok(&chain("kind", 129));
    let err = compile_err(&(chain("trait", 3000) + "kind k extends x2999 { }"));
    assert!(err.contains("more than 128 kinds and traits deep"), "{err}");
    let err = compile_err(&chain("kind", 3000));
    assert!(err.contains("more than 128 kinds and traits deep"), "{err}");
    // Written child first, a kind chain is one deep recursion too.
    let mut t = String::new();
    for i in (1..3000).rev() {
        t += &format!("kind x{i} extends x{} {{ }}\n", i - 1);
    }
    t += "kind x0 { }";
    assert!(compile_err(&t).contains("more than 128 kinds and traits deep"));
}

#[test]
fn a_constant_that_is_not_one_is_reported_where_it_stands() {
    let kind = "kind a { glyph \"a\" when true => idle }";
    for (text, line) in [
        (format!("const C = rand(3)\n{kind}"), 1),
        (format!("{kind}\ntrait t {{ sight age }}"), 2),
        (format!("{kind}\nkind b {{\n need thirst max hour }}"), 3),
        (
            format!("{kind}\ntrait t(n) {{ }}\nkind k extends t(age) {{ }}"),
            3,
        ),
    ] {
        let e = compile_err(&text);
        assert!(e.starts_with(&format!("t.rules:{line}:")), "{text}: {e}");
        assert!(e.contains("must be a number"), "{text}: {e}");
    }
}

#[test]
fn straight_line_second_action_is_an_error() {
    for text in [
        "kind a { when 1 => { idle  move north } }",
        "kind a { when 1 => { if hour > 1 { idle } else { die }  move north } }",
        "sub f() { idle } kind a { when 1 => { f()  move north } }",
        "kind a { when 1 => { choose { 1: idle  2: die }  move north } }",
    ] {
        let e = compile_err(text);
        assert!(e.contains("a second action"), "{text}: {e}");
    }
    let e = compile_err("kind a {\n when 1 => {\n idle\n move north } }");
    assert!(e.starts_with("t.rules:4:2:"), "{e}");
    assert!(e.contains("already acted at line 3"), "{e}");
    // Conservative: a branch that may not act, or a `next`, is fine.
    compile_ok("kind a { when 1 => { if hour > 1 { idle }  move north } }");
    compile_ok("kind a { when 1 => { next S  move north } state S { } }");
    compile_ok("kind a { when 1 => { choose { 1: idle  0: look = 1 }  move north } }");
    // A sub that may return before it acts.
    compile_ok(
        "sub maybe(h) { if h == 0 { return }  move dir(h) }\n\
             kind k { mem heading  when true => { maybe(heading)  move random free } }",
    );
    compile_ok(
        "sub f(c) { if c { return; move east } else { move west } }\n\
             kind k { when true => { f(hour)  move north } }",
    );
    // A return inside a sub it calls leaves only that sub.
    let e = compile_err(
        "sub g() { return }\nsub f() { g()  move east }\n\
             kind k { when true => { f()  move north } }",
    );
    assert!(e.contains("a second action"), "{e}");
}

#[test]
fn a_sub_called_in_a_when_condition_must_not_act() {
    let step = "sub step() { move north  return 1 }\n";
    for (text, sub) in [
        ("kind a { when step() > 0 => idle }", "step"),
        ("kind a { when true and not (step() == 1) => idle }", "step"),
        (
            "kind a { when nearest a within step() as f => idle }",
            "step",
        ),
        ("kind a { when is(dir(step()), free) => idle }", "step"),
        // Through calls, in statements and expressions, and inside
        // if, choose and loops.
        (
            "sub p() { let v = step()  return v }\nkind a { when p() => idle }",
            "p",
        ),
        (
            "sub h() { choose { 1: idle  1: look = 1 } }\n\
                 sub p() { repeat 2 { if hour > 1 { h() } }  return 1 }\n\
                 kind a { when 1 + p() > 0 => idle }",
            "p",
        ),
        (
            "kind a { sub g() { if hour > 1 { next S }  return 1 }\n\
                 when g() => idle  state S { } }",
            "g",
        ),
    ] {
        let e = compile_err(&format!("{step}{text}"));
        assert!(
            e.contains(&format!(
                "`{sub}` may act or `next`: a sub called in a `when` condition must not"
            )),
            "{text}: {e}"
        );
    }
    let e = compile_err(&format!("{step}kind a {{\n when hour > step() => idle }}"));
    assert!(e.starts_with("t.rules:3:14:"), "{e}");
    // A pure sub is fine, and an acting sub still works in a body.
    compile_ok("sub pure() { return hour + 1 }\nkind a { when pure() > 0 => idle }");
    compile_ok(&format!(
        "{step}kind a {{ mem v  when true => {{ v = step() }} }}"
    ));
    compile_ok(&format!(
        "{step}kind a {{ when true => {{ if step() > 0 {{ look = 1 }} }} }}"
    ));
}

#[test]
fn a_declaration_or_a_next_given_twice_is_an_error() {
    for (w, decl) in [
        ("glyph", "glyph \"b\""),
        ("color", "color \"#000000\""),
        ("cover", "cover"),
        ("solid", "solid"),
        ("cadence", "cadence 4"),
        ("sight", "sight 8"),
        ("fuel", "fuel 9"),
        ("food", "food 1h"),
        ("bite", "bite 2"),
    ] {
        let e = compile_err(&format!("kind a {{ {decl}\n {decl} }}"));
        assert!(
            e.starts_with("t.rules:2:2:") && e.contains(&format!("`{w}` declared twice")),
            "{e}"
        );
    }
    let e =
        compile_err("kind a {\n state A { when true => {\n next A\n next B } }\n state B { } }");
    assert!(e.starts_with("t.rules:4:7:"), "{e}");
    assert!(
        e.contains("a second `next`: the think already chose a state at line 3"),
        "{e}"
    );
    compile_ok("kind a { state A { when true => { next B  idle } } state B { } }");
    compile_ok("kind a { state A { when true => { if x > 1 { next B }  next A } } state B { } }");
}

#[test]
fn a_name_that_hides_a_constant_is_not_folded() {
    // A local, a sub's parameter, a mem or a need hides the constant.
    compile_ok("const w = 1 kind a { when 1 => { let w = 0  choose { w: idle }  move north } }");
    compile_ok(
        "const w = 1 kind a { when 1 => { if hour > 1 { let w = 0  choose { w: idle } } \
             else { idle }  move north } }",
    );
    compile_ok(
        "const w = 1 sub pick(w) { choose { w: idle } } kind a { when 1 => { pick(0)  move north } }",
    );
    for text in [
        "const w = 1 kind a { mem w  when w => idle\n when 1 => move north }",
        "const full = 1 kind a { need full max 10 decay 0  when full => idle\n when true => move north }",
        "const w = 1 kind a { when 1 => { let w = 0  choose { w: idle } }\n when 1 => move north }",
    ] {
        let k = compile_ok(text);
        assert!(
            k.debug.diagnostics.is_empty(),
            "{text}: {:?}",
            k.debug.diagnostics
        );
    }
    // A let in another kind, compiled last, hides nothing here.
    let k = compile_ok(
        "const w = 1 kind a { when w => idle\n when 1 => move north }\n\
             kind b { when 1 => { let w = 0  idle } }",
    );
    assert!(
        k.debug
            .diagnostics
            .iter()
            .any(|d| d.to_string().contains("never runs")),
        "{:?}",
        k.debug.diagnostics
    );
    // Nothing hides it: still folded.
    let e = compile_err("const W = 1 kind a { when 1 => { choose { W: idle }  move north } }");
    assert!(e.contains("a second action"), "{e}");
}

#[test]
fn a_local_that_hides_a_need_or_mem_is_an_error() {
    for (text, want) in [
        (
            "kind a { need food max 1d\n when true => { let food = 5  food += 1h } }",
            "t.rules:2:21: local `food` has the name of a need or mem slot of `a`",
        ),
        (
            "kind a { mem m\n when nearest a within 3 as m => idle }",
            "t.rules:2:7: binding `m` has the name of a need or mem slot of `a`",
        ),
        (
            "field s max 9  kind a { mem m\n when best s within 3 as m => idle }",
            "binding `m` has the name of a need or mem slot of `a`",
        ),
        (
            "kind a { mem m\n when true => for each a within 2 as m { look = 1 } }",
            "binding `m` has the name of a need or mem slot of `a`",
        ),
        (
            "kind a { mem m\n sub f(m) { idle }\n when true => f(1) }",
            "t.rules:2:6: parameter `m` of `f` has the name of a need or mem slot of `a`",
        ),
        // A trait's member sub, and a trait's own rule.
        (
            "trait t { need food max 1d  sub f(food: target) { move food } }\n\
                 kind a extends t { when true => f(here) }",
            "parameter `food` of `f` has the name of a need or mem slot of `t`",
        ),
        (
            "trait t { mem m  when true => { let m = 1  idle } }\nkind a extends t { }",
            "local `m` has the name of a need or mem slot of `t`",
        ),
    ] {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}: {e}");
    }
    // A name the code can't see is not hidden: a file sub sees no need,
    // a trait sees only its own; and a const may still be shadowed.
    compile_ok(
        "sub f(food) { if food > 1 { idle } }\n\
             kind a { need food max 1d  when true => f(food) }",
    );
    compile_ok(
        "trait t { when true => { let m = 1  idle } }\n\
             kind a extends t { mem m }",
    );
    compile_ok("const m = 1 kind a { when true => { let m = 2  idle } }");
}

#[test]
fn a_sub_that_calls_itself_many_times_compiles_quickly() {
    // Without a memo the one-action check walks 16^8 calls.
    let calls = "a(n - 1) ".repeat(16);
    compile_ok(&format!(
        "sub a(n) {{ if n > 0 {{ {calls} }} else {{ n = 0 }} }}\n\
             kind stone {{ glyph \"r\" when 1 > 0 => idle }}"
    ));
}

#[test]
fn unreachable_rule_warning_is_conservative() {
    let warnings = |text: &str| -> Vec<String> {
        compile_ok(text)
            .debug
            .diagnostics
            .iter()
            .filter(|d| d.level == Level::Warning)
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(
        warnings("kind a { when true => idle\n when hour > 1 => die }"),
        ["t.rules:2:2: warning: never runs: the rule at t.rules:1 always ends the think"]
    );
    assert!(warnings("kind a { when true => look = 1\n when hour > 1 => die }").is_empty());
    assert!(warnings("kind a { when hour > 1 => idle\n when true => die }").is_empty());
    assert!(
        warnings("kind a { when true => { if hour > 1 { idle } }\n when 1 => die }").is_empty()
    );
    assert!(
        warnings(
            "sub maybe(h) { if h == 0 { return }  move dir(h) }\n\
                 kind a { mem heading  when true => maybe(heading)\n when 1 => die }"
        )
        .is_empty()
    );
    assert_eq!(
        warnings("kind a { when true => idle\n state S { when 1 => die } }"),
        ["t.rules:2:12: warning: never runs: the reflex rule at t.rules:1 always ends the think"]
    );
    // Through inheritance: the trait's rule hides the kind's.
    assert_eq!(
        warnings(
            "trait t { when 2 > 1 => idle }\nkind a extends t { inherit t\n when hour > 1 => die }"
        ),
        ["t.rules:3:2: warning: never runs: the rule at t.rules:1 always ends the think"]
    );
    // One warning per rule, not one per kind that includes it.
    assert_eq!(
            warnings(
                "trait t { when true => idle\n when hour > 1 => die }\nkind a extends t { }\nkind b extends t { }"
            )
            .len(),
            1
        );
}

#[test]
fn a_directory_compiles_in_sorted_file_order() {
    let dir = std::env::temp_dir().join(format!("wmc-rules-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.rules"), "kind bee { when 1 => become ant }").unwrap();
    std::fs::write(dir.join("a.rules"), "kind ant { }").unwrap();
    std::fs::write(dir.join("notes.txt"), "kind ignored { }").unwrap();
    let k = compile_packs(&[&dir]).unwrap();
    assert_eq!(k.names().collect::<Vec<_>>(), vec!["ant", "bee"]);
    let abs = std::fs::canonicalize(&dir).unwrap();
    assert_eq!(k.debug.packs, [abs.to_string_lossy()]);
    std::fs::write(dir.join("c.rules"), "kind ant { }").unwrap();
    let err = compile_packs(&[&dir]).unwrap_err().to_string();
    assert!(
        err.starts_with("c.rules:1:6: kind `ant` declared twice (first at a.rules:1)"),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_directory_with_no_rules_files_is_an_error() {
    let dir = std::env::temp_dir().join(format!("wmc-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("life")).unwrap();
    std::fs::write(dir.join("notes.txt"), "kind ignored { }").unwrap();
    let err = compile_packs(&[&dir]).unwrap_err().to_string();
    assert_eq!(err, format!("{}: holds no .rules files", dir.display()));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pack_labels_name_one_file_each_and_a_pack_loads_once() {
    let root = std::env::temp_dir().join(format!("wmc-labels-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (one, two) = (root.join("one/pk"), root.join("two/pk"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(
        one.join("a.rules"),
        "kind first { glyph \"f\"\n when true => idle }",
    )
    .unwrap();
    std::fs::write(
        two.join("a.rules"),
        "kind second {\n when true => move north }",
    )
    .unwrap();
    // Two dirs of one name: each label takes a parent more.
    let k = compile_packs(&[&one, &two]).unwrap();
    assert_eq!(k.debug.files, ["one/pk/a.rules", "two/pk/a.rules"]);
    let id = k.by_name("second").unwrap().id;
    let r = k.debug.rules.iter().find(|r| r.kind == id).unwrap();
    assert!(r.text.contains("move north"), "{}", r.text);
    // Two files of one name, likewise.
    let k = compile_packs(&[&one.join("a.rules"), &two.join("a.rules")]).unwrap();
    assert_eq!(k.debug.files, ["one/pk/a.rules", "two/pk/a.rules"]);
    std::fs::write(two.join("a.rules"), "kind first { }").unwrap();
    let err = compile_packs(&[&one, &two]).unwrap_err().to_string();
    assert!(
        err.starts_with(
            "two/pk/a.rules:1:6: kind `first` declared twice (first at one/pk/a.rules:1)"
        ),
        "{err}"
    );
    // The same pack twice is that pack once.
    let slash = std::path::PathBuf::from(format!("{}/", one.display()));
    let k = compile_packs(&[&one, &slash]).unwrap();
    assert_eq!(
        (k.debug.packs.len(), k.debug.files.as_slice()),
        (1, ["a.rules".to_string()].as_slice())
    );
    // A file its directory pack already loaded is that file once.
    let k = compile_packs(&[&one, &one.join("a.rules")]).unwrap();
    assert_eq!(k.debug.files.len(), 1, "{:?}", k.debug.files);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Packs are file lists in the order given: a later pack's kinds come
/// after an earlier one's, may extend them and call their subs, and a
/// name declared in two packs is an error naming both files.
#[test]
fn packs_merge_in_order_and_duplicates_name_both_files() {
    let root = std::env::temp_dir().join(format!("wmc-packs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (base, wild) = (root.join("base"), root.join("wild"));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&wild).unwrap();
    std::fs::write(base.join("b.rules"), "kind hen { }\nsub rest() { idle }").unwrap();
    std::fs::write(base.join("a.rules"), "trait walker { }").unwrap();
    std::fs::write(
        wild.join("a.rules"),
        "kind wolf extends walker { when true => rest() }\nkind pup extends hen { }",
    )
    .unwrap();
    let single = root.join("lone.rules");
    std::fs::write(&single, "kind moth { }").unwrap();
    let k = compile_packs(&[&base, &wild, &single]).unwrap();
    // Pre-order: `pup` right after its parent `hen`.
    assert_eq!(
        k.names().collect::<Vec<_>>(),
        ["hen", "pup", "wolf", "moth"]
    );
    assert_eq!(k.debug.packs.len(), 3);
    assert_eq!(
        k.debug.files,
        ["base/a.rules", "base/b.rules", "wild/a.rules", "lone.rules"]
    );
    // Two packs, one name: both positions, pack-qualified.
    std::fs::write(wild.join("b.rules"), "\nkind hen { }").unwrap();
    let err = compile_packs(&[&base, &wild]).unwrap_err().to_string();
    assert!(
        err.starts_with("wild/b.rules:2:6: kind `hen` declared twice (first at base/b.rules:1)"),
        "{err}"
    );
    std::fs::write(wild.join("b.rules"), "const N = 1\nsub rest() { idle }").unwrap();
    let err = compile_packs(&[&base, &wild]).unwrap_err().to_string();
    assert!(
        err.contains("sub `rest` declared twice (first at base/b.rules:2)"),
        "{err}"
    );
    // A file sub shares the namespace with kinds, traits and tags.
    std::fs::write(wild.join("b.rules"), "sub hen() { idle }").unwrap();
    let err = compile_packs(&[&base, &wild]).unwrap_err().to_string();
    assert!(
        err.starts_with("wild/b.rules:1:5: `hen` is already a kind (first at base/b.rules:1)"),
        "{err}"
    );
    std::fs::write(wild.join("b.rules"), "kind owl { tags rest }").unwrap();
    let err = compile_packs(&[&base, &wild]).unwrap_err().to_string();
    assert!(
        err.starts_with(
            "wild/b.rules:1:6: tag `rest` is also a sub's name (first at base/b.rules:2)"
        ),
        "{err}"
    );
    // A member sub is its kind's or trait's own: two may share a name,
    // but not a kind's or trait's name.
    compile_ok(
        "trait t { sub wander() { idle } } trait u { sub wander() { idle } }
             kind a extends t { when true => wander() } kind b extends u { when true => wander() }",
    );
    assert_eq!(
        compile_err("kind a { sub b() { idle } when true => b() }\nkind b { }"),
        "t.rules:1:14: `a`'s sub `b` has the name of a kind (first at t.rules:2)"
    );
    let err = compile_packs(&[&base, &root.join("gone")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("gone"), "{err}");
    std::fs::remove_dir_all(&root).unwrap();
}

/// `--set NAME=V`: a constant takes the value given, the constants after it
/// fold with it, and a name the rules do not declare is an error that
/// lists the ones they do.
#[test]
fn a_const_set_from_outside_replaces_the_declared_one() {
    let text = "const RATE = 2
                const TWICE = RATE * 2
                kind k { glyph \"k\"  mem m
                  when true => m = TWICE }";
    let value = |kinds: &Kinds| kinds.code.iter().any(|op| op.imm == 2 * 7);
    let plain = compile_files_with(&[("t.rules", text)], &[]).unwrap();
    let set = compile_files_with(&[("t.rules", text)], &[("RATE".into(), 7)]).unwrap();
    assert!(!value(&plain) && value(&set), "TWICE folded with RATE = 7");
    assert_ne!(plain.hash, set.hash, "other code, another rule set");
    let e = compile_files_with(&[("t.rules", text)], &[("RAET".into(), 7)]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "--set: `RAET`: the rules declare no such const (they declare: RATE, TWICE)"
    );
    let e = compile_files_with(&[("t.rules", "kind k { glyph \"k\" }")], &[("X".into(), 1)])
        .unwrap_err();
    assert!(e.to_string().ends_with("(they declare: none)"), "{e}");
}

/// What `wmc rates` reads off each rule: its dice multiplied (constants
/// and trait parameters folded), its other gates, the age it waits for,
/// and the first thing its body does that ends the think.
#[test]
fn a_rule_s_shape_is_read_off_its_source() {
    let kinds = compile(
        "t.rules",
        "const ODDS = 3
         trait aging(after, odds) { when age > after and rand(100000) < odds => die }
         sub stroll() { move random free }
         kind k extends aging(5d, ODDS) {
           glyph \"k\"
           mem m
           inherit aging
           when chance(2) and rand(10) <= 4 and m > 0 and nearest free within 1 as c => { m = 0  spawn k at c }
           when true => m += 1
           when age >= 2h => become k
           when m > 3 => { if m > 5 { idle } }
           when rand(0) < 1 => stroll()
         }",
    )
    .unwrap();
    let shapes: Vec<&RuleShape> = kinds.debug.rules.iter().map(|r| &r.shape).collect();
    let shape = |dice, gates, after_age, acts: Option<&str>| RuleShape {
        dice,
        gates,
        after_age,
        acts: acts.map(String::from),
    };
    assert_eq!(
        shapes,
        [
            &shape((3, 100000), 0, Some(5 * 21600), Some("die")),
            &shape((1, 100), 2, None, Some("spawn k")), // 2/100 x 5/10
            &shape((1, 1), 0, None, None),
            &shape((1, 1), 0, Some(1800), Some("become k")),
            &shape((1, 1), 1, None, Some("if")),
            &shape((1, 1), 1, None, Some("stroll()")), // rand(0): no dice, a gate
        ]
    );
    assert!((shapes[1].dice_odds() - 0.01).abs() < 1e-12);
}

/// Each rule keeps the terms of its condition's top-level `and` as
/// written (comments dropped, lines joined), each with its first op: a
/// top-level `or` is one term, a parenthesised `and` stays one.
#[test]
fn a_rule_keeps_the_terms_of_its_condition() {
    let kinds = compile_ok(
        "kind k {
           glyph \"k\"
           mem m
           when m > 1 and (m < 9 or m == 20) and not m == 4 => m = 0
           when m > 1 or m < 0 => m = 1
           when (m > 1 and m < 5) and m != 3   # a comment
             and nearest free within 1 as c    # another
             => spawn k at c
           when true => m += 1
         }",
    );
    let terms: Vec<Vec<&str>> = kinds
        .debug
        .rules
        .iter()
        .map(|r| r.terms.iter().map(|t| t.text.as_str()).collect())
        .collect();
    assert_eq!(
        terms,
        [
            vec!["m > 1", "(m < 9 or m == 20)", "not m == 4"],
            vec!["m > 1 or m < 0"],
            vec!["(m > 1 and m < 5)", "m != 3", "nearest free within 1 as c"],
            vec!["true"],
        ]
    );
    // The terms split the condition's code in order.
    for r in &kinds.debug.rules {
        assert_eq!(r.terms[0].pc, r.cond_pc);
        assert!(r.terms.windows(2).all(|w| w[0].pc < w[1].pc));
        assert!(r.terms[r.terms.len() - 1].pc < r.body_pc);
    }
}

/// A field's declaration and every use of one say what is wrong, and where.
#[test]
fn field_errors_name_the_problem() {
    let errs = [
        ("field f max 0", "max 0 is not in 1..=65535"),
        ("field f max 70000", "max 70000 is not in 1..=65535"),
        ("field f max 4 { init 5 }", "init 5 is not in 0..=4"),
        (
            "field f max 4 { init noise scale 0 }",
            "noise scale 0 is not above 0",
        ),
        ("field f max 4 { init 1 init 2 }", "`init` declared twice"),
        ("field f max 4 { color \"red\" }", "color takes"),
        (
            "field f max 4 { glyph \"x\" }",
            "expected `init`, `color`, `outside`, `every`, `relief` or `}`",
        ),
        ("field f max 4 field f max 5", "field `f` declared twice"),
        ("field k max 4 kind k { }", "`k` is already a kind"),
        ("field s max 4 sub s() { }", "`s` is already a sub"),
        ("field C max 4 const C = 1", "`C` is already a const"),
        ("field t max 4 kind k { tags t }", "`t` is already a tag"),
        (
            "field m max 4 kind k { sub m() { } }",
            "`m` is already a sub of `k`",
        ),
        (
            "field add max 4",
            "`add` is a reserved word, not a field name",
        ),
        (
            "kind k { when true => add f 1 }",
            "no field `f`: the rules declare no field",
        ),
        (
            "field g max 4 kind k { when true => add f 1 }",
            "no field `f` (the fields: g)",
        ),
        (
            "field f max 4 conserved kind k { when true => add f 1 }",
            "the field is conserved",
        ),
        (
            "field f max 4 fixed kind k { when true => add f 1 }",
            "the field is fixed",
        ),
        (
            "field f max 4 fixed kind k { need f max 4 decay 0  when true => harvest f 1 }",
            "the field is fixed, no actor writes it",
        ),
        (
            "field f max 4 kind k { when true => harvest f 1 }",
            "this kind has no need `f`",
        ),
        (
            "field f max 4 sub s() { drop f 1 } kind k { need f max 4  when true => s() }",
            "`drop` inside a sub",
        ),
        (
            "field f max 4 kind k { when f(1) > 0 => idle }",
            "reads the field at one target",
        ),
        (
            "field f max 4 kind k { when f(here, here) > 0 => idle }",
            "reads the field at one target",
        ),
        (
            "field f max 4 kind k { when not best f within 2 as b => idle }",
            "must be a top-level conjunct",
        ),
        (
            "kind k { when true => { add a 1  add b 1 } }",
            "no field `a`",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
}

/// A law says what it can't do, and where.
#[test]
fn law_errors_name_the_problem() {
    let errs = [
        (
            "field f max 4 { every 0 { f = 1 } }",
            "a law runs every 1 tick or more",
        ),
        (
            "field f max 4 { every 1 { f = 1 } every 2 { f = 2 } }",
            "`every` given twice",
        ),
        ("field f max 4 { outside 5 }", "outside 5 is not in 0..=4"),
        (
            "field f max 4 { every 1 { move north } }",
            "`let`, assignments and `if` only",
        ),
        (
            "field f max 4 { every 1 { while f > 0 { f -= 1 } } }",
            "`let`, assignments and `if` only",
        ),
        (
            "field f max 4 field g max 4 { every 1 { f = 1 } }",
            "writes only its own field `g`, not `f`",
        ),
        ("field f max 4 { every 1 { f = zz } }", "unknown name `zz`"),
        (
            "field f max 4 { every 1 { zz = 1 } }",
            "unknown name `zz`: declare it with `let`",
        ),
        ("field f max 4 { every 1 { let f = 1 } }", "`f` is a field"),
        (
            "field f max 4 { every 1 { f = age } }",
            "only the clock of the senses",
        ),
        (
            "field f max 4 { every 1 { f = sum f within 2 } }",
            "within 1, not 2",
        ),
        (
            "field f max 4 { every 1 { f = sum (sum f within 1) within 1 } }",
            "`within` is a reserved word",
        ),
        (
            "field f max 4 { every 1 { let s = sum f within 1  f = max s within 1 } }",
            "an aggregate inside an aggregate",
        ),
        (
            "field f max 4 { every 1 { f = count fox within 1 } }",
            "searches, targets and calls",
        ),
        (
            "field f max 4 { every 1 { f = dist(north) } }",
            "searches, targets and calls",
        ),
        (
            "field f max 4 { every 1 { if nearest f within 1 as v { f = 1 } } }",
            "a law doesn't search",
        ),
        (
            "field f max 4 { every 1 { f = f + flow(f, f, 0) } }",
            "`flow`'s rate is a number above 0, not 0",
        ),
        (
            "field f max 4 { every 1 { f = flow(sum f within 1, f, 2) } }",
            "an aggregate or a flow inside a flow",
        ),
        (
            "field f max 4 { every 1 { f = flow(f, flow(f, f, 1), 2) } }",
            "an aggregate or a flow inside a flow",
        ),
        (
            "field f max 4 { every 1 { f = count (flow(f, f, 1) > 0) within 1 } }",
            "an aggregate inside an aggregate",
        ),
        (
            "field f max 4 { every 1 { f = flow(f, f) } }",
            "expected `,`",
        ),
        (
            "field f max 4 kind hen { glyph \"h\" when true => idle } field g max 4 { every 1 { g = is(north, hen) } }",
            "a law reads actors on its own cell",
        ),
        (
            "field f max 4 { every 1 { f = is(here, (f > 1)) } }",
            "a field condition is a plain condition",
        ),
        (
            "field f max 4 { every 1 { f = is(here, fox) } }",
            "unknown kind, tag or material `fox`",
        ),
        (
            "field f max 4 kind k { glyph \"k\" when flow(f, f, 1) > 0 => idle }",
            "`flow` belongs in a field's law",
        ),
        (
            "kind k { when sum x within 1 > 0 => idle }",
            "expected `=>`, found `x`",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    // Clock, constants, dice and every operator a law has, compiled.
    let k = compile(
        "t.rules",
        "const R = 3
         field g max 9
         field f max 100 { outside 4  every 1h {
           let a = clamp(f + R * 2 - g / 2 % 3, 0, 50)
           if (hour > 6 and light >= 0) or not (day < 0) { f = max(a, min(f, abs(-sign(a)))) }
           else { f -= rand(3) + chance(10) }
           f += count (g > 1 or f == 0) within 1 + min g within 1 + avg f within 1
         } }",
    )
    .unwrap();
    let law = k.fields[1].law.as_ref().unwrap();
    assert_eq!((law.every, k.fields[1].outside), (900, Some(4)));
}

/// A law that leaves 0 at 0, known without dice or the clock, may skip a
/// chunk where the fields it reads are all 0; any other may not.
#[test]
fn a_law_that_leaves_zero_at_zero_is_quiet() {
    let k = compile(
        "t.rules",
        "field room max 9
         field fade max 255 { every 16 { fade -= (fade + 31) / 32 } }
         field crop max 9 { every 1h { crop = min(crop + 1, room) } }
         field life max 1 { every 1 { let n = sum life within 1  if n == 3 { life = 1 } else if n != 4 { life = 0 } } }
         field up max 9 { every 1 { up += 1 } }
         field dice max 9 { every 1 { dice = chance(50) * 0 } }
         field dark max 9 { every 1 { dark = 0 * hour } }
         field some max 9 { every 1 { if room == 0 { some = 3 } } }
         field wet max 9 { every 1 { wet += flow(wet, room + wet, 2) } }",
    )
    .unwrap();
    let quiet: Vec<(&str, bool)> = k
        .fields
        .iter()
        .filter_map(|f| f.law.as_ref().map(|l| (f.name.as_str(), l.quiet)))
        .collect();
    assert_eq!(
        quiet,
        [
            ("fade", true),
            ("crop", true),
            ("life", true),
            ("up", false),
            ("dice", false),
            ("dark", false),
            ("some", false),
            ("wet", true),
        ]
    );
    let in_step: Vec<&str> = k
        .fields
        .iter()
        .filter(|f| f.law.as_ref().is_some_and(|l| l.in_step))
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(in_step, ["wet"], "only a law with a flow runs in step");
}

/// A field condition in a search says what is wrong with it.
#[test]
fn field_condition_errors_name_the_problem() {
    let errs = [
        (
            "kind k { when nearest (free) within 3 as v => idle }",
            "compares a field",
        ),
        (
            "field f max 9 kind k { when nearest (f > 1 and free and bare) within 3 as v => idle }",
            "one predicate at most",
        ),
        (
            "field f max 9 kind k { when nearest (f > 1 and f > 2 and f > 3 and f > 4) within 3 as v => idle }",
            "three fields at most",
        ),
        (
            "field f max 9 kind k { when nearest (f > 1 and (f < 3)) within 3 as v => idle }",
            "inside another",
        ),
        (
            "kind k { when nearest (g > 1) within 3 as v => idle }",
            "no field `g`",
        ),
        (
            "field f max 9 kind k { when count (f > 1 and fox) within 3 > 0 => idle }",
            "unknown kind, tag or material `fox`",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
}

/// Parentheses nested past the limit where a predicate goes are an error,
/// never a stack overflow (a fuzz find).
#[test]
fn nested_field_conditions_are_an_error_not_an_overflow() {
    let text = format!(
        "kind k {{ when nearest {} free within 1 as v => idle }}",
        "(".repeat(5000)
    );
    let e = compile_err(&text);
    assert!(e.contains("nested too deep"), "{e}");
}

/// Packs declare grounds and features after the engine's three, may
/// redeclare those in place, and name them in predicates; the names clash
/// with nothing else.
#[test]
fn materials_are_declared_redeclared_and_named() {
    let k = compile(
        "t.rules",
        "ground mud { walkable  glyph \",\"  color \"#6b4f2a\" }
         ground water { walkable  drink }
         feature wall { blocks  glyph \"=\" }
         kind k { when nearest mud within 3 as v => move toward v
                  when count wall within 1 > 0 or is(here, water) => idle }",
    )
    .unwrap();
    let names =
        |v: &[crate::rules::MaterialDef]| v.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&k.materials.grounds), ["soil", "water", "mud"]);
    assert_eq!(names(&k.materials.features), ["rock", "wall"]);
    let t = k.materials.table;
    assert!(
        t.walk[1] && t.drink[1],
        "water redeclared: walkable, still drinkable"
    );
    assert!(t.walk[2] && !t.drink[2] && t.blocks[2]);
    assert_eq!(k.materials.ground("mud").map(|g| g.0), Some(2));
    assert_ne!(
        k.hash,
        compile("t.rules", "kind k { when true => idle }")
            .unwrap()
            .hash
    );
    let errs = [
        ("ground mud { } ground mud { }", "declared twice"),
        ("ground mud { } feature mud { }", "declared twice"),
        ("feature soil { }", "a ground and a feature"),
        ("kind mud { } ground mud { }", "already a kind"),
        ("const mud = 1  ground mud { }", "already a const"),
        (
            "ground mud { blocks }",
            "expected `walkable`, `drink`, `glyph`, `color` or `}` in a ground",
        ),
        (
            "feature wall { drink }",
            "expected `blocks`, `opaque`, `glyph`, `color` or `}` in a feature",
        ),
        (
            "ground mud { opaque }",
            "expected `walkable`, `drink`, `glyph`, `color` or `}` in a ground",
        ),
        ("ground mud { glyph \"ab\" }", "one printable character"),
        (
            "kind k { when nearest only rock within 1 as v => idle }",
            "`only` applies to a kind",
        ),
        (
            "kind k { when nearest mud within 1 as v => idle }",
            "unknown kind, tag or material `mud`",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    let many: String = (0..15)
        .map(|i| format!("ground g{i} {{ walkable }} "))
        .collect();
    assert!(compile_err(&many).contains("at most 16 grounds"));
}

/// `build` names a declared material; `dig` and `build` are actions.
#[test]
fn build_and_dig_errors_name_the_problem() {
    let errs = [
        (
            "kind k { when true => build clay at east }",
            "no ground or feature `clay`",
        ),
        ("kind k { when true => build rock east }", "expected `at`"),
        (
            "kind k { when true => { dig east  dig west } }",
            "a second action",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    compile(
        "t.rules",
        "kind k { when true => build rock at north  when true => dig south }",
    )
    .unwrap();
}

/// Height's words say what is wrong.
#[test]
fn height_errors_name_the_problem() {
    let errs = [
        ("kind k { reach 0 }", "reach is 1 to 64 cells"),
        ("kind k { reach 65 }", "reach is 1 to 64 cells"),
        ("kind k { climb 2 on height }", "no field `height`"),
        (
            "field h max 9 kind k { climb -1 on h }",
            "a step rises 0 or more",
        ),
        ("field h max 9 kind k { climb 2 h }", "expected `on`"),
        (
            "kind k { mem m  when true => m = sees(north) over h }",
            "no field `h`",
        ),
        (
            "field h max 9 kind k { mem m  when true => m = sees(north) h }",
            "expected `over`",
        ),
        (
            "kind k { mem m  when true => m = max h along north }",
            "no field `h`",
        ),
        ("field h max 9 { relief relief }", "`relief` declared twice"),
        (
            "field h max 9 { every 1 { h = max h along north } }",
            "expected `within`",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    let k = compile(
        "t.rules",
        "field h max 9 { relief }
         trait tall { reach 3  climb 2 on h }
         kind k extends tall { mem m
           when true => m = sees(at(x + 2, y)) over h eye 1 + sum h along north + min h along east }",
    )
    .unwrap();
    assert_eq!(
        (k.defs[0].reach, k.defs[0].climb),
        (3, Some((0, 2))),
        "inherited"
    );
    assert!(k.fields[0].relief);
}

/// Packs declare layers after the engine's two (standing, ground cover);
/// a kind names one, inherits its parent's, and lives in one layer only.
#[test]
fn layers_are_declared_named_and_inherited() {
    let k = compile(
        "t.rules",
        "layer air { flies }
         layer burrow
         kind gull { layer air  when true => move random free }
         kind tern extends gull { }
         kind mole { layer burrow }
         kind crab { }
         kind weed { cover }",
    )
    .unwrap();
    let layer = |n: &str| k.by_name(n).unwrap().layer;
    assert_eq!(
        [
            layer("gull"),
            layer("tern"),
            layer("mole"),
            layer("crab"),
            layer("weed")
        ],
        [2, 2, 3, 0, 1]
    );
    assert!(k.flies(2) && !k.flies(3) && !k.flies(0) && !k.flies(1));
    assert_ne!(
        k.hash,
        compile(
            "t.rules",
            "layer air
             layer burrow
             kind gull { layer air  when true => move random free }
             kind tern extends gull { }
             kind mole { layer burrow }
             kind crab { }
             kind weed { cover }"
        )
        .unwrap()
        .hash,
        "whether a layer flies is in the hash"
    );
    let errs = [
        ("layer air layer air", "already a name"),
        ("kind air { } layer air", "already a name"),
        ("const air = 1  layer air", "already a name"),
        ("layer soil", "already a name"),
        ("layer standing", "already a name"),
        (
            "layer air { walkable }",
            "expected `flies`, `on` or `}` in a layer",
        ),
        ("kind k { layer air }", "no layer `air`"),
        (
            "layer air  kind k { layer air  layer air }",
            "`layer` declared twice",
        ),
        (
            "layer air  kind k { cover  layer air }",
            "a kind lives in one layer",
        ),
        (
            "layer air  kind k { cover }  kind j extends k { layer air }",
            "a kind lives in one layer",
        ),
        (
            "layer a  layer b  trait k { layer a }  trait j { layer b }  kind m extends k, j { }",
            "layers",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    let many: String = (0..=crate::rules::MAX_LAYERS)
        .map(|i| format!("layer l{i} "))
        .collect();
    assert!(compile_err(&many).contains("at most 8 layers"));
}

/// Groups (`group max N [init M]`): inherited like any declaration; a
/// group's own words (`grow`, `split`, `join`) only in a group's code;
/// `heads` and `heads_of` anywhere.
#[test]
fn groups_are_declared_inherited_and_checked() {
    let k = compile(
        "t.rules",
        "kind herd { group max 500 init 20
           when heads > 100 and nearest free within 1 as n => split heads / 2 at n
           when nearest herd within 1 as h and heads_of(h) + heads <= 500 => join h
           when true => { grow 1  idle } }
         kind big extends herd { group max 900 }
         kind calf extends herd { }
         kind lone { when heads_of(here) == 1 => idle }",
    )
    .unwrap();
    let g = |n: &str| k.by_name(n).unwrap().group;
    assert_eq!(
        [g("herd"), g("big"), g("calf"), g("lone")],
        [(500, 20), (900, 1), (500, 20), (0, 0)]
    );
    let d = k.by_name("lone").unwrap();
    assert_eq!((d.max_heads(), d.start_heads()), (1, 1));
    assert_ne!(
        k.hash,
        compile(
            "t.rules",
            "kind herd { group max 501 init 20
               when heads > 100 and nearest free within 1 as n => split heads / 2 at n
               when nearest herd within 1 as h and heads_of(h) + heads <= 500 => join h
               when true => { grow 1  idle } }
             kind big extends herd { group max 900 }
             kind calf extends herd { }
             kind lone { when heads_of(here) == 1 => idle }"
        )
        .unwrap()
        .hash,
        "a group's max is in the hash"
    );
    let errs = [
        ("kind k { group 5 }", "expected `max`"),
        ("kind k { group max 1 }", "a group holds 2 to 65535 heads"),
        (
            "kind k { group max 70000 }",
            "a group holds 2 to 65535 heads",
        ),
        (
            "kind k { group max 5 init 6 }",
            "starts with 1 to its max (5) heads",
        ),
        (
            "kind k { group max 5 init 0 }",
            "starts with 1 to its max (5) heads",
        ),
        ("kind k { cover  group max 5 }", "ground cover and a group"),
        (
            "kind k { when true => { grow 1  idle } }",
            "`grow`: `k` is not a group",
        ),
        (
            "kind k { when true => split 1 at east }",
            "`split`: `k` is not a group",
        ),
        (
            "kind k { when true => join east }",
            "`join`: `k` is not a group",
        ),
        (
            "sub s() { grow 1 }  kind k { group max 5  when true => { s()  idle } }",
            "`grow` inside a sub",
        ),
        ("kind k { mem heads }", "reserved"),
        ("kind k { mem grow }", "reserved"),
        (
            "trait a { group max 5 }  trait b { group max 6 }  kind k extends a, b { }",
            "groups",
        ),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
}

/// Big kinds (`size W H`): inherited, in the hash; never ground cover, nor
/// in a layer that rests on a kind; a big group neither splits nor joins;
/// `fits(t)` anywhere.
#[test]
fn sizes_are_declared_inherited_and_checked() {
    let k = compile(
        "t.rules",
        "kind whale { size 3 2  when fits(east) => move east  when true => idle }
         kind orca extends whale { size 2 1 }
         kind calf extends whale { }
         kind fish { when fits(here) => idle }",
    )
    .unwrap();
    let z = |n: &str| k.by_name(n).unwrap().size;
    assert_eq!(
        [z("whale"), z("orca"), z("calf"), z("fish")],
        [(3, 2), (2, 1), (3, 2), (1, 1)]
    );
    assert!(
        k.any_big() && k.by_name("whale").unwrap().is_big() && !k.by_name("fish").unwrap().is_big()
    );
    assert_ne!(
        k.hash,
        compile(
            "t.rules",
            "kind whale { size 3 3  when fits(east) => move east  when true => idle }
             kind orca extends whale { size 2 1 }
             kind calf extends whale { }
             kind fish { when fits(here) => idle }"
        )
        .unwrap()
        .hash,
        "a size is in the hash"
    );
    let errs = [
        ("kind k { size 0 2 }", "1 to 16 cells each way"),
        ("kind k { size 2 17 }", "1 to 16 cells each way"),
        ("kind k { size 2 }", "expected"),
        (
            "kind k { cover  size 2 2 }",
            "ground cover and bigger than a cell",
        ),
        (
            "layer canopy { on t }  kind t { }  kind k { layer canopy  size 2 2 }",
            "rests on a kind",
        ),
        (
            "kind k { size 2 2  group max 9  when true => split 1 at east }",
            "a big group neither splits nor joins",
        ),
        (
            "kind k { size 2 2  group max 9  when true => join east }",
            "a big group neither splits nor joins",
        ),
        (
            "trait a { size 2 2 }  trait b { size 3 3 }  kind k extends a, b { }",
            "sizes",
        ),
        ("kind k { mem fits }", "reserved"),
    ];
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    assert!(
        compile(
            "t.rules",
            "kind k { size 2 2  group max 9  when true => { grow 1  idle } }"
        )
        .is_ok()
    );
}

/// A layer that rests on a kind (`layer canopy { on tree }`, `docs/WORLD.md`
/// §7): on a kind's family or a tag that stands; never a material, cover or
/// a kind in a declared layer.
#[test]
fn a_layer_rests_on_a_kind_that_stands() {
    use crate::rules::vm::Rest;
    let k = compile(
        "t.rules",
        "layer canopy { on tree }
         layer nest { flies  on tall }
         kind tree { tags tall }
         kind oak extends tree { }
         kind monkey { layer canopy }
         kind owl { layer nest }",
    )
    .unwrap();
    let (tree, oak, monkey) = (
        k.by_name("tree").unwrap().id,
        k.by_name("oak").unwrap().id,
        k.by_name("monkey").unwrap().id,
    );
    let on = |l: u8, kind: u16| {
        k.rest(l)
            .holds(crate::stage::ActorId::pack(kind, 0), &k.tag_bits)
    };
    assert!(
        on(2, tree) && on(2, oak) && !on(2, monkey),
        "a kind's family"
    );
    assert!(on(3, tree) && on(3, oak) && !on(3, monkey), "a tag");
    assert!(!on(2, u16::MAX - 1) && k.rest(0) == Rest::Any && k.rest(1) == Rest::Any);
    assert!(k.flies(3) && !k.flies(2));
    assert!(k.any_rest());
    assert_ne!(
        k.hash,
        compile(
            "t.rules",
            "layer canopy
             layer nest { flies  on tall }
             kind tree { tags tall }
             kind oak extends tree { }
             kind monkey { layer canopy }
             kind owl { layer nest }"
        )
        .unwrap()
        .hash,
        "what a layer rests on is in the hash"
    );
    let errs = [
        ("layer canopy { on }", "expected a kind or tag"),
        ("layer canopy { on soil }", "not a material"),
        (
            "layer canopy { on nobody }",
            "unknown kind, tag or material `nobody`",
        ),
        (
            "layer canopy { on grass }  kind grass { cover }",
            "does not stand",
        ),
        (
            "layer canopy { on monkey }  kind monkey { layer canopy }",
            "does not stand",
        ),
        ("layer canopy { on t }  trait t { }", "is a trait"),
    ];
    // A layer that rests on a kind takes a tag bit.
    let tags: Vec<String> = (0..64).map(|i| format!("t{i}")).collect();
    let many = format!(
        "layer canopy {{ on k }}  kind k {{ tags {} }}",
        tags.join(" ")
    );
    assert!(
        compile_err(&many).contains("at most 63 tags in a rule set (64, less one for each layer")
    );
    let fits = format!(
        "layer canopy {{ on k }}  kind k {{ tags {} }}",
        tags[..63].join(" ")
    );
    assert!(compile("t.rules", &fits).is_ok());
    for (text, want) in errs {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
}

/// Players (`docs/WORLD.md` §12): each one's top-level names are its own,
/// `PLAYER.NAME`; it uses the arena's fields, traits and consts, declares
/// kinds, traits, subs, consts and `private` fields only, and names nothing
/// the arena has.
#[test]
fn players_have_their_own_names_and_only_a_players_part() {
    let arena = "field crumbs max 9 { init 9 }
                 const BIG = 9
                 trait walker { need food max 1d vital  sub go() { move random free } }";
    let red = "const LOAD = 3
               field trail max 99 private { every 16 { trail = trail / 2 } }
               kind ant extends walker { when crumbs(here) > LOAD and trail(here) < BIG => { add trail 9  idle }
                                         when true => go() }
               kind queen extends ant { }";
    let blue = "const LOAD = 5
                field trail max 50 private
                kind ant extends walker { when trail(here) > LOAD => idle  when true => go() }";
    let files = [
        ("arena.rules", arena, None),
        ("red.rules", red, Some("red")),
        ("blue.rules", blue, Some("blue")),
    ];
    let k = compile_owned(&files, &[]).unwrap();
    let mut names: Vec<&str> = k.names().collect();
    names.sort_unstable();
    assert_eq!(names, ["blue.ant", "red.ant", "red.queen"]);
    let fields: Vec<&str> = k.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, ["crumbs", "red.trail", "blue.trail"]);
    assert_eq!(k.fields[1].max, 99);
    assert_eq!(k.fields[2].max, 50);
    assert_eq!(
        k.by_name("red.queen").unwrap().id,
        k.by_name("red.ant").unwrap().id + 1,
        "a player's kind extends its own"
    );
    // Without players, nothing is renamed.
    assert!(compile_owned(&[("arena.rules", arena, None)], &[]).is_ok());

    let player = |text: &str| {
        compile_owned(
            &[("arena.rules", arena, None), ("p.rules", text, Some("p"))],
            &[],
        )
        .unwrap_err()
        .to_string()
    };
    let errs = [
        ("ground mud { walkable }", "declares no ground"),
        ("feature wall { blocks }", "declares no feature"),
        ("layer air", "declares no layer"),
        ("field heat max 9", "a player's field is `private`"),
        ("kind crumbs { }", "`crumbs` is the arena's"),
        ("const BIG = 1", "`BIG` is the arena's"),
        ("sub go() { idle }", "`go` is the arena's"),
        ("kind walker { }", "`walker` is the arena's"),
        ("kind food { }", "a word the language reads"),
        (
            "kind k { when trail(here) > 0 => idle }",
            "unknown sub `trail`",
        ),
    ];
    for (text, want) in errs {
        let e = player(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    let e = compile_owned(&[("arena.rules", "field f max 9 private", None)], &[])
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("only a player's pack declares a `private` field"),
        "{e}"
    );
    // The arena can't name a player's kind.
    let e = compile_owned(
        &[
            (
                "arena.rules",
                "kind nest { when nearest ant within 3 as a => idle }",
                None,
            ),
            ("p.rules", "kind ant { }", Some("p")),
        ],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("`ant`"), "{e}");

    assert_eq!(
        crate::rules::player_of("red=packs/red"),
        (Some("red"), "packs/red")
    );
    assert_eq!(crate::rules::player_of("packs/red"), (None, "packs/red"));
    assert_eq!(crate::rules::player_of("Red=x"), (None, "Red=x"));
    assert_eq!(crate::rules::player_of("a/b=c"), (None, "a/b=c"));
}

/// An arena's `budget`: each player's kinds think in at most `fuel` ops,
/// a player declares at most `private` private fields, and the rows cap
/// reaches the kinds table with who owns which kind. Only an arena sets
/// it, once.
#[test]
fn an_arenas_budget_bounds_its_players() {
    let arena = "const ROOM = 40
                 budget { fuel 300  rows ROOM  private 1 }
                 kind crumb { glyph \"c\"  fuel 900  when true => idle }";
    let red = "field trail max 9 private
               kind ant { glyph \"a\"  fuel 2000  when true => idle }
               kind scout { glyph \"s\"  fuel 100  when true => idle }";
    let blue = "kind ant { glyph \"b\"  when true => idle }";
    let k = compile_owned(
        &[
            ("arena.rules", arena, None),
            ("red.rules", red, Some("red")),
            ("blue.rules", blue, Some("blue")),
        ],
        &[],
    )
    .unwrap();
    let fuel = |n: &str| k.by_name(n).unwrap().fuel;
    assert_eq!(
        [
            fuel("crumb"),
            fuel("red.ant"),
            fuel("red.scout"),
            fuel("blue.ant")
        ],
        [900, 300, 100, 300],
        "the arena's own kinds are not bound"
    );
    assert_eq!(k.players.rows, Some(40));
    let mut names = k.players.names.clone();
    names.sort_unstable();
    assert_eq!(names, ["blue", "red"]);
    let owner = |n: &str| {
        k.players
            .of(k.by_name(n).unwrap().id)
            .map(|p| k.players.names[p].as_str())
    };
    assert_eq!(
        [owner("crumb"), owner("red.ant"), owner("blue.ant")],
        [None, Some("red"), Some("blue")]
    );

    let err =
        |files: &[(&str, &str, Option<&str>)]| compile_owned(files, &[]).unwrap_err().to_string();
    let e = err(&[
        ("arena.rules", arena, None),
        (
            "red.rules",
            "field a max 9 private  field b max 9 private",
            Some("red"),
        ),
    ]);
    assert!(
        e.contains("player `red` declares more private fields than the arena's budget of 1"),
        "{e}"
    );
    let e = err(&[
        ("arena.rules", arena, None),
        ("red.rules", "budget { rows 9 }", Some("red")),
    ]);
    assert!(e.contains("a player's pack declares no budget"), "{e}");
    let e = err(&[("a.rules", "budget { } budget { }", None)]);
    assert!(e.contains("one `budget` in a rule set"), "{e}");
    for (text, want) in [
        ("budget { fuel 0 }", "budget fuel 0: 1 to 32768"),
        ("budget { rows -1 }", "budget rows -1: 0 to"),
        ("budget { rows 1 rows 2 }", "given twice in the budget"),
        (
            "budget { speed 1 }",
            "expected `fuel`, `rows`, `private` or `}` in a budget",
        ),
    ] {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
    // No players, no budget: the table stays empty and the hash as it was.
    let plain = compile_ok("kind k { glyph \"k\"  when true => idle }");
    assert_eq!(plain.players, crate::rules::Players::default());
}

/// Two arena packs that both declare `rain` and `cloud`: one in a
/// namespace keeps its own as `weather.rain`, plain inside its files; a
/// third file names it `weather.rain`. A namespace's kinds are the arena's,
/// not a player's. Tags and the engine's words stay global.
#[test]
fn a_namespace_keeps_an_arena_packs_names_apart() {
    let weather = "namespace weather
                   const WET = 3
                   field rain max 9 { every 1 { rain = min(rain + 1, WET) } }
                   ground water { walkable drink }
                   layer high { flies }
                   kind cloud { glyph \"c\"  tags sky  layer high  when rain(here) > 1 => idle  when true => move random free }";
    let farm = "field rain max 5
                kind cloud { glyph \"o\"  when true => idle }
                kind farmer { glyph \"f\"
                  when weather.rain(here) > weather.WET - 1 and rain(here) == 0 => idle
                  when nearest weather.cloud within 4 as c => move toward c
                  when nearest sky within 4 as c => move away c
                  when true => move random free }";
    let k = compile_owned(
        &[("weather.rules", weather, None), ("farm.rules", farm, None)],
        &[],
    )
    .unwrap();
    let mut names: Vec<&str> = k.names().collect();
    names.sort_unstable();
    assert_eq!(names, ["cloud", "farmer", "weather.cloud"]);
    let fields: Vec<&str> = k.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, ["weather.rain", "rain"]);
    assert_eq!(k.layers[0].name, "weather.high");
    assert!(
        k.materials
            .ground("water")
            .is_some_and(|g| k.materials.grounds[usize::from(g.0)].solid),
        "water redeclared, globally"
    );
    assert_eq!(
        k.players,
        crate::rules::Players::default(),
        "a namespace is no player"
    );

    let err =
        |files: &[(&str, &str, Option<&str>)]| compile_owned(files, &[]).unwrap_err().to_string();
    for (files, want) in [
        (
            vec![("a.rules", "namespace", None)],
            "expected a name after `namespace`",
        ),
        (
            vec![("a.rules", "namespace kind", None)],
            "`kind` can't name a namespace",
        ),
        (
            vec![("a.rules", "namespace food", None)],
            "`food` can't name a namespace",
        ),
        (
            vec![(
                "a.rules",
                "kind k { glyph \"k\" when true => idle } namespace w",
                None,
            )],
            "found `namespace`",
        ),
        (
            vec![
                (
                    "a.rules",
                    "namespace w  kind k { glyph \"k\" when true => idle }",
                    None,
                ),
                ("b.rules", "const w = 1", None),
            ],
            "`w` is a namespace",
        ),
        (
            vec![
                ("a.rules", "namespace red  const A = 1", None),
                (
                    "r.rules",
                    "kind k { glyph \"k\" when true => idle }",
                    Some("red"),
                ),
            ],
            "`red` is both a player and a namespace",
        ),
        (
            vec![(
                "r.rules",
                "namespace q  kind k { glyph \"k\" when true => idle }",
                Some("red"),
            )],
            "its pack declares no namespace",
        ),
        (
            vec![
                ("a.rules", "namespace w  field rain max 9", None),
                (
                    "b.rules",
                    "kind k { glyph \"k\" when rain(here) > 0 => idle }",
                    None,
                ),
            ],
            "rain",
        ),
    ] {
        let e = err(&files);
        assert!(e.contains(want), "{files:?}\n  got: {e}");
    }
}

/// `mem route[N]`: N slots named `route[0]` .. in a row; `route[i]` reads
/// and writes one, with any expression for i; what is past the ends reads
/// 0 and takes no write. Mistakes say so.
#[test]
fn memory_arrays_are_slots_in_a_row() {
    let k = compile_ok(
        "kind k { glyph \"k\"  mem a, route[3], b  when true => { route[a] = 5  b = route[2]  idle } }",
    );
    let d = k.by_name("k").unwrap();
    assert_eq!(d.mems, ["a", "route[0]", "route[1]", "route[2]", "b"]);
    for (text, want) in [
        ("kind k { mem r[0] }", "`r[N]`: N is a number, 1 to 32"),
        ("kind k { mem r[33] }", "`r[N]`: N is a number, 1 to 32"),
        ("kind k { mem r[2], r }", "`r` declared twice"),
        ("kind k { mem r[30], a, b, c }", "at most 32"),
        (
            "kind k { mem a  when true => { a[1] = 2  idle } }",
            "`a` is not a memory array",
        ),
        (
            "kind k { mem r[2]  when r > 0 => idle }",
            "unknown name `r`",
        ),
        (
            "kind k { mem r[2]  when true => { r[0] *= 2  idle } }",
            "expected `=`, `+=` or `-=` after `r[...]`",
        ),
        (
            "field f max 9 { every 1 { f = r[0] } }",
            "a law reads fields",
        ),
    ] {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
}

/// The lint names an unused memory array once, and an array read by index
/// is used.
#[test]
fn the_lint_sees_memory_arrays_whole() {
    let k = compile_ok(
        "kind k { glyph \"k\"  mem spare[3], used[2]  when used[0] > 0 => idle  when true => idle }",
    );
    let msgs: Vec<&str> = k.debug.diagnostics.iter().map(|d| d.msg.as_str()).collect();
    assert_eq!(
        msgs.iter()
            .filter(|m| m.contains("never used"))
            .copied()
            .collect::<Vec<_>>(),
        ["mem `spare` of kind `k` is never used"]
    );
}

/// A search says what it can't do, and where.
#[test]
fn search_errors_name_the_problem() {
    let ok = "search s { state a = 0  next { try a + 1 }  goal a > 2 }";
    assert!(compile(
        "t.rules",
        &format!("{ok} kind k {{ glyph \"k\"  when s() as p and p.steps > 0 => idle  when true => idle }}")
    )
    .is_ok());
    for (text, want) in [
        (
            "kind k { when true => { try 1  idle } }",
            "`try` belongs in a search's `next`",
        ),
        (
            "search s { state a, b = 0, 0  next { try a + 1 }  goal a > 2 }",
            "this search's state holds 2 numbers; the `try` gives 1",
        ),
        (
            "search s { state a = 0, 1  next { try a }  goal a > 2 }",
            "1 state names, 2 starting values",
        ),
        (
            "search s { state a, b, c, d, e, f, g, h, i = 0, 0, 0, 0, 0, 0, 0, 0, 0  next { try a }  goal a > 2 }",
            "at most 8 numbers",
        ),
        (
            "search s { state a = 0  goal a > 2 }",
            "needs a `state`, a `next` and a `goal`",
        ),
        (
            "search s { state a = 0  next { try a }  next { try a }  goal a > 2 }",
            "`next` given twice",
        ),
        (
            "search s { state a = 0  next { try a }  goal a > 2  speed 3 }",
            "expected `state`, `next`",
        ),
        (
            "search s { state a = 0  next { move north }  goal a > 2 }",
            "a search doesn't act",
        ),
        (
            "search s { state a = 0  next { return 1 }  goal a > 2 }",
            "doesn't `return`: it `try`s",
        ),
        (
            "sub t() { return 1 } kind k { when t() as p => idle }",
            "`t` is not a search",
        ),
        (
            "search s { state a = 0  next { try a + 1 }  goal a > 2 } kind k { when s() as p and p.dx > 0 => idle }",
            "`p` is a plan: `.via`, `.cost` or `.steps`",
        ),
        (
            "search s { state a = 0  next { try a + 1 }  goal a > 2 } kind k { when s() as p and p > 0 => idle }",
            "`p` is a plan",
        ),
        (
            "search s { state a = 0  next { try a + 1 }  goal a > 2 } kind k { when not s() as p => idle }",
            "must be a top-level conjunct",
        ),
        (
            "search s { state a = 0  next { try a + 1 }  goal a > 2 } kind k { mem m when s() as p into m => idle }",
            "`m` is not a memory array",
        ),
        (
            "field f max 9 { every 1 { if s() as p { f = 1 } } }",
            "a law doesn't search",
        ),
        ("kind k { mem search }", "reserved"),
    ] {
        let e = compile_err(text);
        assert!(e.contains(want), "{text}\n  got: {e}");
    }
}

/// Review finds, each fixed: a search whose `limit` acts is refused; a
/// namespace's or a player's `const steps` doesn't rename the `.steps` of a
/// plan; an array declared with two lengths is refused where it is used;
/// the guess at a `try` reads the search's names, not a `let` in `next`.
#[test]
fn search_and_rename_review_finds_stay_fixed() {
    let e = compile_err(
        "sub mover() { move east  return 5 }
         search s { state a = 0  next { try a + 1 }  goal a == 3  limit mover() }
         kind k { glyph \"k\"  when true => { if s() { idle } else { idle } } }",
    );
    assert!(e.contains("a search doesn't act"), "{e}");

    let spaced = "namespace foo
                  const steps = 3
                  const cost = 2
                  search s { state a = 0  next { try a + 1 cost cost }  goal a >= steps }
                  kind k { glyph \"k\"  mem m  when s() as p and p.steps > 0 => { m = p.cost  idle }  when true => idle }";
    let k = compile("t.rules", spaced).unwrap();
    assert!(k.by_name("foo.k").is_some());
    let e = compile_owned(
        &[
            ("a.rules", "kind a { glyph \"a\" when true => idle }", None),
            ("r.rules", "const steps = 3", Some("red")),
        ],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("a word the language reads"), "{e}");

    let e = compile_err(
        "trait t { mem route[2], z }
         kind k extends t { glyph \"k\"  mem route[4]  when true => { route[3] = 7  idle } }",
    );
    assert!(e.contains("`route` is declared with two lengths"), "{e}");

    // The guess reads the constant gx, whatever `next` binds: from the
    // start and at each try alike (10 - a), so the plan is the same as
    // without the `let`.
    let plan = |next: &str| {
        let text = format!(
            "const gx = 10
             search s {{ state a = 0  next {{ {next} try a + 1 }}  goal a >= 4  guess gx - a }}
             kind k {{ glyph \"k\"  when s() as p => idle }}"
        );
        compile("t.rules", &text).unwrap().code
    };
    let with_let = plan("let gx = 0");
    let without = plan("");
    assert_eq!(
        with_let.iter().filter(|o| o.code == OpCode::Load).count(),
        without.iter().filter(|o| o.code == OpCode::Load).count(),
        "the guess loads no local `gx`"
    );
}
