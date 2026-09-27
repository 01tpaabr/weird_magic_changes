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
        chunks,
        tags: &k.tag_bits,
        family_end: &k.family_end,
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
    assert!(compile_err("kind a { sight 40 }").contains("0 to 16"));
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
            "member subs come before the rules",
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
            "kind a { mem m\n when sniff s within 3 as m => idle }",
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
