//! The windowed game: a Bevy `App` around the sim.
//!
//! A frame (`Update`, one chain, in this order):
//! 1. `handle_input`: keys -> camera direction, clock/zoom toggles, pending save/step/reload/quit.
//! 2. `advance_camera`: glide the camera by the frame's real `dt`.
//! 3. `layout`: window size and DPI -> cell size, map/status grids; rebuild the
//!    glyph tileset and the tile layers when their shape changes.
//! 4. `stream_and_tick` (exclusive): load/unload chunks around the camera, run
//!    the ticks the [`SimClock`] owes (bounded by [`TICK_BUDGET`]), save/quit.
//! 5. `render_frame`: phase 1, chunks -> `CellFrame`, dimmed by the sim's daylight.
//! 6. `upload_tiles`: phase 2, `CellFrame` -> tilemap tile data + layer transforms.
//!
//! Then Bevy draws: two tilemap chunks per layer, one draw call each.
//!
//! The camera sits at the origin with the projection scaled so that **one
//! world unit is one physical pixel**; the layers are placed in window pixel
//! coordinates by [`Layer::place`]. Wall-clock time enters through `Time`
//! here and in [`SimClock`], never below.
//!
//! Keys: hold `w a s d` / arrows to glide (two keys = diagonal), Shift for
//! x4 speed; `space` pauses/resumes the sim; `.` runs one tick and pauses;
//! `[` / `]` slow down / speed up (1x .. 16x, max); `p` saves; `r` reloads the
//! rules (`WMC_RULES`, else `./rules`), the status bar says what changed;
//! `+`/`-` zoom; `q` / `Esc` / close saves and quits.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bevy::app::{TaskPoolOptions, TaskPoolPlugin};
use bevy::camera::Projection;
use bevy::log::{Level, LogPlugin};
use bevy::prelude::*;
use bevy::render::view::Msaa;
use bevy::sprite_render::{TilemapChunkTileData, update_tilemap_chunk_indices};
use bevy::window::{PresentMode, PrimaryWindow, WindowCloseRequested, WindowResolution};
use sim_core::Tick;
use sim_core::actors::{Tally, life};
use sim_core::{
    CHUNK_SIZE, ChunkActors, Kinds, LoadPolicy, Pos, SimConfig, StageCells, Store, StreamStats,
};
use sim_core::{Scenario, par, sim, time};

use crate::camera::{Input, ViewCamera};
use crate::clock::SimClock;
use crate::render::atlas::GlyphAtlas;
use crate::render::cells::{CellFrame, Viewport, render_cells};
use crate::render::grid::{self, Layer};
use crate::render::palette::{Looks, TEXT_BG, TEXT_FG, VOID_BG, brightness};

/// Cell edge in logical pixels at startup; multiplied by the window's scale
/// factor (2 on Retina) to get the physical cell the tileset is built for.
pub const DEFAULT_CELL_LOGICAL: u32 = 16;
const MIN_CELL_LOGICAL: u32 = 6;
const MAX_CELL_LOGICAL: u32 = 64;
const ZOOM_STEP: i32 = 2;
/// Rows reserved under the map for status text.
const STATUS_ROWS: usize = 4;
/// Longest frame time fed to the camera: a stall becomes a small step, not a leap.
const MAX_DT: f64 = 0.1;
/// Sim time per frame. The rest of a 60 Hz frame is for streaming and drawing;
/// at max speed this is how long each frame ticks for.
pub const TICK_BUDGET: Duration = Duration::from_millis(10);
const HELP: &str = "wasd/arrows move (shift x4) | space pause | . step | [ ] speed | p save | r reload rules | +/- zoom | q quit";

