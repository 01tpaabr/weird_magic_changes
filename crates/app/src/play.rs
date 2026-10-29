//! The windowed game: a Bevy `App` around the sim.
//!
//! A frame (`Update`, one chain, in this order):
//! 1. `handle_input`: keys -> camera direction, clock/zoom toggles, pending save/step/reload/quit.
//! 2. `advance_camera`: glide the camera by the frame's real `dt`.
//! 3. `layout`: window size and DPI -> cell size, map/status grids; rebuild the
//!    glyph tileset and the tile layers when their shape changes.
//! 4. `stream_and_tick` (exclusive): load/unload chunks around the camera, run
//!    the ticks the [`SimClock`] owes (bounded by [`TICK_BUDGET`]), save/quit.
//! 5. `inspect` (exclusive): a click selects an actor; the selected one is
//!    found again by uid and its next think explained for the side panel.
//! 6. `render_frame`: phase 1, chunks -> `CellFrame`, dimmed by the sim's
//!    daylight, plus the status rows and the inspector panel.
//! 7. `upload_tiles`: phase 2, `CellFrame` -> tilemap tile data + layer transforms.
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
//! rules (the world's packs, else `./rules`), the status bar says what changed;
//! `+`/`-` zoom; a click on an actor opens the inspector (again on the same
//! cell: the ground cover under it), `f` follows it, `Esc` closes it;
//! `q` / `Esc` / close saves and quits.

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
use crate::inspect::{self, ALERT, DIM, FAINT, Inspector, KEYS, Selection};
use crate::render::atlas::GlyphAtlas;
use crate::render::cells::{CellFrame, Viewport, render_cells};
use crate::render::grid::{self, Layer};
use crate::render::palette::{Color, Looks, TEXT_BG, TEXT_FG, VOID_BG, brightness};

/// Cell edge in logical pixels at startup; multiplied by the window's scale
/// factor (2 on Retina) to get the physical cell the tileset is built for.
pub const DEFAULT_CELL_LOGICAL: u32 = 16;
const MIN_CELL_LOGICAL: u32 = 6;
const MAX_CELL_LOGICAL: u32 = 64;
const ZOOM_STEP: i32 = 2;
/// Rows reserved under the map for status text.
const STATUS_ROWS: u32 = 4;
/// Longest frame time fed to the camera: a stall becomes a small step, not a leap.
const MAX_DT: f64 = 0.1;
/// Sim time per frame. The rest of a 60 Hz frame is for streaming and drawing;
/// at max speed this is how long each frame ticks for.
pub const TICK_BUDGET: Duration = Duration::from_millis(10);
const HELP: &str = "wasd/arrows move (shift x4) | space pause | . step | [ ] speed | click inspect | f follow | p save | r reload rules | +/- zoom | q quit";
/// The inspector panel: two fifths of the window, held to this many columns.
const PANEL_MIN: u32 = 36;
const PANEL_MAX: u32 = 64;
/// Opacity of the panel's background: the map shows through it.
const PANEL_ALPHA: f32 = 0.78;
/// The selected actor's cell on the map.
const SELECTED_BG: Color = Color::rgb(0xf0, 0xc6, 0x74);
const SELECTED_FG: Color = Color::rgb(0x10, 0x10, 0x12);

