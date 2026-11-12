//! `*.sprite` text to a [`SpriteFile`]: a lexer and a recursive-descent
//! parser, the grammar in `docs/SPRITES.md` §10. Every error carries the
//! position of the token it is about (`file:line:col: message`).

use super::{
    Frames, Ink, MAX_EVERY_MS, MAX_FRAMES, MAX_PALETTE, MAX_POSES, MAX_SCALE, MAX_STAMPS,
    MAX_SWING_TURN, MAX_TURN, MAX_XY, MIN_EVERY_MS, MIN_SCALE, Pose, Sprite, SpriteError,
    SpriteFile, Stamp,
};

/// A number's unit: none, milliseconds or seconds (real time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    None,
    Ms,
    S,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Name(String),
    /// A number in thousandths (`0.3` is 300), whether it had a decimal
    /// point, and its unit.
    Num {
        milli: i64,
        point: bool,
        unit: Unit,
    },
    Str(String),
    Open,
    Close,
    Tilde,
    Eof,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: u32,
    col: u32,
}

/// The largest whole part a number may have: far above every limit, far
/// below overflow in thousandths.
const MAX_WHOLE: i64 = 999_999_999;

fn lex(file: &str, text: &str) -> Result<Vec<Token>, SpriteError> {
    let b = text.as_bytes();
    let (mut i, mut line, mut line_start) = (0usize, 1u32, 0usize);
    let mut out = Vec::new();
    let col = |i: usize, line_start: usize| u32::try_from(i - line_start + 1).unwrap_or(u32::MAX);
    let err = |line: u32, col: u32, msg: String| SpriteError {
        file: file.to_string(),
        line,
        col,
        msg,
    };
    while i < b.len() {
        let c = b[i];
        let at = col(i, line_start);
        match c {
            b'\n' => {
                i += 1;
                line = line.saturating_add(1);
                line_start = i;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' | b'}' | b'~' => {
                let tok = match c {
                    b'{' => Tok::Open,
                    b'}' => Tok::Close,
                    _ => Tok::Tilde,
                };
                out.push(Token { tok, line, col: at });
                i += 1;
            }
            b'"' | b'\'' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != c && b[j] != b'\n' {
                    j += 1;
                }
                if j >= b.len() || b[j] != c {
                    return Err(err(line, at, "unterminated string".into()));
                }
                // Both ends sit on an ASCII quote: a whole UTF-8 slice.
                out.push(Token {
                    tok: Tok::Str(text[start..j].to_string()),
                    line,
                    col: at,
                });
                i = j + 1;
            }
            b'-' | b'0'..=b'9' => {
                let neg = c == b'-';
                let mut j = i + usize::from(neg);
                if j >= b.len() || !b[j].is_ascii_digit() {
                    return Err(err(line, at, "a `-` must start a number".into()));
                }
                let mut whole: i64 = 0;
                while j < b.len() && b[j].is_ascii_digit() {
                    whole = whole * 10 + i64::from(b[j] - b'0');
                    if whole > MAX_WHOLE {
                        return Err(err(line, at, "number too large".into()));
                    }
                    j += 1;
                }
                let mut milli = whole * 1000;
                let point = j < b.len() && b[j] == b'.';
                if point {
                    j += 1;
                    let mut digits = 0;
                    let mut frac: i64 = 0;
                    while j < b.len() && b[j].is_ascii_digit() {
                        digits += 1;
                        if digits > 3 {
                            return Err(err(line, at, "at most 3 decimals".into()));
                        }
                        frac = frac * 10 + i64::from(b[j] - b'0');
                        j += 1;
                    }
                    if digits == 0 {
                        return Err(err(
                            line,
                            at,
                            "a decimal point needs digits after it".into(),
                        ));
                    }
                    for _ in digits..3 {
                        frac *= 10;
                    }
                    milli += frac;
                }
                let u0 = j;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.')
                {
                    j += 1;
                }
                let unit = match &text[u0..j] {
                    "" => Unit::None,
                    "ms" => Unit::Ms,
                    "s" => Unit::S,
                    u => {
                        return Err(err(line, at, format!("unknown unit `{u}` (ms, s)")));
                    }
                };
                out.push(Token {
                    tok: Tok::Num {
                        milli: if neg { -milli } else { milli },
                        point,
                        unit,
                    },
                    line,
                    col: at,
                });
                i = j;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.')
                {
                    i += 1;
                }
                let name = &text[start..i];
                if name.split('.').any(str::is_empty) {
                    return Err(err(line, at, format!("bad name `{name}`")));
                }
                out.push(Token {
                    tok: Tok::Name(name.to_string()),
                    line,
                    col: at,
                });
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or('?');
                return Err(err(
                    line,
                    at,
                    format!("unexpected character `{}`", ch.escape_debug()),
                ));
            }
        }
    }
    out.push(Token {
        tok: Tok::Eof,
        line,
        col: col(i, line_start),
    });
    Ok(out)
}