/// Open the world in `dir`, else create it from `scenario` (read from
/// `name`, for errors), and run the window until quit.
pub fn run(dir: &str, scenario: &Scenario, name: &str) -> anyhow::Result<()> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let mut app = App::new();
    let task_pool_options = par::threads_from_env()
        .map_or_else(TaskPoolOptions::default, TaskPoolOptions::with_num_threads);
    app.add_plugins(
        DefaultPlugins
            .set(TaskPoolPlugin { task_pool_options })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: format!("wmc - {}", store.dir().display()),
                    // Logical size (the backend applies the scale factor).
                    resolution: WindowResolution::new(1280, 800),
                    present_mode: PresentMode::AutoVsync,
                    ..default()
                }),
                // We save before quitting, so the close button is ours to handle.
                close_when_requested: false,
                ..default()
            })
            .set(LogPlugin {
                filter: "warn,app=info".into(),
                level: Level::INFO,
                ..default()
            }),
    );
    let kinds = crate::rules()?;
    let world = app.world_mut();
    sim::install_with(world, kinds);
    if !sim::open(world, &store).context("reading save")? {
        sim::create(world, scenario).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        store
            .write_meta(&sim::meta(world))
            .context("writing save meta")?;
    }
    let camera = camera_for(world, &store);
    app.insert_resource(store)
        .insert_resource(camera)
        .insert_resource(ClearColor(VOID_BG.tint()))
        .add_plugins(PlayPlugin);
    match app.run() {
        AppExit::Success => Ok(()),
        AppExit::Error(code) => bail!("exited with code {code}"),
    }
}

/// The saved camera, or the middle of the initial region.
pub fn camera_for(world: &World, store: &Store) -> ViewCamera {
    ViewCamera::load(store.dir()).unwrap_or_else(|| {
        let c = world.resource::<SimConfig>();
        ViewCamera::new(Pos::new(
            i32::try_from(c.initial_width / 2).unwrap_or(0),
            i32::try_from(c.initial_height / 2).unwrap_or(0),
        ))
    })
}

/// Everything the window adds on top of an installed sim world: the camera,
/// the clock, the frame pipeline. Expects `Store` and `ViewCamera` resources.
#[derive(Debug)]
pub struct PlayPlugin;

impl Plugin for PlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SimClock>()
            .init_resource::<Zoom>()
            .init_resource::<MoveInput>()
            .init_resource::<Pending>()
            .init_resource::<Notice>()
            .init_resource::<Layout>()
            .init_resource::<Frames>()
            .init_resource::<StreamInfo>()
            .init_resource::<Glyphs>()
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    handle_input,
                    advance_camera,
                    layout,
                    stream_and_tick,
                    render_frame,
                    upload_tiles,
                )
                    .chain()
                    // Bevy repacks changed tile data in `Update` too: run before
                    // it so tiles and transforms land in the same frame.
                    .before(update_tilemap_chunk_indices),
            );
    }
}

// ---- resources -----------------------------------------------------------------------------

/// Cell size the player asked for, in logical pixels.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zoom {
    pub cell_logical: u32,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            cell_logical: DEFAULT_CELL_LOGICAL,
        }
    }
}

impl Zoom {
    fn change(&mut self, delta: i32) {
        self.cell_logical = self
            .cell_logical
            .saturating_add_signed(delta)
            .clamp(MIN_CELL_LOGICAL, MAX_CELL_LOGICAL);
    }
}

/// Direction the movement keys push this frame.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct MoveInput(Input);

/// One-shot requests from input, consumed by the exclusive system.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Pending {
    save: bool,
    step: bool,
    reload: bool,
    quit: bool,
}

/// A message for the help row, shown until `until` (wall clock: the app's,
/// never the sim's).
#[derive(Resource, Debug, Default, Clone)]
struct Notice {
    text: String,
    until: Option<Instant>,
}

impl Notice {
    const FOR: Duration = Duration::from_secs(8);

    fn show(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.until = Some(Instant::now() + Self::FOR);
    }

    fn current(&self) -> Option<&str> {
        self.until
            .is_some_and(|t| Instant::now() < t)
            .then_some(self.text.as_str())
    }
}

/// Last streaming call that did something, for the status line.
#[derive(Resource, Debug, Default, Clone, Copy)]
struct StreamInfo(Option<StreamStats>);

/// The glyph tileset on the GPU and the physical cell size it was built for
/// (`0` = not built yet).
#[derive(Resource, Debug, Default)]
struct Glyphs {
    cell: u32,
    tileset: Handle<Image>,
}

/// The two cell frames of a frame: the map and the status rows.
#[derive(Resource, Debug, Default)]
struct Frames {
    map: CellFrame,
    status: CellFrame,
}

/// The tile layers on screen.
#[derive(Resource, Debug)]
struct Grids {
    map: Layer,
    status: Layer,
}

/// Where the map grid sits so that a fractional camera centre lands on the
/// middle of a `width x height` pixel area: the first visible cell and the
/// pixel shift of the grid (`0..cell`), plus how many cells cover the area.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MapLayout {
    pub origin: Pos,
    pub shift_x: usize,
    pub shift_y: usize,
    pub cols: usize,
    pub rows: usize,
}