/// Open the world in `dir`, else create it from `scenario` (read from
/// `name`, for errors), under the rules of `packs` (`--rules` paths; see
/// [`crate::rules_for`]), and run the window until quit.
pub fn run(dir: &str, scenario: &Scenario, name: &str, packs: &[String]) -> anyhow::Result<()> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let mut app = App::new();
    // `WMC_THREADS`, which `main` checked, unless `--threads` made the
    // compute pool already (it wins, and the variable is not read).
    let threads = match par::thread_count() {
        0 => par::threads_from_env(),
        _ => None,
    };
    let task_pool_options =
        threads.map_or_else(TaskPoolOptions::default, TaskPoolOptions::with_num_threads);
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
    let kinds = crate::rules_for(&store, packs, &scenario.packs)?;
    let world = app.world_mut();
    sim::install_with(world, kinds);
    if sim::open(world, &store).context("reading save")? {
        crate::say_skipped_starts(world);
    } else {
        sim::create(world, scenario)
            .map_err(|e| crate::new_world_error(name, scenario.line_of(&e), e))?;
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
            .init_resource::<Click>()
            .init_resource::<Selection>()
            .init_resource::<Inspector>()
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    handle_input,
                    advance_camera,
                    layout,
                    stream_and_tick,
                    inspect,
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

/// The map cell the player clicked this frame, for `inspect`.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Click(Option<Pos>);

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

/// The cell frames of a frame: the map, the status rows, the inspector.
#[derive(Resource, Debug, Default)]
struct Frames {
    map: CellFrame,
    status: CellFrame,
    panel: CellFrame,
}

/// The tile layers on screen.
#[derive(Resource, Debug)]
struct Grids {
    map: Layer,
    status: Layer,
    /// Over the map's right side while an actor is selected.
    panel: Layer,
    panel_shown: bool,
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

/// The map cell under window pixel `(px, py)` (physical, from the top-left).
pub fn cell_at(map: &MapLayout, cell: u32, px: f32, py: f32) -> Pos {
    let cell = f64::from(cell);
    // A pixel inside the window, a cell of a few pixels: far inside i32.
    #[allow(clippy::cast_possible_truncation)]
    let at = |px: f32, shift: usize| ((f64::from(px) + shift as f64) / cell).floor() as i32;
    Pos::new(
        map.origin.x.saturating_add(at(px, map.shift_x)),
        map.origin.y.saturating_add(at(py, map.shift_y)),
    )
}

pub fn map_layout(camera: &ViewCamera, cell: usize, width: usize, height: usize) -> MapLayout {
    let cell_i = cell as i64;
    let corner = |center: f64, extent: usize| -> (i64, usize) {
        // The camera holds its centre to ±1e9 cells: in pixels, far inside i64.
        #[allow(clippy::cast_possible_truncation)]
        let px = (center * cell as f64 - extent as f64 / 2.0).round() as i64;
        let shift = usize::try_from(px.rem_euclid(cell_i)).expect("in 0..cell");
        (px.div_euclid(cell_i), shift)
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
    /// The inspector, over the map's right edge: `0 x 0` while nobody is
    /// selected.
    pub panel_cols: u32,
    pub panel_rows: u32,
}

// ---- systems ---------------------------------------------------------------------------------

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, Msaa::Off));
    let map = Layer::spawn(&mut commands, 0.0);
    let panel = Layer::spawn(&mut commands, 1.0);
    let status = Layer::spawn(&mut commands, 2.0);
    for e in [panel.bg, panel.fg] {
        commands.entity(e).insert(Visibility::Hidden);
    }
    commands.insert_resource(Grids {
        map,
        status,
        panel,
        panel_shown: false,
    });
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    layout: Res<Layout>,
    mut close: MessageReader<WindowCloseRequested>,
    mut input: ResMut<MoveInput>,
    mut clock: ResMut<SimClock>,
    mut zoom: ResMut<Zoom>,
    mut pending: ResMut<Pending>,
    mut click: ResMut<Click>,
    mut selection: ResMut<Selection>,
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
    // Moving the view yourself lets go of the actor.
    if up || down || left || right {
        selection.follow = false;
    }
    click.0 = None;
    if mouse.just_pressed(MouseButton::Left)
        && layout.visible
        && let Ok(window) = window.single()
        && let Some(at) = window.physical_cursor_position()
    {
        // The map, minus the panel over its right side; the status rows
        // and the panel take no clicks.
        let panel_x = layout.width - layout.panel_cols * layout.cell;
        if at.y < layout.map_h as f32 && (layout.panel_cols == 0 || at.x < panel_x as f32) {
            click.0 = Some(cell_at(&layout.map, layout.cell, at.x, at.y));
        }
    }
    if keys.just_pressed(KeyCode::KeyF) && selection.uid.is_some() && selection.gone_at.is_none() {
        selection.follow = !selection.follow;
    }
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
    // Esc closes the inspector first, then quits.
    let esc = keys.just_pressed(KeyCode::Escape);
    if esc && selection.uid.is_some() {
        selection.clear();
    } else if esc || keys.just_pressed(KeyCode::KeyQ) || close.read().next().is_some() {
        pending.quit = true;
    }
}