struct Parser<'a> {
    file: &'a str,
    tokens: Vec<Token>,
    at: usize,
    /// Palette names used by stamps, resolved when the file ends: the
    /// stamp (sprite, pose, stamp index) and where the name was.
    pending: Vec<(usize, usize, usize, String, u32, u32)>,
}

/// Parse one sprite file. `file` labels errors.
pub fn parse(file: &str, text: &str) -> Result<SpriteFile, SpriteError> {
    let tokens = lex(file, text)?;
    let mut p = Parser {
        file,
        tokens,
        at: 0,
        pending: Vec::new(),
    };
    p.file()
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.at.min(self.tokens.len() - 1)]
    }

    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if self.at < self.tokens.len() - 1 {
            self.at += 1;
        }
        t
    }

    fn err_at(&self, t: &Token, msg: impl Into<String>) -> SpriteError {
        SpriteError {
            file: self.file.to_string(),
            line: t.line,
            col: t.col,
            msg: msg.into(),
        }
    }

    fn what(t: &Tok) -> String {
        match t {
            Tok::Name(n) => format!("`{n}`"),
            Tok::Num { .. } => "a number".into(),
            Tok::Str(s) => format!("\"{s}\""),
            Tok::Open => "`{`".into(),
            Tok::Close => "`}`".into(),
            Tok::Tilde => "`~`".into(),
            Tok::Eof => "the end of the file".into(),
        }
    }

    fn expect(&mut self, want: &Tok, what: &str) -> Result<Token, SpriteError> {
        let t = self.next();
        if t.tok == *want {
            Ok(t)
        } else {
            Err(self.err_at(&t, format!("expected {what}, found {}", Self::what(&t.tok))))
        }
    }

    fn name(&mut self, what: &str) -> Result<(String, Token), SpriteError> {
        let t = self.next();
        match &t.tok {
            Tok::Name(n) => Ok((n.clone(), t.clone())),
            other => Err(self.err_at(&t, format!("expected {what}, found {}", Self::what(other)))),
        }
    }

    /// A number in thousandths, in `lo..=hi` (thousandths too).
    fn decimal(&mut self, what: &str, lo: i64, hi: i64) -> Result<i32, SpriteError> {
        let t = self.next();
        match t.tok {
            Tok::Num {
                milli,
                unit: Unit::None,
                ..
            } if (lo..=hi).contains(&milli) => {
                Ok(i32::try_from(milli).expect("in a range far inside i32"))
            }
            Tok::Num {
                unit: Unit::None, ..
            } => Err(self.err_at(
                &t,
                format!(
                    "{what} must be from {} to {}",
                    milli_text(lo),
                    milli_text(hi)
                ),
            )),
            Tok::Num { .. } => Err(self.err_at(&t, format!("{what} takes no unit"))),
            ref other => Err(self.err_at(
                &t,
                format!("expected {what} (a number), found {}", Self::what(other)),
            )),
        }
    }

    /// A whole number in `lo..=hi`.
    fn whole(&mut self, what: &str, lo: i64, hi: i64) -> Result<i32, SpriteError> {
        let t = self.next();
        match t.tok {
            Tok::Num {
                milli,
                point: false,
                unit: Unit::None,
            } if (lo * 1000..=hi * 1000).contains(&milli) => {
                Ok(i32::try_from(milli / 1000).expect("in a range far inside i32"))
            }
            Tok::Num {
                point: false,
                unit: Unit::None,
                ..
            } => Err(self.err_at(&t, format!("{what} must be from {lo} to {hi}"))),
            Tok::Num {
                unit: Unit::None, ..
            } => Err(self.err_at(&t, format!("{what} is a whole number"))),
            Tok::Num { .. } => Err(self.err_at(&t, format!("{what} takes no unit"))),
            ref other => Err(self.err_at(
                &t,
                format!(
                    "expected {what} (a whole number), found {}",
                    Self::what(other)
                ),
            )),
        }
    }

    fn file(&mut self) -> Result<SpriteFile, SpriteError> {
        let mut f = SpriteFile::default();
        let mut palette_at: Vec<Token> = Vec::new();
        loop {
            let t = self.next();
            match &t.tok {
                Tok::Eof => break,
                Tok::Name(k) if k == "palette" => {
                    self.expect(&Tok::Open, "`{` after `palette`")?;
                    loop {
                        if self.peek().tok == Tok::Close {
                            self.next();
                            break;
                        }
                        let (name, at) = self.name("a colour's name or `}`")?;
                        if name.contains('.') {
                            return Err(
                                self.err_at(&at, format!("`{name}`: a colour's name has no `.`"))
                            );
                        }
                        if f.palette.iter().any(|(n, _)| *n == name) {
                            return Err(self.err_at(&at, format!("colour `{name}` is named twice")));
                        }
                        if f.palette.len() >= MAX_PALETTE {
                            return Err(self.err_at(
                                &at,
                                format!("at most {MAX_PALETTE} colours in a file's palette"),
                            ));
                        }
                        let c = self.next();
                        let Tok::Str(s) = &c.tok else {
                            return Err(self.err_at(
                                &c,
                                format!(
                                    "expected a colour \"#rrggbb\" after `{name}`, found {}",
                                    Self::what(&c.tok)
                                ),
                            ));
                        };
                        let rgb = rgb(s).ok_or_else(|| {
                            self.err_at(&c, format!("\"{s}\" is not a colour \"#rrggbb\""))
                        })?;
                        f.palette.push((name, rgb));
                        palette_at.push(at);
                    }
                }
                Tok::Name(k) if k == "sprite" => {
                    let (name, at) = self.name("the name of the kind the sprite draws")?;
                    if f.sprites.iter().any(|s| s.name == name) {
                        return Err(self
                            .err_at(&at, format!("sprite `{name}` is drawn twice in this file")));
                    }
                    let open = self.expect(&Tok::Open, "`{` after the sprite's name")?;
                    let si = f.sprites.len();
                    f.sprites.push(Sprite {
                        name,
                        line: at.line,
                        col: at.col,
                        poses: vec![Pose::default()],
                    });
                    self.body(&mut f, si, 0, &open)?;
                    let s = &f.sprites[si];
                    if s.poses.len() > MAX_POSES {
                        return Err(self.err_at(
                            &at,
                            format!(
                                "at most {MAX_POSES} poses a sprite, its own stamps counted as one: `{}` has {}",
                                s.name,
                                s.poses.len()
                            ),
                        ));
                    }
                    // A pose without its own `frames` line takes the sprite's.
                    let own = f.sprites[si].poses[0].frames;
                    for p in &mut f.sprites[si].poses[1..] {
                        if !p.own_frames {
                            p.frames = own;
                        }
                    }
                }
                other => {
                    return Err(self.err_at(
                        &t,
                        format!(
                            "expected `palette` or `sprite`, found {}",
                            Self::what(other)
                        ),
                    ));
                }
            }
        }
        for (si, pi, k, name, line, col) in std::mem::take(&mut self.pending) {
            let i = f
                .palette
                .iter()
                .position(|(n, _)| *n == name)
                .ok_or_else(|| SpriteError {
                    file: self.file.to_string(),
                    line,
                    col,
                    msg: format!("no colour `{name}` in this file's palette"),
                })?;
            f.sprites[si].poses[pi].stamps[k].color =
                Ink::Named(u8::try_from(i).expect("at most MAX_PALETTE colours"));
        }
        Ok(f)
    }

    /// A sprite's body (`pose == 0`) or a pose's, up to its `}`.
    fn body(
        &mut self,
        f: &mut SpriteFile,
        si: usize,
        pi: usize,
        open: &Token,
    ) -> Result<(), SpriteError> {
        loop {
            let t = self.next();
            match &t.tok {
                Tok::Close => return Ok(()),
                Tok::Eof => {
                    return Err(self.err_at(
                        open,
                        if pi == 0 {
                            "this sprite's `{` is never closed"
                        } else {
                            "this pose's `{` is never closed"
                        },
                    ));
                }
                Tok::Name(k) if k == "frames" => {
                    if f.sprites[si].poses[pi].own_frames {
                        return Err(self.err_at(&t, "`frames` given twice"));
                    }
                    let n = self.whole("the number of frames", 1, i64::from(MAX_FRAMES))?;
                    let n = u8::try_from(n).expect("at most MAX_FRAMES");
                    let mut every_ms = 0;
                    if matches!(&self.peek().tok, Tok::Name(e) if e == "every") {
                        self.next();
                        every_ms = self.time()?;
                    } else if n > 1 {
                        return Err(self.err_at(
                            &t,
                            format!(
                                "`frames {n}` needs `every T`: how long each is shown (200ms, 1s)"
                            ),
                        ));
                    }
                    let p = &mut f.sprites[si].poses[pi];
                    p.frames = Frames { n, every_ms };
                    p.own_frames = true;
                }
                Tok::Name(k) if k == "pose" => {
                    if pi != 0 {
                        return Err(self.err_at(&t, "a pose holds no pose"));
                    }
                    let v = self.whole("a pose's look", 0, 255)?;
                    let v = u8::try_from(v).expect("0 to 255");
                    if f.sprites[si].poses[1..].iter().any(|p| p.look == Some(v)) {
                        return Err(self.err_at(&t, format!("pose {v} is drawn twice")));
                    }
                    let open = self.expect(&Tok::Open, "`{` after the pose's look")?;
                    f.sprites[si].poses.push(Pose {
                        look: Some(v),
                        ..Pose::default()
                    });
                    let p = f.sprites[si].poses.len() - 1;
                    self.body(f, si, p, &open)?;
                }
                Tok::Str(s) => {
                    let ch = match s.as_bytes() {
                        [c] if (b' '..=b'~').contains(c) => *c,
                        _ => {
                            return Err(self.err_at(
                                &t,
                                format!("\"{s}\": a stamp is one printable ASCII character"),
                            ));
                        }
                    };
                    let n = f.sprites[si].poses[pi].stamps.len();
                    if n >= MAX_STAMPS {
                        return Err(self.err_at(&t, format!("at most {MAX_STAMPS} stamps a pose")));
                    }
                    let stamp = self.stamp(ch, si, pi, n)?;
                    f.sprites[si].poses[pi].stamps.push(stamp);
                }
                other => {
                    return Err(self.err_at(
                        &t,
                        format!(
                            "expected a stamp (\"C\" at X Y), `frames`, {}`}}`, found {}",
                            if pi == 0 { "`pose` or " } else { "" },
                            Self::what(other)
                        ),
                    ));
                }
            }
        }
    }

    /// `every`'s time, in milliseconds.
    fn time(&mut self) -> Result<u32, SpriteError> {
        let t = self.next();
        let ms = match t.tok {
            Tok::Num {
                milli,
                unit: Unit::Ms,
                point: false,
            } => milli / 1000,
            Tok::Num {
                milli,
                unit: Unit::S,
                ..
            } => milli,
            Tok::Num {
                unit: Unit::Ms,
                point: true,
                ..
            } => return Err(self.err_at(&t, "milliseconds are whole")),
            Tok::Num { .. } => {
                return Err(self.err_at(&t, "`every` takes a time with its unit: 200ms, 1s"));
            }
            ref other => {
                return Err(self.err_at(
                    &t,
                    format!("expected a time (200ms, 1s), found {}", Self::what(other)),
                ));
            }
        };
        if !(i64::from(MIN_EVERY_MS)..=i64::from(MAX_EVERY_MS)).contains(&ms) {
            return Err(self.err_at(
                &t,
                format!(
                    "`every` must be from {MIN_EVERY_MS}ms to {}s",
                    MAX_EVERY_MS / 1000
                ),
            ));
        }
        Ok(u32::try_from(ms).expect("in range"))
    }

    fn stamp(&mut self, ch: u8, si: usize, pi: usize, k: usize) -> Result<Stamp, SpriteError> {
        let a = self.next();
        if a.tok != Tok::Name("at".into()) {
            return Err(self.err_at(
                &a,
                format!(
                    "expected `at X Y` after the stamp's character, found {}",
                    Self::what(&a.tok)
                ),
            ));
        }
        let x = self.decimal("x", 0, i64::from(MAX_XY))?;
        let y = self.decimal("y", 0, i64::from(MAX_XY))?;
        let mut s = Stamp {
            ch,
            x,
            y,
            ..Stamp::default()
        };
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            let t = self.peek().clone();
            let word: &'static str = match &t.tok {
                Tok::Tilde => {
                    self.next();
                    let (w, at) = self.name("`turn`, `x`, `y` or `scale` after `~`")?;
                    match w.as_str() {
                        "turn" => "~turn",
                        "x" => "~x",
                        "y" => "~y",
                        "scale" => "~scale",
                        _ => {
                            return Err(self.err_at(
                                &at,
                                format!("`~{w}`: a swing is `~turn`, `~x`, `~y` or `~scale`"),
                            ));
                        }
                    }
                }
                Tok::Name(w) => match w.as_str() {
                    "turn" => "turn",
                    "scale" => "scale",
                    "color" => "color",
                    "phase" => "phase",
                    _ => return Ok(s),
                },
                _ => return Ok(s),
            };
            if word.starts_with('~') {
                // The `~` and its word are taken already.
            } else {
                self.next();
            }
            if seen.contains(&word) {
                return Err(self.err_at(&t, format!("`{word}` given twice")));
            }
            seen.push(word);
            match word {
                "turn" => s.turn = self.whole("turn", -i64::from(MAX_TURN), i64::from(MAX_TURN))?,
                "scale" => {
                    s.scale = self.decimal("scale", i64::from(MIN_SCALE), i64::from(MAX_SCALE))?
                }
                "phase" => s.phase = self.whole("phase", 0, 360)?,
                "~turn" => s.swing.turn = self.whole("~turn", 0, i64::from(MAX_SWING_TURN))?,
                "~x" => s.swing.x = self.decimal("~x", 0, i64::from(MAX_XY))?,
                "~y" => s.swing.y = self.decimal("~y", 0, i64::from(MAX_XY))?,
                "~scale" => s.swing.scale = self.decimal("~scale", 0, i64::from(MAX_SCALE))?,
                _ => {
                    // color
                    let c = self.next();
                    match &c.tok {
                        Tok::Str(lit) => {
                            s.color = Ink::Rgb(rgb(lit).ok_or_else(|| {
                                self.err_at(&c, format!("\"{lit}\" is not a colour \"#rrggbb\""))
                            })?);
                        }
                        Tok::Name(n) => {
                            self.pending.push((si, pi, k, n.clone(), c.line, c.col));
                        }
                        other => {
                            return Err(self.err_at(
                                &c,
                                format!("expected a palette name or \"#rrggbb\" after `color`, found {}", Self::what(other)),
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// `"#rrggbb"` as `0xRRGGBB`.
pub fn rgb(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// Thousandths as a sprite file writes them: `0.3`, `-1.25`, `4`.
pub fn milli_text(v: i64) -> String {
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    let (whole, frac) = (a / 1000, a % 1000);
    if frac == 0 {
        return format!("{sign}{whole}");
    }
    let f = format!("{frac:03}");
    format!("{sign}{whole}.{}", f.trim_end_matches('0'))
}
