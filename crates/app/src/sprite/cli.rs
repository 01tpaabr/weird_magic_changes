//! `wmc sprite render | score | fit`: the sprite tools, headless and
//! deterministic, on the rasterizer the game draws with (`docs/SPRITES.md`
//! §5). `main.rs` takes the flags off the command line; this runs them.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use sim_core::Kinds;

use super::score::{self, Target};
use super::{Pose, Sprite, SpriteFile, parse, rgb, sheet};

/// The flags of `wmc sprite`, as given (checked here).
#[derive(Debug, Default)]
pub struct Opts {
    pub png: Option<String>,
    pub sprite: Option<String>,
    pub pose: Option<String>,
    pub cell: Option<String>,
    pub bg: Option<String>,
    pub target: Option<String>,
    pub diff: Option<String>,
    pub key: Option<String>,
    /// `fit`'s: the kind, `-o`, `--stamps`, `--colors`, `--chars`,
    /// `--size W H`, `--seed`.
    pub kind: Option<String>,
    pub out: Option<String>,
    pub stamps: Option<String>,
    pub colors: Option<String>,
    pub chars: Option<String>,
    pub size: Option<(String, String)>,
    pub seed: Option<String>,
}

impl Opts {
    /// Any of them given?
    pub fn any(&self) -> bool {
        [
            &self.png,
            &self.sprite,
            &self.pose,
            &self.cell,
            &self.bg,
            &self.target,
            &self.diff,
            &self.key,
            &self.kind,
            &self.out,
            &self.stamps,
            &self.colors,
            &self.chars,
            &self.seed,
        ]
        .iter()
        .any(|o| o.is_some())
            || self.size.is_some()
    }
}

/// The most pixels a sheet may have: a 16 x 16 sprite at a big cell would
/// be gigabytes.
const MAX_SHEET_PIXELS: usize = 64 << 20;

/// `wmc sprite <command> ...`. `packs` are `--rules` (else `WMC_RULES`):
/// they give the kinds' colours and sizes; without, a sprite is drawn in
/// the colour of a kind that declares none, over the cells its stamps
/// reach.
pub fn run(args: &[String], packs: &[String], o: &Opts) -> anyhow::Result<()> {
    let command = match args.first().map(String::as_str) {
        Some(c @ ("render" | "score" | "fit")) => c,
        Some(other) => bail!("unknown sprite command {other:?}; use `render`, `score` or `fit`"),
        None => bail!("sprite needs a command: render, score or fit"),
    };
    let flag_for = |given: bool, flag: &str, only: &str| {
        if given && command != only {
            bail!("{flag} is only for sprite {only}");
        }
        Ok(())
    };
    flag_for(o.png.is_some(), "--png", "render")?;
    flag_for(o.pose.is_some(), "--pose", "render")?;
    flag_for(o.cell.is_some(), "--cell", "render")?;
    flag_for(o.target.is_some(), "--target", "score")?;
    flag_for(o.diff.is_some(), "--diff", "score")?;
    flag_for(o.key.is_some() && command == "render", "--key", "score")?;
    flag_for(o.sprite.is_some() && command == "fit", "--sprite", "render")?;
    flag_for(o.bg.is_some() && command == "fit", "--bg", "render")?;
    for (given, flag) in [
        (o.kind.is_some(), "--kind"),
        (o.out.is_some(), "-o"),
        (o.stamps.is_some(), "--stamps"),
        (o.colors.is_some(), "--colors"),
        (o.chars.is_some(), "--chars"),
        (o.size.is_some(), "--size"),
        (o.seed.is_some(), "--seed"),
    ] {
        flag_for(given, flag, "fit")?;
    }
    let file = args.get(1).with_context(|| {
        if command == "fit" {
            "sprite fit needs an image".to_string()
        } else {
            format!("sprite {command} needs a sprite file")
        }
    })?;
    if let Some(a) = args.get(2) {
        bail!("sprite {command} takes one file; unexpected {a:?}");
    }
    let colour = |flag: &str, v: &Option<String>| -> anyhow::Result<Option<u32>> {
        v.as_ref()
            .map(|b| rgb(b).with_context(|| format!("{flag} {b:?} is not a colour \"#rrggbb\"")))
            .transpose()
    };
    let bg = colour("--bg", &o.bg)?.unwrap_or(crate::render::palette::SOIL_BG.0);
    if command == "fit" {
        return fit(file, packs, o, colour("--key", &o.key)?);
    }
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
    let f = parse(file, &text)?;
    let kinds = {
        let packs = crate::packs(packs);
        if packs.is_empty() {
            None
        } else {
            Some(crate::compile(&packs)?)
        }
    };
    if command == "render" {
        return render(file, &f, kinds.as_ref(), o, bg);
    }
    let target = o
        .target
        .as_deref()
        .context("sprite score needs --target <image.png>")?;
    let key = colour("--key", &o.key)?;
    let s = match &o.sprite {
        Some(n) => find(file, &f, n)?,
        None if f.sprites.len() == 1 => &f.sprites[0],
        None if f.sprites.is_empty() => bail!("{file}: holds no sprite"),
        None => bail!(
            "{file} holds {} sprites: name one with --sprite ({})",
            f.sprites.len(),
            names(&f)
        ),
    };
    let t = Target::open(Path::new(target), key)?;
    let (size, kind) = looks(s, kinds.as_ref());
    let palette: Vec<u32> = f.palette.iter().map(|(_, c)| *c).collect();
    let pose = &s.poses[0];
    let sc = score::score(pose, &palette, kind, size, &t);
    let mut out = std::io::stdout().lock();
    let background = match t.key {
        Some(k) => format!("all but #{k:06x} and colours near it"),
        None => "its alpha".into(),
    };
    writeln!(
        out,
        "{} ({} x {}), first frame, against {target} ({} x {}, the subject: {background})",
        s.name, size.0, size.1, t.picture.width, t.picture.height
    )?;
    writeln!(
        out,
        "shape   {:.3}  silhouettes' overlap at {} px (1: the same outline)",
        sc.shape,
        score::CELL
    )?;
    writeln!(
        out,
        "colour  {:.3}  mean colour distance where both are, at {} px (0: the same)",
        sc.colour,
        score::CELL
    )?;
    writeln!(
        out,
        "detail  {:.3} {:.3}  the same two at {} px",
        sc.detail_shape,
        sc.detail_colour,
        score::DETAIL_CELL
    )?;
    writeln!(out, "stamps  {}", sc.stamps)?;
    writeln!(
        out,
        "value   {:.3}  what fit climbs: 0.9 (shape - colour / 4) + 0.1 (the same at {} px)",
        sc.value(),
        score::DETAIL_CELL
    )?;
    if let Some(d) = &o.diff {
        let pic = score::diff(pose, &palette, kind, size, &t, bg);
        sheet::write_png(&pic, Path::new(d))?;
        writeln!(
            out,
            "wrote {d}: image, sprite, difference at {} px (grey both, red only the image, blue only the sprite)",
            score::DETAIL_CELL
        )?;
    }
    Ok(())
}