pub fn map_layout(camera: &ViewCamera, cell: usize, width: usize, height: usize) -> MapLayout {
    let cell_i = cell as i64;
    let corner = |center: f64, extent: usize| -> (i64, usize) {
        let px = (center * cell as f64 - extent as f64 / 2.0).round() as i64;
        (px.div_euclid(cell_i), px.rem_euclid(cell_i) as usize)
    };
    let (ox, shift_x) = corner(camera.x, width);
    let (oy, shift_y) = corner(camera.y, height);
    MapLayout {
        origin: Pos::new(
            i32::try_from(ox).expect("camera keeps x in i32 range"),
            i32::try_from(oy).expect("camera keeps y in i32 range"),
        ),
        shift_x,
        shift_y,
        cols: (shift_x + width).div_ceil(cell),
        rows: (shift_y + height).div_ceil(cell),
    }
}

/// Load radius that keeps the whole view plus one chunk of margin loaded.
pub fn policy_for(view_w: u32, view_h: u32) -> LoadPolicy {
    let half_chunks = |cells: u32| (cells / 2).div_ceil(CHUNK_SIZE as u32) as i32;
    let load = half_chunks(view_w).max(half_chunks(view_h)) + 1;
    LoadPolicy {
        load,
        unload: load + 2,
    }
}

/// This frame's geometry, all in physical pixels (= world units).
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct Layout {
    /// False while the window has no area (minimised).
    pub visible: bool,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    /// Cell edge.
    pub cell: u32,
    pub map: MapLayout,
    /// Height of the map area; the status rows take the rest.
    pub map_h: u32,
    pub status_cols: u32,
    pub status_h: u32,
}

// ---- systems ---------------------------------------------------------------------------------

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, Msaa::Off));
    let map = Layer::spawn(&mut commands, 0.0);
    let status = Layer::spawn(&mut commands, 2.0);
    commands.insert_resource(Grids { map, status });
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut close: MessageReader<WindowCloseRequested>,
    mut input: ResMut<MoveInput>,
    mut clock: ResMut<SimClock>,
    mut zoom: ResMut<Zoom>,
    mut pending: ResMut<Pending>,
) {
    let held = |a, b| keys.pressed(a) || keys.pressed(b);
    let up = held(KeyCode::KeyW, KeyCode::ArrowUp);
    let down = held(KeyCode::KeyS, KeyCode::ArrowDown);
    let left = held(KeyCode::KeyA, KeyCode::ArrowLeft);
    let right = held(KeyCode::KeyD, KeyCode::ArrowRight);
    input.0 = Input {
        dx: i8::from(right) - i8::from(left),
        dy: i8::from(down) - i8::from(up),
        fast: held(KeyCode::ShiftLeft, KeyCode::ShiftRight),
    };
    if keys.just_pressed(KeyCode::Space) {
        clock.toggle_pause();
    }
    if keys.just_pressed(KeyCode::Period) {
        clock.pause();
        pending.step = true;
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        clock.slower();
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        clock.faster();
    }
    if keys.just_pressed(KeyCode::KeyP) {
        pending.save = true;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        pending.reload = true;
    }
    if keys.any_just_pressed([KeyCode::Equal, KeyCode::NumpadAdd]) {
        zoom.change(ZOOM_STEP);
    }
    if keys.any_just_pressed([KeyCode::Minus, KeyCode::NumpadSubtract]) {
        zoom.change(-ZOOM_STEP);
    }
    if keys.any_just_pressed([KeyCode::KeyQ, KeyCode::Escape]) || close.read().next().is_some() {
        pending.quit = true;
    }
}

fn advance_camera(time: Res<Time>, input: Res<MoveInput>, mut camera: ResMut<ViewCamera>) {
    camera.update(time.delta_secs_f64().min(MAX_DT), input.0);
}

