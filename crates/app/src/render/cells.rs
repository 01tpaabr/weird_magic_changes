//! Phase 1 of a frame: the viewport as three flat per-cell buffers.
//!
//! Rows are the parallel unit: a band of [`ROW_BAND`] rows is one task on the
//! compute pool, writes only its own slices, reads only through the chunk
//! lookup, and does one lookup per (row, chunk) span, never per cell. No
//! reduction, so the result is a pure function of `(chunks, view, light)`
//! whatever the thread count. `light` (day/night) is applied here, per cell,
//! so the status bar and the GPU never see it.

use bevy::prelude::Resource;
use bevy::tasks::ComputeTaskPool;
use sim_core::{ActorId, CHUNK_SIZE, ChunkCells, ChunkCoord, Pos};

use super::palette::{Color, Looks, VOID, feature_look, fielded, ground_look, relieved, style_of};

/// Frame rows per task. 160 columns x 8 rows is ~10 KB of output per task;
/// small frames run on one task and skip the pool entirely.
pub const ROW_BAND: usize = 8;

/// Rectangle of the world to draw, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Top-left cell.
    pub origin: Pos,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    /// A viewport of `width x height` centred on `center`.
    pub fn centered(center: Pos, width: u32, height: u32) -> Self {
        let half = |n: u32| i32::try_from(n / 2).expect("viewport fits i32");
        Self {
            origin: Pos::new(
                center.x.saturating_sub(half(width)),
                center.y.saturating_sub(half(height)),
            ),
            width,
            height,
        }
    }
}

/// `cols x rows` cells, structure of arrays, row-major. Reused across frames;
/// only a size change reallocates.
#[derive(Resource, Debug, Default)]
pub struct CellFrame {
    cols: usize,
    rows: usize,
    /// Printable ASCII byte per cell.
    pub glyph: Vec<u8>,
    /// Packed [`Color`] per cell.
    pub fg: Vec<u32>,
    pub bg: Vec<u32>,
    /// Per cell, a sprite tile plus one (0: none; the glyph is blank where
    /// there is one).
    pub sprite: Vec<u16>,
    /// The light the map was drawn in ([`render_cells`]): sprites are
    /// dimmed by it as the colours are.
    pub light: u8,
}

impl CellFrame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let n = cols * rows;
        self.cols = cols;
        self.rows = rows;
        self.glyph.resize(n, VOID.glyph);
        self.fg.resize(n, VOID.fg.0);
        self.bg.resize(n, VOID.bg.0);
        self.sprite.resize(n, 0);
    }

    /// Write `text` into row `row` starting at column 0, padding the rest of
    /// the row with spaces. Rows outside the frame are ignored, text is
    /// clipped, non-ASCII bytes draw as `?`.
    pub fn put_text(&mut self, row: usize, text: &str, fg: Color, bg: Color) {
        if row >= self.rows {
            return;
        }
        let range = row * self.cols..(row + 1) * self.cols;
        let mut src = text.bytes().map(|b| {
            if b.is_ascii_graphic() || b == b' ' {
                b
            } else {
                b'?'
            }
        });
        for g in &mut self.glyph[range.clone()] {
            *g = src.next().unwrap_or(b' ');
        }
        self.fg[range.clone()].fill(fg.0);
        self.sprite[range.clone()].fill(0);
        self.bg[range].fill(bg.0);
    }

    /// [`Self::put_text`] with a colour per cell: `cells` from column 0,
    /// the rest of the row blank on `bg`.
    pub fn put_cells(&mut self, row: usize, cells: &[(u8, Color)], bg: Color) {
        if row >= self.rows {
            return;
        }
        let range = row * self.cols..(row + 1) * self.cols;
        let mut src = cells.iter();
        for (g, f) in self.glyph[range.clone()]
            .iter_mut()
            .zip(&mut self.fg[range.clone()])
        {
            let (b, c) = src.next().copied().unwrap_or((b' ', Color(bg.0)));
            *g = if b.is_ascii_graphic() || b == b' ' {
                b
            } else {
                b'?'
            };
            *f = c.0;
        }
        self.sprite[range.clone()].fill(0);
        self.bg[range].fill(bg.0);
    }
}