/// Glide by the keys, or after the followed actor: it stands in the middle
/// of the map left of the panel.
fn advance_camera(
    time: Res<Time>,
    input: Res<MoveInput>,
    selection: Res<Selection>,
    inspector: Res<Inspector>,
    layout: Res<Layout>,
    mut camera: ResMut<ViewCamera>,
) {
    let dt = time.delta_secs_f64().min(MAX_DT);
    match inspector.at {
        Some(p) if selection.follow => camera.glide_to(
            f64::from(p.x) + 0.5 + f64::from(layout.panel_cols) / 2.0,
            f64::from(p.y) + 0.5,
            dt,
        ),
        _ => camera.update(dt, input.0),
    }
}

fn layout(
    window: Query<&Window, With<PrimaryWindow>>,
    mut projection: Query<&mut Projection, With<Camera2d>>,
    zoom: Res<Zoom>,
    camera: Res<ViewCamera>,
    selection: Res<Selection>,
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
    // `Zoom` holds the cell to a few dozen logical pixels and the scale is a
    // DPI factor: a small whole number of pixels, at least 1.
    #[allow(clippy::cast_possible_truncation)]
    let cell = ((zoom.cell_logical as f32) * scale).round().max(1.0) as u32;
    if glyphs.cell != cell {
        glyphs.tileset = images.add(GlyphAtlas::build(cell).tileset());
        glyphs.cell = cell;
    }
    let status_h = (STATUS_ROWS * cell).min(height);
    let map_h = height - status_h;
    let map = map_layout(&camera, cell as usize, width as usize, map_h as usize);
    let status_cols = width.div_ceil(cell);
    let (panel_cols, panel_rows) = if selection.uid.is_some() {
        (
            (width / cell * 2 / 5)
                .clamp(PANEL_MIN, PANEL_MAX)
                .min(width / cell),
            (map_h / cell).max(1),
        )
    } else {
        (0, 0)
    };
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
        panel_cols,
        panel_rows,
    };
    let (cols, rows) = (
        u32::try_from(map.cols).expect("the map is a window wide"),
        u32::try_from(map.rows).expect("the map is a window tall"),
    );
    let shown = panel_cols > 0 && panel_rows > 0;
    let Grids {
        map: m,
        status: s,
        panel: p,
        panel_shown,
    } = &mut *grids;
    if shown && (p.cols, p.rows, p.cell) != (panel_cols, panel_rows, cell) {
        p.configure(&mut commands, panel_cols, panel_rows, cell, &glyphs.tileset);
    }
    if *panel_shown != shown {
        *panel_shown = shown;
        let v = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        for e in [p.bg, p.fg] {
            commands.entity(e).insert(v);
        }
    }
    if (m.cols, m.rows, m.cell) != (cols, rows, cell) {
        m.configure(&mut commands, cols, rows, cell, &glyphs.tileset);
    }
    if (s.cols, s.rows, s.cell) != (status_cols, STATUS_ROWS, cell) {
        s.configure(
            &mut commands,
            status_cols,
            STATUS_ROWS,
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

    let policy = policy_for(
        u32::try_from(layout.map.cols).expect("the map is a window wide"),
        u32::try_from(layout.map.rows).expect("the map is a window tall"),
    );
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

/// The inspector: a click selects whoever is on the cell (or clears the
/// selection on an empty one); the selected actor is found again by uid
/// and its next think explained, read-only, into [`Inspector`]. When it is
/// gone the panel keeps its last state, faded, under a line saying so.
fn inspect(world: &mut World) {
    let click = world.resource::<Click>().0;
    let width = world.resource::<Layout>().panel_cols as usize;
    let mut sel = *world.resource::<Selection>();
    if let Some(p) = click {
        match inspect::pick(world, p, sel.uid) {
            Some((uid, cc)) => sel.select(uid, cc),
            None => sel.clear(),
        }
    }
    let tick = sim::tick(world);
    match (sel.uid, sel.near) {
        (Some(uid), Some(near)) if sel.gone_at.is_none() => {
            match inspect::locate(world, uid, near) {
                Some((p, cc, slot)) => {
                    sel.near = Some(cc);
                    let e = sim::explain_slot(world, cc, slot).expect("a row it was found in");
                    let lines = inspect::panel(
                        world.resource::<Kinds>(),
                        tick,
                        p,
                        &e,
                        sel.follow,
                        width.saturating_sub(2),
                    );
                    *world.resource_mut::<Inspector>() = Inspector { at: Some(p), lines };
                }
                None => {
                    sel.gone_at = Some(tick);
                    sel.follow = false;
                    let mut ins = world.resource_mut::<Inspector>();
                    ins.at = None;
                    for l in &mut ins.lines {
                        l.1 = FAINT;
                    }
                    ins.lines.insert(
                        0,
                        (
                            format!("GONE at tick {tick}: died, eaten or unloaded"),
                            ALERT,
                        ),
                    );
                }
            }
        }
        (Some(_), _) => {}
        (None, _) => *world.resource_mut::<Inspector>() = Inspector::default(),
    }
    *world.resource_mut::<Selection>() = sel;
}

/// `r`: recompile the packs the world runs (the built-in rules: `./rules`,
/// when run from the repository) and swap them in, rows and saved chunks
/// remapped by name (`sim_core::reload`). Returns the line for the status
/// bar: what changed, or why nothing did.
fn reload(world: &mut World) -> String {
    let mut packs: Vec<std::path::PathBuf> = world
        .resource::<Kinds>()
        .debug
        .packs
        .iter()
        .map(Into::into)
        .collect();
    if packs.is_empty() {
        let here = std::path::PathBuf::from("rules");
        if !here.is_dir() {
            return "reload: the built-in rules (run from the repository, or give --rules)".into();
        }
        packs.push(here);
    }
    let what = crate::shown(&packs);
    let kinds = match crate::compile(&packs) {
        Ok(k) => k,
        Err(e) => return format!("reload: {e:#}"),
    };
    if kinds.hash == world.resource::<Kinds>().hash {
        return format!("reload: {what} unchanged");
    }
    let r = world.resource_scope(|world, store: Mut<Store>| {
        sim_core::reload::reload_rules(world, Some(&store), kinds)
    });
    match r {
        Err(e) => format!("reload refused: {e}"),
        Ok(r) => {
            let mut text = format!("reloaded {what} (rules {:016x})", r.hash);
            let warnings: Vec<_> = world
                .resource::<Kinds>()
                .debug
                .diagnostics
                .iter()
                .filter(|d| d.level == sim_core::rules::Level::Warning)
                .collect();
            if !warnings.is_empty() {
                text.push_str(&format!(" | {} warnings (log)", warnings.len()));
                for d in warnings {
                    warn!("rules: {d}");
                }
            }
            if !r.added.is_empty() {
                text.push_str(&format!(" | new {}", r.added.join(", ")));
            }
            for (name, rows) in &r.removed {
                text.push_str(&format!(" | {name} gone ({rows} dropped)"));
            }
            if r.rewritten > 0 {
                text.push_str(&format!(" | {} saved chunks rewritten", r.rewritten));
            }
            if !r.skipped_starts.is_empty() {
                text.push_str(&format!(
                    " | starts skipped (kept): {}",
                    r.skipped_starts.join(", ")
                ));
            }
            if let Some(e) = &r.world_file {
                text.push_str(&format!(
                    " | world file not written ({e}): p saves it again"
                ));
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
    inspector: Res<Inspector>,
    mut frames: ResMut<Frames>,
) {
    if !layout.visible {
        return;
    }
    let map = layout.map;
    frames.map.resize(map.cols, map.rows);
    let view = Viewport {
        origin: map.origin,
        width: u32::try_from(map.cols).expect("the map is a window wide"),
        height: u32::try_from(map.rows).expect("the map is a window tall"),
    };
    let light = brightness(time::daylight(tick.0));
    render_cells(
        |c| stage.chunk(c),
        Looks::of(&kinds),
        view,
        light,
        &mut frames.map,
    );
    if let Some(p) = inspector.at {
        let (dx, dy) = (
            i64::from(p.x) - i64::from(map.origin.x),
            i64::from(p.y) - i64::from(map.origin.y),
        );
        if let (Ok(c), Ok(r)) = (usize::try_from(dx), usize::try_from(dy))
            && c < map.cols
            && r < map.rows
        {
            let i = r * map.cols + c;
            frames.map.bg[i] = SELECTED_BG.0;
            frames.map.fg[i] = SELECTED_FG.0;
        }
    }
    let (pc, pr) = (layout.panel_cols as usize, layout.panel_rows as usize);
    frames.panel.resize(pc, pr);
    if pc > 0 && pr > 0 {
        let body = pr - 1;
        let lines = &inspector.lines;
        for row in 0..body {
            let (text, colour) = match lines.get(row) {
                Some(_) if lines.len() > body && row == body - 1 => (
                    format!(
                        "... {} more rows (a taller window shows them)",
                        lines.len() - row
                    ),
                    DIM,
                ),
                Some((t, c)) => (t.clone(), *c),
                None => (String::new(), TEXT_FG),
            };
            frames
                .panel
                .put_text(row, &format!(" {text}"), colour, TEXT_BG);
        }
        frames
            .panel
            .put_text(body, &format!(" {KEYS}"), DIM, TEXT_BG);
    }

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
        .resize(layout.status_cols as usize, STATUS_ROWS as usize);
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
/// (`alive C97 o4 ...`, `born o12 | became c4 | eaten C3 | died C5`), with
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
        ("became", life::BECAME),
        ("eaten", life::EATEN),
        ("died", life::DIED),
        ("TRAPS", life::TRAPS),
    ] {
        let parts: Vec<String> = (0..kinds.len())
            .filter_map(|k| {
                let n = tally.get(u16::try_from(k).expect("kind ids are u16"), event);
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
    let panel_at = grids.panel.place(
        (layout.width - layout.panel_cols * layout.cell) as f32,
        0.0,
        w,
        h,
    );
    for (layer, frame, (bg_at, fg_at), alpha) in [
        (&grids.map, &frames.map, map_at, 1.0),
        (&grids.status, &frames.status, status_at, 1.0),
        (&grids.panel, &frames.panel, panel_at, PANEL_ALPHA),
    ] {
        if frame.cols() == 0 || !layer.fits(frame) {
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
        grid::upload(frame, &mut bg, &mut fg, alpha);
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
    fn a_click_lands_on_the_cell_drawn_under_it() {
        // The camera's cell is under the window centre, and every pixel of
        // a drawn cell maps back to it.
        for (x, y) in [(10.5, 5.5), (-3.7, 2.2), (0.02, -0.97)] {
            let mut cam = ViewCamera::new(Pos::new(0, 0));
            cam.x = x;
            cam.y = y;
            let l = map_layout(&cam, 16, 320, 160);
            let mid = cell_at(&l, 16, 160.0, 80.0);
            assert!((mid.x - cam.cell().x).abs() <= 1 && (mid.y - cam.cell().y).abs() <= 1);
            // Frame column c starts at window pixel c * 16 - shift.
            for c in 0..l.cols {
                let left = (c * 16) as f32 - l.shift_x as f32;
                for px in [left, left + 15.9] {
                    if (0.0..320.0).contains(&px) {
                        let p = cell_at(&l, 16, px, 0.0);
                        assert_eq!(p.x, l.origin.x + c as i32, "{x},{y} column {c} at {px}");
                    }
                }
            }
            let top = cell_at(&l, 16, 0.0, 0.0);
            assert_eq!(top, l.origin, "the top-left pixel is in the first cell");
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