fn layout(
    window: Query<&Window, With<PrimaryWindow>>,
    mut projection: Query<&mut Projection, With<Camera2d>>,
    zoom: Res<Zoom>,
    camera: Res<ViewCamera>,
    mut layout: ResMut<Layout>,
    mut glyphs: ResMut<Glyphs>,
    mut images: ResMut<Assets<Image>>,
    mut grids: ResMut<Grids>,
    mut commands: Commands,
) {
    let Ok(window) = window.single() else {
        layout.visible = false;
        return;
    };
    let (width, height) = (window.physical_width(), window.physical_height());
    let scale = window.scale_factor();
    if width == 0 || height == 0 {
        layout.visible = false;
        return;
    }
    // One world unit = one physical pixel, whatever the DPI.
    if let Ok(mut p) = projection.single_mut()
        && let Projection::Orthographic(o) = &mut *p
        && o.scale != scale
    {
        o.scale = scale;
    }
    let cell = ((zoom.cell_logical as f32) * scale).round().max(1.0) as u32;
    if glyphs.cell != cell {
        glyphs.tileset = images.add(GlyphAtlas::build(cell).tileset());
        glyphs.cell = cell;
    }
    let status_h = (STATUS_ROWS as u32 * cell).min(height);
    let map_h = height - status_h;
    let map = map_layout(&camera, cell as usize, width as usize, map_h as usize);
    let status_cols = width.div_ceil(cell);
    *layout = Layout {
        visible: true,
        width,
        height,
        scale,
        cell,
        map,
        map_h,
        status_cols,
        status_h,
    };
    let (cols, rows) = (map.cols as u32, map.rows as u32);
    let Grids { map: m, status: s } = &mut *grids;
    if (m.cols, m.rows, m.cell) != (cols, rows, cell) {
        m.configure(&mut commands, cols, rows, cell, &glyphs.tileset);
    }
    if (s.cols, s.rows, s.cell) != (status_cols, STATUS_ROWS as u32, cell) {
        s.configure(
            &mut commands,
            status_cols,
            STATUS_ROWS as u32,
            cell,
            &glyphs.tileset,
        );
    }
}

/// Streaming, ticking, saving, quitting: everything that needs the whole world.
fn stream_and_tick(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs_f64();
    let layout = *world.resource::<Layout>();
    let focus = world.resource::<ViewCamera>().cell();
    let pending = std::mem::take(&mut *world.resource_mut::<Pending>());

    let policy = policy_for(layout.map.cols as u32, layout.map.rows as u32);
    let streamed = world.resource_scope(|world, store: Mut<Store>| {
        sim::ensure_loaded(world, focus, policy, Some(&store))
    });
    match streamed {
        Ok(s) if s.generated + s.read + s.unloaded + s.written > 0 => {
            world.resource_mut::<StreamInfo>().0 = Some(s);
        }
        Ok(_) => {}
        Err(e) => error!("streaming chunks: {e}"),
    }

    if pending.reload {
        let text = reload(world);
        info!("{text}");
        world.resource_mut::<Notice>().show(text);
    }
    if pending.step {
        sim::step(world);
    }
    world.resource_scope(|world, mut clock: Mut<SimClock>| {
        clock.run(dt, TICK_BUDGET, || sim::step(world));
    });

    if pending.save || pending.quit {
        match save(world) {
            Ok(n) => info!("saved {n} chunks"),
            Err(e) => {
                error!("saving: {e:#}");
                if pending.quit {
                    world.write_message(AppExit::error());
                    return;
                }
            }
        }
    }
    if pending.quit {
        world.write_message(AppExit::Success);
    }
}

/// `r`: recompile the rules directory and swap the rules in, rows and saved
/// chunks remapped by name (`sim_core::reload`). Returns the line for the
/// status bar: what changed, or why nothing did.
fn reload(world: &mut World) -> String {
    let Some(dir) = crate::rules_dir() else {
        return "reload: no rules directory (run from the repository, or set WMC_RULES)".into();
    };
    let kinds = match sim_core::rules::compile_dir(&dir) {
        Ok(k) => k,
        Err(e) => return format!("reload: {e}"),
    };
    if kinds.hash == world.resource::<Kinds>().hash {
        return format!("reload: {} unchanged", dir.display());
    }
    let r = world.resource_scope(|world, store: Mut<Store>| {
        sim_core::reload::reload_rules(world, Some(&store), kinds)
    });
    match r {
        Err(e) => format!("reload refused: {e}"),
        Ok(r) => {
            let mut text = format!("reloaded {} (rules {:016x})", dir.display(), r.hash);
            if !r.added.is_empty() {
                text.push_str(&format!(" | new {}", r.added.join(", ")));
            }
            for (name, rows) in &r.removed {
                text.push_str(&format!(" | {name} gone ({rows} dropped)"));
            }
            if r.rewritten > 0 {
                text.push_str(&format!(" | {} saved chunks rewritten", r.rewritten));
            }
            text
        }
    }
}