/// `wmc sprite fit <image> --kind NAME -o out.sprite`: a first draft.
fn fit(image: &str, packs: &[String], o: &Opts, key: Option<u32>) -> anyhow::Result<()> {
    use super::fit::{CHARS, Options, fit};
    let kind = o.kind.as_deref().context("sprite fit needs --kind NAME")?;
    let out = o
        .out
        .as_deref()
        .context("sprite fit needs -o <out.sprite>")?;
    let count = |flag: &str, v: &Option<String>, lo: usize, hi: usize, default: usize| {
        v.as_ref().map_or(Ok(default), |n| {
            n.parse()
                .ok()
                .filter(|n| (lo..=hi).contains(n))
                .with_context(|| format!("{flag} needs a count from {lo} to {hi}"))
        })
    };
    let stamps = count("--stamps", &o.stamps, 1, super::MAX_STAMPS, 8)?;
    let colors = count("--colors", &o.colors, 1, 8, 3)?;
    let chars: Vec<u8> = o.chars.as_deref().unwrap_or(CHARS).bytes().collect();
    if chars.is_empty() || chars.iter().any(|c| !(b'!'..=b'~').contains(c)) {
        bail!("--chars needs printable ASCII characters, no space");
    }
    let seed: u64 = match &o.seed {
        Some(s) => s.parse().ok().context("--seed needs a whole number")?,
        None => 1,
    };
    // The size: --size, else the kind's in the rules, else one cell.
    let size = match &o.size {
        Some((w, h)) => {
            let side = |v: &str| {
                v.parse::<u8>()
                    .ok()
                    .filter(|n| (1..=16).contains(n))
                    .context("--size needs two counts from 1 to 16")
            };
            (side(w)?, side(h)?)
        }
        None => {
            let packs = crate::packs(packs);
            if packs.is_empty() {
                (1, 1)
            } else {
                let kinds = crate::compile(&packs)?;
                kinds
                    .by_name(kind)
                    .with_context(|| format!("--kind {kind}: the rules have no such kind"))?
                    .size
            }
        }
    };
    let target = Target::open(Path::new(image), key)?;
    let f = fit(
        &target,
        kind,
        &Options {
            stamps,
            colors,
            chars,
            size,
            seed,
        },
    );
    let shown = Path::new(image)
        .file_name()
        .map_or_else(|| image.to_string(), |n| n.to_string_lossy().into_owned());
    let mut text = format!(
        "# {kind}: drafted by wmc sprite fit from {shown} (seed {seed}, {} x {} cells);\n\
         # edit it by hand (the /sprite-fitting skill)\n",
        size.0, size.1
    );
    text.push_str(f.file.text().trim_start());
    std::fs::write(out, &text).with_context(|| format!("writing {out}"))?;
    let mut w = std::io::stdout().lock();
    for (i, v) in f.steps.iter().enumerate() {
        writeln!(w, "stamp {:>2}: value {v:.3}", i + 1)?;
    }
    let sc = f.score;
    writeln!(
        w,
        "shape {:.3}, colour {:.3}, detail {:.3} {:.3}, {} stamps, {} colours: wrote {out}",
        sc.shape,
        sc.colour,
        sc.detail_shape,
        sc.detail_colour,
        sc.stamps,
        f.file.palette.len()
    )?;
    Ok(())
}