/// Fill the first `view.height` rows of `frame` from the chunks `chunk`
/// returns (`None` = not loaded, drawn as void), with every colour scaled by
/// `light` (255 = the palette as is, see [`super::palette::brightness`]).
/// `looks` is how each kind is drawn (`Looks::of(&kinds)`). `frame`
/// must already be `view.width` columns wide and at least `view.height` rows
/// tall (extra rows are left for the caller: status text).
pub fn render_cells<'c>(
    chunk: impl Fn(ChunkCoord) -> Option<&'c ChunkCells> + Sync,
    looks: Looks<'_>,
    view: Viewport,
    light: u8,
    frame: &mut CellFrame,
) {
    let cols = frame.cols;
    let rows = view.height as usize;
    assert_eq!(cols, view.width as usize, "frame width != viewport width");
    assert!(rows <= frame.rows, "viewport taller than frame");
    let n = cols * rows;
    frame.light = light;
    if n == 0 {
        return;
    }
    let band = cols * ROW_BAND;
    let bands = frame.glyph[..n]
        .chunks_mut(band)
        .zip(frame.fg[..n].chunks_mut(band))
        .zip(
            frame.bg[..n]
                .chunks_mut(band)
                .zip(frame.sprite[..n].chunks_mut(band)),
        )
        .enumerate();
    let chunk = &chunk;
    let render_band =
        move |b: usize, glyph: &mut [u8], fg: &mut [u32], bg: &mut [u32], sp: &mut [u16]| {
            let rows = glyph
                .chunks_mut(cols)
                .zip(fg.chunks_mut(cols))
                .zip(bg.chunks_mut(cols).zip(sp.chunks_mut(cols)));
            for (i, ((g, f), (bg, sp))) in rows.enumerate() {
                let out = Row {
                    glyph: g,
                    fg: f,
                    bg,
                    sprite: sp,
                };
                render_row(chunk, looks, view, light, b * ROW_BAND + i, out);
            }
        };
    if rows <= ROW_BAND {
        for (b, ((g, f), (bg, sp))) in bands {
            render_band(b, g, f, bg, sp);
        }
        return;
    }
    ComputeTaskPool::get().scope(|s| {
        for (b, ((g, f), (bg, sp))) in bands {
            s.spawn(async move { render_band(b, g, f, bg, sp) });
        }
    });
}

/// One frame row's outputs.
struct Row<'a> {
    glyph: &'a mut [u8],
    fg: &'a mut [u32],
    bg: &'a mut [u32],
    sprite: &'a mut [u16],
}