/// World and camera, into the open store. Returns chunks written.
fn save(world: &mut World) -> anyhow::Result<usize> {
    world.resource_scope(|world, store: Mut<Store>| {
        let n = sim::save(world, &store).context("saving world")?;
        world
            .resource::<ViewCamera>()
            .save(store.dir())
            .context("saving camera")?;
        Ok(n)
    })
}

/// Phase 1: chunks -> cells, plus the status rows.
fn render_frame(
    stage: StageCells,
    kinds: Res<Kinds>,
    tally: Res<Tally>,
    rows: Query<&ChunkActors>,
    tick: Res<Tick>,
    layout: Res<Layout>,
    camera: Res<ViewCamera>,
    clock: Res<SimClock>,
    info: Res<StreamInfo>,
    notice: Res<Notice>,
    mut frames: ResMut<Frames>,
) {
    if !layout.visible {
        return;
    }
    let map = layout.map;
    frames.map.resize(map.cols, map.rows);
    let view = Viewport {
        origin: map.origin,
        width: map.cols as u32,
        height: map.rows as u32,
    };
    let light = brightness(time::daylight(tick.0));
    render_cells(
        |c| stage.chunk(c),
        Looks::of(&kinds),
        view,
        light,
        &mut frames.map,
    );

    let focus_chunk = camera.cell().split().0;
    let status = format!(
        "{} | {} | tick {} | cam ({:.1}, {:.1}) chunk ({}, {}) | {}x{} @ {}px | loaded {} | stream: {}",
        time::Clock::at(tick.0),
        clock.label(),
        tick.0,
        camera.x,
        camera.y,
        focus_chunk.x,
        focus_chunk.y,
        map.cols,
        map.rows,
        layout.cell,
        stage.loaded_count(),
        info.0.map_or_else(
            || "-".to_string(),
            |s| format!(
                "gen {} read {} unload {} write {}",
                s.generated, s.read, s.unloaded, s.written
            )
        ),
    );
    frames
        .status
        .resize(layout.status_cols as usize, STATUS_ROWS);
    frames.status.put_text(0, &status, TEXT_FG, TEXT_BG);
    let (alive, events) = life_lines(&kinds, &tally, &rows);
    frames.status.put_text(1, &alive, TEXT_FG, TEXT_BG);
    frames.status.put_text(2, &events, TEXT_FG, TEXT_BG);
    frames
        .status
        .put_text(3, notice.current().unwrap_or(HELP), TEXT_FG, TEXT_BG);
}

/// Two status rows: how many of each kind are loaded, and the life events
/// since the world was opened, each kind by its glyph
/// (`alive C97 o4 ...`, `born o12 | grew c4 | eaten C3 | died C5`), with
/// `TRAPS b2` when a kind's program ran out of fuel or faulted (a rules
/// bug: `wmc why` shows the think).
fn life_lines(kinds: &Kinds, tally: &Tally, rows: &Query<&ChunkActors>) -> (String, String) {
    let mut alive = vec![0usize; kinds.len()];
    for a in rows {
        for r in &a.rows {
            if let Some(n) = alive.get_mut(usize::from(r.kind)) {
                *n += 1;
            }
        }
    }
    let glyph = |k: usize| char::from(kinds.glyphs[k]);
    let mut line = String::from("alive");
    for (k, n) in alive.iter().enumerate() {
        line.push_str(&format!(" {}{n}", glyph(k)));
    }
    let mut events = String::new();
    for (label, event) in [
        ("born", life::BORN),
        ("grew", life::BECAME),
        ("eaten", life::EATEN),
        ("died", life::DIED),
        ("TRAPS", life::TRAPS),
    ] {
        let parts: Vec<String> = (0..kinds.len())
            .filter_map(|k| {
                let n = tally.get(k as u16, event);
                (n > 0).then(|| format!("{}{n}", glyph(k)))
            })
            .collect();
        if !parts.is_empty() {
            if !events.is_empty() {
                events.push_str(" | ");
            }
            events.push_str(&format!("{label} {}", parts.join(" ")));
        }
    }
    if events.is_empty() {
        events.push_str("no births or deaths yet");
    }
    (line, events)
}