fn names(f: &SpriteFile) -> String {
    f.sprites
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn find<'f>(file: &str, f: &'f SpriteFile, name: &str) -> anyhow::Result<&'f Sprite> {
    f.sprites
        .iter()
        .find(|s| s.name == name)
        .with_context(|| format!("{file}: no sprite `{name}` (it has: {})", names(f)))
}

/// A sprite's box and kind colour: its kind's, when the rules have it,
/// else the cells its stamps reach and the colour of a kind that declares
/// none.
fn looks(s: &Sprite, kinds: Option<&Kinds>) -> ((u8, u8), u32) {
    let def = kinds.and_then(|k| k.by_name(&s.name));
    (
        def.map_or_else(|| score::reach(s), |d| d.size),
        def.map_or(sim_core::rules::DEFAULT_COLOR, |d| d.color),
    )
}

/// `wmc sprite render <file> --png out`: a sheet of a sprite file's poses
/// and frames.
fn render(
    file: &str,
    f: &SpriteFile,
    kinds: Option<&Kinds>,
    o: &Opts,
    bg: u32,
) -> anyhow::Result<()> {
    let png = o
        .png
        .as_deref()
        .context("sprite render needs --png <out.png>")?;
    let cell: u32 = match &o.cell {
        Some(c) => c
            .parse()
            .ok()
            .filter(|c| (4..=256).contains(c))
            .context("--cell needs a size in pixels from 4 to 256")?,
        None => 24,
    };
    let pose: Option<u8> = o
        .pose
        .as_ref()
        .map(|v| v.parse().ok().context("--pose needs a look from 0 to 255"))
        .transpose()?;
    let chosen: Vec<&Sprite> = match &o.sprite {
        Some(n) => vec![find(file, f, n)?],
        None => f.sprites.iter().collect(),
    };
    if chosen.is_empty() {
        bail!("{file}: holds no sprite");
    }
    let palette: Vec<u32> = f.palette.iter().map(|(_, c)| *c).collect();
    let mut rows = Vec::new();
    let mut legend = Vec::new();
    for s in chosen {
        let (size, kind) = looks(s, kinds);
        let poses: Vec<&Pose> = match pose {
            Some(v) => vec![
                s.poses
                    .iter()
                    .find(|p| p.look == Some(v))
                    .unwrap_or(&s.poses[0]),
            ],
            None => s.poses.iter().collect(),
        };
        for p in poses {
            legend.push(format!(
                "{} ({} x {}): {}",
                s.name,
                size.0,
                size.1,
                sheet::caption(p)
            ));
            rows.push(sheet::Row {
                pose: p,
                palette: &palette,
                kind,
                size,
            });
        }
    }
    let c = cell as usize;
    let (w, h) = rows.iter().fold((0usize, 0usize), |(w, h), r| {
        let (rw, rh) = (usize::from(r.size.0) * c, usize::from(r.size.1) * c);
        (
            w.max(usize::from(r.pose.frames.n) * rw * (sheet::ENLARGE + 1)),
            h + rh * (sheet::ENLARGE + 1),
        )
    });
    if w.saturating_mul(h) > MAX_SHEET_PIXELS {
        bail!(
            "the sheet would be about {w} x {h} pixels: choose a smaller --cell, a --sprite or a --pose"
        );
    }
    let pic = sheet::sheet(&rows, cell, bg);
    sheet::write_png(&pic, Path::new(png))?;
    let mut out = std::io::stdout().lock();
    for (i, l) in legend.iter().enumerate() {
        writeln!(out, "row {}: {l}", i + 1)?;
    }
    writeln!(
        out,
        "wrote {png}: {} x {} pixels, cell {cell}, enlarged {}x below each row",
        pic.width,
        pic.height,
        sheet::ENLARGE
    )?;
    Ok(())
}