fn render_row<'c>(
    chunk: &(impl Fn(ChunkCoord) -> Option<&'c ChunkCells> + Sync),
    looks: Looks<'_>,
    view: Viewport,
    light: u8,
    row: usize,
    out: Row<'_>,
) {
    let Row {
        glyph,
        fg,
        bg,
        sprite,
    } = out;
    let w = i32::try_from(glyph.len()).expect("row width fits i32");
    let y = view
        .origin
        .y
        .saturating_add(i32::try_from(row).expect("row fits i32"));
    let x_end = view.origin.x.saturating_add(w);
    let mut x = view.origin.x;
    let mut i = 0usize;
    // The first field drawn in relief, if any: its index and max.
    let relief = looks
        .fields
        .iter()
        .position(|f| f.relief)
        .map(|f| (f, looks.fields[f].max));
    while x < x_end {
        let (cc, local) = Pos::new(x, y).split();
        let span_end = (cc.origin().x + CHUNK_SIZE).min(x_end);
        let n = (span_end - x) as usize;
        let (g_out, f_out, b_out) = (&mut glyph[i..i + n], &mut fg[i..i + n], &mut bg[i..i + n]);
        let s_out = &mut sprite[i..i + n];
        match chunk(cc) {
            Some(c) => {
                let cells = c.ground[local..local + n]
                    .iter()
                    .zip(&c.feature[local..local + n])
                    .zip(
                        c.occupant[local..local + n]
                            .iter()
                            .zip(&c.cover[local..local + n]),
                    );
                let outs = g_out.iter_mut().zip(f_out.iter_mut().zip(b_out.iter_mut()));
                for (k, (((&g, &f), (&o, &cover)), (go, (fo, bo)))) in cells.zip(outs).enumerate() {
                    // Who is on top here (the last declared layer that holds
                    // someone, else who stands), on what grows here, in
                    // which fields.
                    let o = c
                        .extra
                        .iter()
                        .rev()
                        .map(|l| l[local + k])
                        .find(|id| !id.is_none())
                        .unwrap_or(o);
                    let s = style_of(
                        ground_look(g, looks.soil, looks.materials),
                        feature_look(f, looks.materials),
                        looks.occupant(o),
                        looks.actor(cover),
                    );
                    let mut bg = fielded(s.bg, looks.fields, &c.fields, local + k);
                    if let Some((f, max)) = relief {
                        // The neighbours north-west and south-east, in any
                        // chunk; a cell where none is loaded reads as this one.
                        let x = x + i32::try_from(k).expect("a span of a chunk");
                        let own = i32::from(c.fields[f][local + k]);
                        let at = |dx: i32, dy: i32| {
                            let (cc, i) = Pos::new(x + dx, y + dy).split();
                            chunk(cc)
                                .and_then(|c| c.fields.get(f))
                                .map_or(own, |l| i32::from(l[i]))
                        };
                        bg = relieved(bg, at(-1, -1), at(1, 1), max);
                    }
                    *go = s.glyph;
                    *fo = s.fg.scaled(light).0;
                    *bo = bg.scaled(light).0;
                }
                // Sprites in a pass of their own: a world without any pays
                // one fill a span. The kind whose glyph would show (who is
                // on top, else the cover) shows its picture instead.
                s_out.fill(0);
                if !looks.sprites.is_empty() {
                    let rows = looks.rows.and_then(|r| (r.0)(cc));
                    for (k, (so, go)) in s_out.iter_mut().zip(g_out.iter_mut()).enumerate() {
                        let i = local + k;
                        let (who, layer) = top_of(c, i);
                        let Some((kind, slot)) = who.unpack() else {
                            continue;
                        };
                        if !looks.has_sprite(kind) {
                            continue;
                        }
                        let tile = if slot & ActorId::PART == 0 {
                            // An anchor: its row's look and frame offset.
                            let row = rows
                                .and_then(|r| r.get(usize::from(slot)))
                                .map(|r| (r.look, r.stagger));
                            looks.sprite_tile(kind, 0, 0, row)
                        } else {
                            // A part: its anchor's row, in this chunk or
                            // another (none if that one is not loaded: the
                            // own stamps then, which every part shares).
                            let (dx, dy) = (slot >> 4 & 15, slot & 15);
                            let at = Pos::new(
                                x + i32::try_from(k).expect("a span of a chunk") - i32::from(dx),
                                y - i32::from(dy),
                            );
                            let (acc, ai) = at.split();
                            let anchor = if acc == cc { Some(c) } else { chunk(acc) };
                            let row = anchor
                                .and_then(|a| layer.id(a, ai).row())
                                .filter(|&(k2, _)| k2 == kind)
                                .and_then(|(_, s)| {
                                    let r = if acc == cc {
                                        rows
                                    } else {
                                        looks.rows.and_then(|r| (r.0)(acc))
                                    };
                                    r?.get(usize::from(s)).map(|r| (r.look, r.stagger))
                                });
                            looks.sprite_tile(kind, dx, dy, row)
                        };
                        if tile != 0 {
                            *so = tile;
                            *go = b' ';
                        }
                    }
                }
            }
            None => {
                g_out.fill(VOID.glyph);
                s_out.fill(0);
                f_out.fill(VOID.fg.scaled(light).0);
                b_out.fill(VOID.bg.scaled(light).0);
            }
        }
        x = span_end;
        i += n;
    }
}

/// The layer an id was found in, to read the same layer elsewhere (a big
/// actor's anchor).
#[derive(Debug, Clone, Copy)]
enum Layer {
    Extra(usize),
    Stand,
    Cover,
}

impl Layer {
    fn id(self, c: &ChunkCells, i: usize) -> ActorId {
        match self {
            Layer::Extra(j) => c.extra.get(j).map_or(ActorId::NONE, |l| l[i]),
            Layer::Stand => c.occupant[i],
            Layer::Cover => c.cover[i],
        }
    }
}