/// Phase 2: cells -> tiles, and the layers into place.
fn upload_tiles(
    layout: Res<Layout>,
    frames: Res<Frames>,
    grids: Res<Grids>,
    mut tiles: Query<(&mut TilemapChunkTileData, &mut Transform)>,
) {
    if !layout.visible {
        return;
    }
    let (w, h) = (layout.width as f32, layout.height as f32);
    let map = layout.map;
    let map_at = grids
        .map
        .place(-(map.shift_x as f32), -(map.shift_y as f32), w, h);
    let status_at = grids
        .status
        .place(0.0, (layout.height - layout.status_h) as f32, w, h);
    for (layer, frame, (bg_at, fg_at)) in [
        (&grids.map, &frames.map, map_at),
        (&grids.status, &frames.status, status_at),
    ] {
        if !layer.fits(frame) {
            continue;
        }
        let Ok([(mut bg, mut bg_tf), (mut fg, mut fg_tf)]) =
            tiles.get_many_mut([layer.bg, layer.fg])
        else {
            continue;
        };
        let n = frame.cols() * frame.rows();
        if bg.len() != n || fg.len() != n {
            continue;
        }
        grid::upload(frame, &mut bg, &mut fg);
        bg_tf.translation = bg_at;
        fg_tf.translation = fg_at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_covers_the_view() {
        // 80x24 view: half-width 40 cells = 1 chunk, plus margin.
        assert_eq!(policy_for(80, 24), LoadPolicy { load: 2, unload: 4 });
        // 300 wide: half = 150 cells = 3 chunks, plus margin.
        assert_eq!(policy_for(300, 24), LoadPolicy { load: 4, unload: 6 });
    }

    #[test]
    fn map_layout_centres_the_camera_and_covers_the_window() {
        // Camera on a cell centre, window an exact number of cells: no shift.
        let cam = ViewCamera::new(Pos::new(10, 5)); // (10.5, 5.5)
        let l = map_layout(&cam, 16, 160, 96); // 10 x 6 cells
        assert_eq!(l.origin, Pos::new(5, 2));
        assert_eq!((l.shift_x, l.shift_y), (8, 8));
        assert_eq!((l.cols, l.rows), (11, 7));
        // Any camera (off exact cell boundaries, where a half pixel rounds
        // either way): the grid must cover the whole window.
        for (x, y) in [(0.02, 0.03), (-3.7, 2.2), (1234.9, -777.01), (0.49, 0.51)] {
            let mut cam = ViewCamera::new(Pos::new(0, 0));
            cam.x = x;
            cam.y = y;
            for (w, h) in [(1, 1), (17, 33), (2560, 1536), (2559, 1535)] {
                let l = map_layout(&cam, 32, w, h);
                assert!(l.shift_x < 32 && l.shift_y < 32);
                assert!(l.cols * 32 >= l.shift_x + w, "{x},{y} {w}x{h}");
                assert!(l.rows * 32 >= l.shift_y + h, "{x},{y} {w}x{h}");
                // The world pixel under the window centre is the camera's
                // position, to within the rounding of the corner.
                let centre_x = i64::from(l.origin.x) * 32 + l.shift_x as i64 + (w / 2) as i64;
                let centre_y = i64::from(l.origin.y) * 32 + l.shift_y as i64 + (h / 2) as i64;
                assert!(
                    (centre_x - (x * 32.0).floor() as i64).abs() <= 1,
                    "{x},{y} {w}x{h}"
                );
                assert!(
                    (centre_y - (y * 32.0).floor() as i64).abs() <= 1,
                    "{x},{y} {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn zoom_steps_and_clamps() {
        let mut z = Zoom::default();
        assert_eq!(z.cell_logical, DEFAULT_CELL_LOGICAL);
        z.change(ZOOM_STEP);
        assert_eq!(z.cell_logical, DEFAULT_CELL_LOGICAL + 2);
        for _ in 0..100 {
            z.change(-ZOOM_STEP);
        }
        assert_eq!(z.cell_logical, MIN_CELL_LOGICAL);
        for _ in 0..100 {
            z.change(ZOOM_STEP);
        }
        assert_eq!(z.cell_logical, MAX_CELL_LOGICAL);
    }

    #[test]
    fn a_notice_shows_until_it_expires() {
        let mut n = Notice::default();
        assert_eq!(n.current(), None);
        n.show("reloaded rules/");
        assert_eq!(n.current(), Some("reloaded rules/"));
        n.until = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(n.current(), None);
    }
}