/// Whose glyph shows on local cell `i`: the last declared layer that holds
/// someone, else who stands, else the cover; and the layer.
fn top_of(c: &ChunkCells, i: usize) -> (ActorId, Layer) {
    if let Some((j, l)) = c
        .extra
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| !l[i].is_none())
    {
        return (l[i], Layer::Extra(j));
    }
    if c.occupant[i].is_none() {
        (c.cover[i], Layer::Cover)
    } else {
        (c.occupant[i], Layer::Stand)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::palette::style;
    use bevy::ecs::world::World;
    use sim_core::{Kinds, Scenario, StageCells, sim, stage};

    fn world(w: u32, h: u32, seed: u64) -> World {
        sim::new_world(&Scenario {
            width: w,
            height: h,
            seed,
            ..Scenario::builtin()
        })
    }

    fn frame_with(world: &World, view: Viewport, light: u8) -> CellFrame {
        let mut f = CellFrame::new();
        f.resize(view.width as usize, view.height as usize + 2);
        let kinds = world.resource::<Kinds>();
        render_cells(
            |c| stage::chunk(world, c),
            Looks::of(kinds),
            view,
            light,
            &mut f,
        );
        f
    }

    #[test]
    fn parallel_bands_match_a_per_cell_walk_and_the_system_param_view() {
        let mut world = world(200, 150, 9);
        // Straddles chunk borders and unloaded space on every side; 97 rows
        // is many bands with a ragged last one.
        let view = Viewport::centered(Pos::new(30, 40), 173, 97);
        let a = frame_with(&world, view, 200);
        let kinds = world.resource::<Kinds>().clone();
        let looks = Looks::of(&kinds);
        let glyphs = kinds.glyphs.clone();
        let n = 173 * 97;
        let mut actors = 0;
        for r in 0..97usize {
            for c in 0..173usize {
                let p = Pos::new(view.origin.x + c as i32, view.origin.y + r as i32);
                let (cc, i) = p.split();
                let want = match stage::chunk(&world, cc) {
                    Some(ch) => {
                        actors += usize::from(!ch.occupant[i].is_none() || !ch.cover[i].is_none());
                        let top = ch.extra.iter().rev().map(|l| l[i]).find(|id| !id.is_none());
                        let s = style(
                            ch.ground[i],
                            ch.feature[i],
                            looks.occupant(top.unwrap_or(ch.occupant[i])),
                            looks.actor(ch.cover[i]),
                            looks.soil,
                        );
                        crate::render::palette::Style {
                            bg: fielded(s.bg, looks.fields, &ch.fields, i),
                            ..s
                        }
                    }
                    None => VOID,
                };
                let k = r * 173 + c;
                assert_eq!(a.glyph[k], want.glyph, "{p:?}");
                assert_eq!(a.fg[k], want.fg.scaled(200).0, "{p:?}");
                assert_eq!(a.bg[k], want.bg.scaled(200).0, "{p:?}");
            }
        }
        assert!(actors > 0, "worldgen seeds are drawn");
        assert_eq!(
            a.glyph[..n].iter().filter(|g| glyphs.contains(g)).count(),
            actors,
            "every actor draws its kind's glyph"
        );
        // Extra rows untouched.
        assert!(a.glyph[n..].iter().all(|&g| g == b' '));
        // The same picture through the read-only system param.
        let b = world
            .run_system_once(move |s: StageCells, k: Res<Kinds>| {
                let mut f = CellFrame::new();
                f.resize(173, 99);
                render_cells(|c| s.chunk(c), Looks::of(&k), view, 200, &mut f);
                f
            })
            .unwrap();
        assert_eq!(a.glyph, b.glyph);
        assert_eq!(a.fg, b.fg);
        assert_eq!(a.bg, b.bg);
    }

    use bevy::ecs::system::{Res, RunSystemOnce};

    /// A kind with a sprite draws its tile and a blank glyph; every other
    /// cell, and the whole frame without a sprite table, is as before.
    #[test]
    fn a_kind_with_a_sprite_draws_its_tile_and_no_glyph() {
        use crate::render::palette::{PoseTile, SpriteTile};
        let world = world(200, 150, 9);
        let view = Viewport::centered(Pos::new(60, 40), 120, 70);
        let plain = frame_with(&world, view, 200);
        assert!(plain.sprite.iter().all(|&s| s == 0), "no table, no sprites");
        let kinds = world.resource::<Kinds>();
        let chicken = kinds.by_name("chicken").unwrap().id;
        let mut table = vec![SpriteTile::default(); kinds.len()];
        table[usize::from(chicken)] = SpriteTile {
            poses: 1,
            count: 1,
            width: 1,
            height: 1,
        };
        let poses = [PoseTile {
            look: None,
            first: 6,
            frames: 1,
            every_ms: 0,
        }];
        let mut f = CellFrame::new();
        f.resize(120, 72);
        let looks = Looks {
            sprites: &table,
            poses: &poses,
            ..Looks::of(kinds)
        };
        render_cells(|c| stage::chunk(&world, c), looks, view, 200, &mut f);
        let mut drawn = 0;
        for k in 0..120 * 70 {
            if f.sprite[k] != 0 {
                assert_eq!(f.sprite[k], 7);
                assert_eq!(f.glyph[k], b' ');
                assert_eq!(plain.glyph[k], kinds.def(chicken).glyph);
                drawn += 1;
            } else {
                assert_eq!(f.glyph[k], plain.glyph[k]);
            }
            assert_eq!((f.fg[k], f.bg[k]), (plain.fg[k], plain.bg[k]));
        }
        assert!(drawn > 0, "the world has chickens in view");
        assert_eq!(f.light, 200);
        // Status rows never hold sprites.
        f.put_text(70, "status", Color(1), Color(2));
        assert!(f.sprite[70 * 120..71 * 120].iter().all(|&s| s == 0));
    }

    /// A sprite's pose follows its actor's `look`, its frame the clock
    /// passed in, offset by the actor's `stagger`; with no rows to read,
    /// every actor shows its own stamps from offset 0.
    #[test]
    fn a_sprite_changes_pose_with_look_and_frame_with_the_clock() {
        use crate::render::palette::{PoseTile, Rows, SpriteTile};
        use sim_core::{ChunkActors, Stage};
        let mut world = world(200, 150, 9);
        let chicken = world.resource::<Kinds>().by_name("chicken").unwrap().id;
        // Every other chicken shows look 1.
        let mut q = world.query::<&mut ChunkActors>();
        for mut a in q.iter_mut(&mut world) {
            for r in a.rows.iter_mut().filter(|r| r.kind == chicken) {
                r.look = u8::from(r.stagger % 2 == 0);
            }
        }
        let kinds = world.resource::<Kinds>();
        let mut table = vec![SpriteTile::default(); kinds.len()];
        table[usize::from(chicken)] = SpriteTile {
            poses: 1,
            count: 2,
            width: 1,
            height: 1,
        };
        let poses = [
            PoseTile {
                look: None,
                first: 0,
                frames: 4,
                every_ms: 100,
            },
            PoseTile {
                look: Some(1),
                first: 4,
                frames: 1,
                every_ms: 0,
            },
        ];
        let view = Viewport::centered(Pos::new(60, 40), 120, 70);
        let rows_of = |c: ChunkCoord| {
            world
                .resource::<Stage>()
                .entity(c)
                .and_then(|e| world.get::<ChunkActors>(e))
                .map(|a| a.rows.as_slice())
        };
        let draw = |clock_ms: u64, with_rows: bool| {
            let mut f = CellFrame::new();
            f.resize(120, 70);
            let looks = Looks {
                sprites: &table,
                poses: &poses,
                clock_ms,
                rows: with_rows.then_some(Rows(&rows_of)),
                ..Looks::of(kinds)
            };
            render_cells(|c| stage::chunk(&world, c), looks, view, 255, &mut f);
            f
        };
        let (mut tucked, mut walking) = (0, 0);
        for clock in [0u64, 100, 250, 1_000_000_123] {
            let f = draw(clock, true);
            for k in 0..120 * 70 {
                if f.sprite[k] == 0 {
                    continue;
                }
                let p = Pos::new(
                    view.origin.x + (k % 120) as i32,
                    view.origin.y + (k / 120) as i32,
                );
                let (cc, i) = p.split();
                let (_, slot) = stage::chunk(&world, cc).unwrap().occupant[i].row().unwrap();
                let row = rows_of(cc).unwrap()[usize::from(slot)];
                let want = if row.look == 1 {
                    tucked += 1;
                    5
                } else {
                    walking += 1;
                    1 + ((clock / 100 + u64::from(row.stagger)) % 4) as u16
                };
                assert_eq!(f.sprite[k], want, "{p:?} at {clock} ms");
            }
            // Without rows: the own stamps, from offset 0, for everyone.
            let bare = draw(clock, false);
            let want = 1 + ((clock / 100) % 4) as u16;
            assert!(bare.sprite.iter().all(|&s| s == 0 || s == want));
            assert_eq!(
                bare.sprite.iter().filter(|&&s| s != 0).count(),
                f.sprite.iter().filter(|&&s| s != 0).count()
            );
        }
        assert!(tucked > 0 && walking > 0, "{tucked} {walking}");
        // The frames advance: two clocks a frame apart differ somewhere.
        assert_ne!(draw(0, true).sprite, draw(100, true).sprite);
    }

    /// A big actor is one picture over its footprint: each part shows its
    /// slice of its anchor's pose and frame, across a chunk border; with
    /// the anchor's chunk away, the parts left show the own stamps from
    /// offset 0, all alike; with a part's chunk away, the rest stay whole.
    #[test]
    fn a_big_sprite_is_whole_across_a_chunk_border() {
        use crate::render::palette::{PoseTile, Rows, SpriteTile};
        use sim_core::{CHUNK_CELLS, ChunkActors, Stage};
        let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/giants");
        let kinds = sim_core::rules::compile_packs(&[pack.as_path()]).unwrap();
        // Mammoths start inside chunks (a start may not cross a border) and
        // walk until one stands across one.
        let scenario = Scenario::parse(
            "t",
            "seed 1\nsize 128 128\nterrain water_level 0 rock_on_soil 0 rock_on_water 0\nstart mammoth 1 / 40 in (48, 48) to (76, 76)\n",
        )
        .unwrap();
        let mammoth = kinds.by_name("mammoth").unwrap().id;
        // A mammoth's anchor across a border and its row's slot.
        let across = |world: &mut World| -> Option<(Pos, u16)> {
            let mut q = world.query::<(&ChunkCoord, &ChunkCells)>();
            let mut found: Vec<(Pos, u16)> = q
                .iter(world)
                .flat_map(|(cc, c)| {
                    (0..CHUNK_CELLS).filter_map(move |i| {
                        let (k, slot) = c.occupant[i].row()?;
                        let p = cc.cell(i);
                        (k == mammoth && (p.x.rem_euclid(64) >= 62 || p.y.rem_euclid(64) >= 62))
                            .then_some((p, slot))
                    })
                })
                .collect();
            found.sort_by_key(|(p, _)| (p.y, p.x));
            found.first().copied()
        };
        let walked = || {
            let mut w = sim::new_world_with(&scenario, kinds.clone()).unwrap();
            for _ in 0..4000 {
                if let Some(at) = across(&mut w) {
                    return (w, at);
                }
                sim::step(&mut w);
            }
            panic!("no mammoth crossed a border in 4000 ticks");
        };
        let set_look = |world: &mut World, look: u8| {
            let mut q = world.query::<&mut ChunkActors>();
            for mut a in q.iter_mut(world) {
                for r in a.rows.iter_mut().filter(|r| r.kind == mammoth) {
                    r.look = look;
                }
            }
        };
        let n = kinds.len();
        let mut table = vec![SpriteTile::default(); n];
        table[usize::from(mammoth)] = SpriteTile {
            poses: 1,
            count: 2,
            width: 3,
            height: 3,
        };
        let poses = [
            PoseTile {
                look: None,
                first: 0,
                frames: 4,
                every_ms: 100,
            },
            PoseTile {
                look: Some(1),
                first: 36,
                frames: 1,
                every_ms: 0,
            },
        ];
        // The nine cells' tiles over the anchor at `p`, row by row (0: none).
        let nine = |world: &World, p: Pos, clock_ms: u64| -> Vec<u16> {
            let rows_of = |c: ChunkCoord| {
                world
                    .resource::<Stage>()
                    .entity(c)
                    .and_then(|e| world.get::<ChunkActors>(e))
                    .map(|a| a.rows.as_slice())
            };
            let view = Viewport {
                origin: Pos::new(p.x - 6, p.y - 6),
                width: 16,
                height: 16,
            };
            let mut f = CellFrame::new();
            f.resize(16, 16);
            let looks = Looks {
                sprites: &table,
                poses: &poses,
                clock_ms,
                rows: Some(Rows(&rows_of)),
                ..Looks::of(world.resource::<Kinds>())
            };
            render_cells(|c| stage::chunk(world, c), looks, view, 255, &mut f);
            (0..3)
                .flat_map(|dy| (0..3).map(move |dx| (6 + dy) * 16 + 6 + dx))
                .map(|k| f.sprite[k])
                .collect()
        };
        let slices = |first: u16| (0..9).map(|i| first + i + 1).collect::<Vec<u16>>();
        let chunk_of = |p: Pos, i: usize| {
            Pos::new(p.x + (i % 3) as i32, p.y + (i / 3) as i32)
                .split()
                .0
        };
        let (mut world, (p, slot)) = walked();
        let (anchor, part) = (chunk_of(p, 0), chunk_of(p, 8));
        assert_ne!(anchor, part, "it stands across a border");
        // Look 1: the pose's nine slices, over two or four chunks.
        set_look(&mut world, 1);
        assert_eq!(nine(&world, p, 0), slices(36));
        // Look 0: the anchor's frame on every part, whatever the clock.
        set_look(&mut world, 0);
        let rows = world
            .get::<ChunkActors>(world.resource::<Stage>().entity(anchor).unwrap())
            .unwrap();
        let stagger = rows.rows[usize::from(slot)].stagger;
        for clock in [0u64, 100, 230, 999] {
            let frame = ((clock / 100 + u64::from(stagger)) % 4) as u16;
            assert_eq!(nine(&world, p, clock), slices(frame * 9), "at {clock} ms");
        }
        // The anchor's chunk away: its cells blank, the parts left on the
        // own stamps' frame clock / every (offset 0), alike.
        let (mut gone, _) = walked();
        set_look(&mut gone, 1);
        stage::remove(&mut gone, anchor).unwrap();
        let t = nine(&gone, p, 200);
        let own = slices(2 * 9);
        for i in 0..9 {
            let want = if chunk_of(p, i) == anchor { 0 } else { own[i] };
            assert_eq!(t[i], want, "cell {i}");
        }
        // A part's chunk away: the rest keep the anchor's look 1.
        set_look(&mut world, 1);
        stage::remove(&mut world, part).unwrap();
        let t = nine(&world, p, 200);
        let pose = slices(36);
        for i in 0..9 {
            let want = if chunk_of(p, i) == part { 0 } else { pose[i] };
            assert_eq!(t[i], want, "cell {i}");
        }
    }

    #[test]
    fn light_scales_every_colour_and_nothing_else() {
        let world = world(100, 100, 4);
        let view = Viewport::centered(Pos::new(90, 90), 40, 30); // includes unloaded cells
        let day = frame_with(&world, view, 255);
        let dusk = frame_with(&world, view, 128);
        let night = frame_with(&world, view, 0);
        assert_eq!(day.glyph, dusk.glyph);
        assert_eq!(day.glyph, night.glyph);
        let n = 40 * 30;
        assert!(night.fg[..n].iter().all(|&c| c == 0));
        assert!(night.bg[..n].iter().all(|&c| c == 0));
        for (&d, &k) in day.bg[..n].iter().zip(&dusk.bg[..n]) {
            assert_eq!(Color(d).scaled(128), Color(k));
        }
        assert_ne!(day.bg, dusk.bg);
    }

    #[test]
    fn put_text_pads_clips_and_ignores_bad_rows() {
        let mut f = CellFrame::new();
        f.resize(4, 2);
        f.put_text(1, "ab\u{e9}cdef", Color(1), Color(2));
        assert_eq!(&f.glyph[4..], b"ab??");
        assert_eq!(&f.fg[4..], &[1, 1, 1, 1]);
        f.put_text(0, "x", Color(3), Color(4));
        assert_eq!(&f.glyph[..4], b"x   ");
        f.put_text(7, "nope", Color(0), Color(0));
        assert_eq!(&f.glyph[..4], b"x   ");
    }

    #[test]
    fn empty_and_tiny_viewports() {
        let world = world(64, 64, 1);
        let f = frame_with(&world, Viewport::centered(Pos::new(0, 0), 0, 0), 255);
        assert!(f.glyph.is_empty());
        let f = frame_with(&world, Viewport::centered(Pos::new(5, 5), 1, 1), 255);
        assert_eq!(f.glyph.len(), 3);
        assert_ne!(f.glyph[0], VOID.glyph);
    }

    #[test]
    fn centered_viewport() {
        let v = Viewport::centered(Pos::new(10, 10), 7, 3);
        assert_eq!(v.origin, Pos::new(7, 9));
    }
}
