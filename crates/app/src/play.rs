//! The windowed game: a Bevy `App` around the sim.
//!
//! One app, one sim world at a time, on one of two [`Screen`]s: the menu
//! ([`crate::menu`], `wmc` alone) over the gallery world, or a world played
//! (`wmc play`, or one the menu opened). Opening another world is
//! [`switch`]: what can fail first, then `sim::uninstall` and a new world
//! installed in the same frame. The menu fits the gallery in the window, in
//! daylight, with no status rows.
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

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bevy::app::{TaskPoolOptions, TaskPoolPlugin};
use bevy::camera::Projection;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
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
use crate::inspect::{self, ALERT, DIM, FAINT, Inspector, KEYS, Line, Selection};
use crate::menu::{self, Menu};
use crate::render::atlas::GlyphAtlas;
use crate::render::cells::{CellFrame, Viewport, render_cells};
use crate::render::grid::{self, Layer};
use crate::render::palette::{
    Color, GLYPH_MIN, Looks, NIGHT_SOIL, SOIL, TEXT_BG, TEXT_FG, VOID_BG, brightness,
};

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
/// [`HELP`] when quitting goes back to the menu.
const HELP_HOME: &str = "wasd/arrows move (shift x4) | space pause | . step | [ ] speed | click inspect | f follow | p save | r reload rules | +/- zoom | q menu";
/// The menu box: at most this many text cells, and its background's opacity.
const MENU_COLS: u32 = 96;
const MENU_ROWS: u32 = 26;
const MENU_ALPHA: f32 = 0.8;
/// The gallery's column split (`scenarios/gallery.scenario`: the rock line at
/// this x between its left column of panels and its right one): the menu box
/// ends there, so the right column stands clear of it.
const GALLERY_SPLIT_X: i32 = 322;
/// Ticks the gallery runs before it starts over: its busy beaver's first
/// 10 000 steps or so, about 5 minutes at the menu's 8x.
const GALLERY_TICKS: u64 = 20_000;
/// The inspector panel: two fifths of the window, held to this many columns
/// of text cells.
const PANEL_MIN: u32 = 40;
const PANEL_MAX: u32 = 120;
/// The panel's text height in logical pixels, whatever the map's zoom; a
/// text cell is [`crate::render::atlas::TEXT_ASPECT`] of that wide.
const PANEL_TEXT_LOGICAL: u32 = 13;
/// Rows one notch of a mouse wheel scrolls the panel.
const WHEEL_STEP: f32 = 3.0;
/// Opacity of the panel's background: the map shows through it.
const PANEL_ALPHA: f32 = 0.78;
/// The selected actor's cell on the map.
const SELECTED_BG: Color = Color::rgb(0xf0, 0xc6, 0x74);
const SELECTED_FG: Color = Color::rgb(0x10, 0x10, 0x12);
/// The panel line under the pointer.
const HOVER_BG: Color = Color::rgb(0x3a, 0x3a, 0x4c);

/// Open the world in `dir`, else create it from `scenario` (read from
/// `name`, for errors), under the rules of `packs` (`--rules` paths; see
/// [`crate::rules_for`]), and run the window until quit.
pub fn run(dir: &str, scenario: &Scenario, name: &str, packs: &[String]) -> anyhow::Result<()> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let mut app = window_app(format!("wmc - {}", store.dir().display()));
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
        .insert_resource(Screen::Play)
        .insert_resource(Home(false))
        .add_plugins(PlayPlugin);
    exit(app.run())
}

/// The main menu (`wmc` with no arguments), over the gallery: open, make
/// and edit worlds ([`crate::menu`]); quitting a world comes back here.
pub fn menu() -> anyhow::Result<()> {
    let mut app = window_app("wmc".into());
    let cwd = std::env::current_dir().context("the working directory")?;
    app.insert_resource(Menu::scan(&cwd))
        .insert_resource(Screen::Menu)
        .insert_resource(Home(true))
        .add_plugins(PlayPlugin);
    switch(app.world_mut(), Open::Gallery)?;
    exit(app.run())
}

fn exit(e: AppExit) -> anyhow::Result<()> {
    match e {
        AppExit::Success => Ok(()),
        AppExit::Error(code) => bail!("exited with code {code}"),
    }
}

/// The window and Bevy's plugins, no world yet.
fn window_app(title: String) -> App {
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
                    title,
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
    app.insert_resource(ClearColor(VOID_BG.tint()));
    app
}

/// What a switch puts in place of the world on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Open {
    /// The gallery, behind the menu.
    Gallery,
    /// The world saved in this directory.
    Load(std::path::PathBuf),
    /// A new world from this scenario file, saved in this directory.
    New {
        scenario: std::path::PathBuf,
        dir: std::path::PathBuf,
    },
}

/// The gallery's scenario and rules (`scenarios/gallery.scenario` under the
/// working directory), else the built-in world: it is a background, never
/// an error.
fn gallery() -> (Scenario, Kinds, bool) {
    let read = || -> anyhow::Result<(Scenario, Kinds, bool)> {
        let s = crate::read_scenario(menu::GALLERY)?;
        let packs: Vec<std::path::PathBuf> = s.packs.iter().map(Into::into).collect();
        let kinds = crate::compile(&packs)?;
        sim_core::scenario::Placement::resolve(&s.starts, &kinds, &s.terrain())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok((s, kinds, true))
    };
    read().unwrap_or_else(|e| {
        warn!(
            "the gallery ({}): {e:#}; the built-in world instead",
            menu::GALLERY
        );
        (Scenario::builtin(), Kinds::builtin(), false)
    })
}

/// Put the world `open` names on screen in place of the one there (saved
/// first by the caller, when it was played). What can fail (reading,
/// compiling, fitting the starts) is done before the old world goes, so an
/// error leaves it in place; a save that will not open after all brings
/// the gallery back. Then the screen, the camera, the clock and the
/// window's title follow.
fn switch(world: &mut World, open: Open) -> anyhow::Result<()> {
    enum Ready {
        Create(Scenario, Option<Store>),
        Open(Store),
    }
    let (kinds, ready) = match &open {
        Open::Gallery => {
            let (s, kinds, found) = gallery();
            world.insert_resource(MenuEdge(found.then_some(GALLERY_SPLIT_X)));
            (kinds, Ready::Create(s, None))
        }
        Open::Load(dir) => {
            let store = Store::open(dir).with_context(|| format!("opening {}", dir.display()))?;
            (crate::rules_for(&store, &[], &[])?, Ready::Open(store))
        }
        Open::New { scenario, dir } => {
            let name = scenario.to_string_lossy();
            let s = crate::read_scenario(&name)?;
            let packs: Vec<std::path::PathBuf> = s.packs.iter().map(Into::into).collect();
            let kinds = crate::compile(&packs)?;
            sim_core::scenario::Placement::resolve(&s.starts, &kinds, &s.terrain())
                .map_err(|e| crate::new_world_error(&name, s.line_of(&e), e))?;
            let store = Store::open(dir).with_context(|| format!("making {}", dir.display()))?;
            (kinds, Ready::Create(s, Some(store)))
        }
    };
    sim::uninstall(world);
    world.remove_resource::<Store>();
    sim::install_with(world, kinds);
    let made = match ready {
        Ready::Create(s, store) => {
            sim::create(world, &s).map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(store) = &store {
                store
                    .write_meta(&sim::meta(world))
                    .context("writing save meta")?;
            }
            store
        }
        Ready::Open(store) => match sim::open(world, &store) {
            Ok(true) => Some(store),
            Ok(false) => {
                switch(world, Open::Gallery)?;
                bail!("{} holds no world", store.dir().display());
            }
            Err(e) => {
                switch(world, Open::Gallery)?;
                return Err(e).context(format!("reading {}", store.dir().display()));
            }
        },
    };
    let camera = match &made {
        Some(store) => camera_for(world, store),
        None => {
            let c = world.resource::<SimConfig>();
            ViewCamera::new(Pos::new(
                i32::try_from(c.initial_width / 2).unwrap_or(0),
                i32::try_from(c.initial_height / 2).unwrap_or(0),
            ))
        }
    };
    let title = match &made {
        Some(store) => format!("wmc - {}", store.dir().display()),
        None => "wmc".into(),
    };
    let screen = if made.is_some() {
        Screen::Play
    } else {
        Screen::Menu
    };
    let mut clock = SimClock::default();
    if screen == Screen::Menu {
        // 8x: the gallery's busy beaver takes 32 steps a second.
        for _ in 0..3 {
            clock.faster();
        }
        if let Some(mut m) = world.get_resource_mut::<Menu>() {
            m.home();
        }
    }
    if let Some(store) = made {
        world.insert_resource(store);
    }
    world.insert_resource(camera);
    world.insert_resource(screen);
    world.insert_resource(clock);
    world.insert_resource(Selection::default());
    world.insert_resource(Inspector::default());
    world.insert_resource(MoveInput::default());
    world.insert_resource(StreamInfo::default());
    world.insert_resource(PanelView::default());
    let mut windows = world.query_filtered::<&mut Window, With<PrimaryWindow>>();
    if let Ok(mut w) = windows.single_mut(world) {
        w.title = title;
    }
    Ok(())
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
            .init_resource::<PanelView>()
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    handle_input.run_if(resource_equals(Screen::Play)),
                    menu_input.run_if(resource_equals(Screen::Menu)),
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

/// What the window shows: the menu over the gallery, or a world played.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Menu,
    Play,
}

/// The cell column the menu box's right edge sits on, when the world behind
/// it has one (the gallery's split); else the box is centred.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MenuEdge(pub Option<i32>);

/// Whether quitting a world goes back to the menu (`wmc` alone) or quits
/// (`wmc play`).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Home(pub bool);

/// Direction the movement keys push this frame.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct MoveInput(Input);

/// One-shot requests from input, consumed by the exclusive system.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
struct Pending {
    save: bool,
    step: bool,
    reload: bool,
    /// `q`: the menu, if there is one, else quit.
    quit: bool,
    /// The window's close button: quit, from anywhere.
    exit: bool,
    /// The menu chose a world.
    open: Option<Open>,
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

/// The glyph tilesets on the GPU and the physical sizes they were built for
/// (`0` = not built yet): square map cells, and the panel's text cells
/// (`text_h` tall).
#[derive(Resource, Debug, Default)]
struct Glyphs {
    cell: u32,
    tileset: Handle<Image>,
    text_h: u32,
    text: Handle<Image>,
}

/// How the inspector is viewed: scrolled `row` rows down (plus what a
/// trackpad's pixel deltas moved short of a row), the rules a click opened
/// (by `cond_pc`), the body row under the pointer, and the line and rule
/// each body row on screen shows, for the next frame's click and hover.
/// The scroll resets when another actor is selected; open rules stay open.
#[derive(Resource, Debug, Default, Clone, PartialEq)]
struct PanelView {
    row: usize,
    rest: f32,
    open: BTreeSet<u32>,
    hover: Option<usize>,
    shown: Vec<(usize, Option<u32>)>,
}

impl PanelView {
    /// Scroll by `rows` (fractions kept), never before the first row.
    fn scroll(&mut self, rows: f32) {
        self.rest += rows;
        let whole = self.rest.trunc();
        self.rest -= whole;
        // A wheel moves a few rows a frame: far inside i64.
        #[allow(clippy::cast_possible_truncation)]
        let whole = whole as i64;
        let at = i64::try_from(self.row).unwrap_or(i64::MAX);
        self.row = usize::try_from(at.saturating_add(whole).max(0)).unwrap_or(0);
    }

    /// Open a closed rule, close an open one.
    fn toggle(&mut self, rule: u32) {
        if !self.open.remove(&rule) {
            self.open.insert(rule);
        }
    }
}

/// The cell frames of a frame: the map, the status rows, the inspector,
/// the menu.
#[derive(Resource, Debug, Default)]
struct Frames {
    map: CellFrame,
    status: CellFrame,
    panel: CellFrame,
    menu: CellFrame,
}

/// The tile layers on screen.
#[derive(Resource, Debug)]
struct Grids {
    map: Layer,
    status: Layer,
    /// Over the map's right side while an actor is selected.
    panel: Layer,
    panel_shown: bool,
    /// In the middle, over the gallery, on the menu screen; the status
    /// rows are hidden there.
    menu: Layer,
    menu_shown: bool,
    status_shown: bool,
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
    /// How far the map layer is stretched, across and down: 1 but on the
    /// menu, where the gallery fills the window to its edges.
    pub map_scale: (f32, f32),
    /// Height of the map area; the status rows take the rest.
    pub map_h: u32,
    pub status_cols: u32,
    pub status_h: u32,
    /// The inspector, over the map's right edge from pixel `panel_x`:
    /// `panel_cols x panel_rows` text cells of `panel_cell` (width, height),
    /// `0 x 0` while nobody is selected.
    pub panel_x: u32,
    pub panel_cols: u32,
    pub panel_rows: u32,
    pub panel_cell: (u32, u32),
    /// The menu box, from pixel `(menu_x, menu_y)`, in text cells; `0 x 0`
    /// while a world is played.
    pub menu_x: u32,
    pub menu_y: u32,
    pub menu_cols: u32,
    pub menu_rows: u32,
}

// ---- systems ---------------------------------------------------------------------------------

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, Msaa::Off));
    let map = Layer::spawn(&mut commands, 0.0);
    let panel = Layer::spawn(&mut commands, 1.0);
    let status = Layer::spawn(&mut commands, 2.0);
    let menu = Layer::spawn(&mut commands, 3.0);
    for e in [panel.bg, panel.fg, menu.bg, menu.fg] {
        commands.entity(e).insert(Visibility::Hidden);
    }
    commands.insert_resource(Grids {
        map,
        status,
        panel,
        panel_shown: false,
        menu,
        menu_shown: false,
        status_shown: true,
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
    wheel: Res<AccumulatedMouseScroll>,
    mut click: ResMut<Click>,
    mut selection: ResMut<Selection>,
    mut view: ResMut<PanelView>,
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
    let cursor = window
        .single()
        .ok()
        .and_then(Window::physical_cursor_position)
        .filter(|_| layout.visible);
    let over_panel = cursor.is_some_and(|at| {
        layout.panel_cols > 0 && at.x >= layout.panel_x as f32 && at.y < layout.map_h as f32
    });
    if mouse.just_pressed(MouseButton::Left)
        && let Some(at) = cursor
        && at.y < layout.map_h as f32
        && !over_panel
    {
        // The map; the status rows and the panel take no clicks.
        click.0 = Some(cell_at(&layout.map, layout.cell, at.x, at.y));
    }
    view.hover = None;
    if over_panel && let Some(at) = cursor {
        let ch = layout.panel_cell.1.max(1) as f32;
        // A pixel inside the window over a row a dozen pixels tall.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = (at.y / ch) as usize;
        view.hover = Some(row);
        if mouse.just_pressed(MouseButton::Left)
            && let Some(&(_, Some(rule))) = view.shown.get(row)
        {
            view.toggle(rule);
        }
        // Content follows the fingers (or the wheel): a positive delta
        // scrolls back.
        if wheel.delta.y != 0.0 {
            view.scroll(match wheel.unit {
                MouseScrollUnit::Line => -wheel.delta.y * WHEEL_STEP,
                MouseScrollUnit::Pixel => -wheel.delta.y * layout.scale / ch,
            });
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
    } else if esc || keys.just_pressed(KeyCode::KeyQ) {
        pending.quit = true;
    }
    if close.read().next().is_some() {
        pending.exit = true;
    }
}

/// Where the pointer was, and the wheel's pixels short of a row, for
/// [`menu_input`].
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct MenuPointer {
    at: Option<Vec2>,
    rest: f32,
}

/// The menu's keys and mouse. Keys: arrows, Enter, Esc, Backspace and what
/// is typed (layout-independent: the text a key makes), into [`Menu::key`].
/// Mouse: moving over a row chooses it (only a move: a still pointer leaves
/// the keys their choice), a click opens it, the wheel moves the choice, a
/// right click goes back. What the menu asks for goes to [`Pending`], or
/// opens a file in the editor here.
#[allow(clippy::too_many_arguments)]
fn menu_input(
    mut typed: MessageReader<KeyboardInput>,
    mut close: MessageReader<WindowCloseRequested>,
    window: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    wheel: Res<AccumulatedMouseScroll>,
    layout: Res<Layout>,
    mut pointer: Local<MenuPointer>,
    mut menu: ResMut<Menu>,
    mut pending: ResMut<Pending>,
) {
    if close.read().next().is_some() {
        pending.exit = true;
    }
    let mut asked = Vec::new();
    let (cw, ch) = (
        layout.panel_cell.0.max(1) as f32,
        layout.panel_cell.1.max(1) as f32,
    );
    let row = window
        .single()
        .ok()
        .and_then(Window::physical_cursor_position)
        .filter(|_| layout.visible && layout.menu_rows > 0)
        .and_then(|at| {
            let (x, y) = (at.x - layout.menu_x as f32, at.y - layout.menu_y as f32);
            let inside = x >= 0.0
                && y >= 0.0
                && x < layout.menu_cols as f32 * cw
                && y < layout.menu_rows as f32 * ch;
            // A pixel inside the box over a row a dozen pixels tall.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            inside.then_some(((y / ch) as usize, at))
        });
    if let Some((row, at)) = row {
        let moved = pointer.at != Some(at);
        pointer.at = Some(at);
        if let Some(i) = menu.item_at(row) {
            if moved {
                menu.hover(i);
            }
            if mouse.just_pressed(MouseButton::Left) {
                asked.push(menu.click(i));
            }
        }
    } else {
        pointer.at = None;
    }
    if mouse.just_pressed(MouseButton::Right) {
        asked.push(menu.key(menu::Key::Back));
    }
    // A notch of the wheel, or a row's height of a trackpad's pixels, is a
    // row; up moves the choice up.
    pointer.rest += match wheel.unit {
        MouseScrollUnit::Line => wheel.delta.y,
        MouseScrollUnit::Pixel => wheel.delta.y * layout.scale / ch,
    };
    while pointer.rest >= 1.0 {
        pointer.rest -= 1.0;
        asked.push(menu.key(menu::Key::Up));
    }
    while pointer.rest <= -1.0 {
        pointer.rest += 1.0;
        asked.push(menu.key(menu::Key::Down));
    }
    if wheel.delta.y == 0.0 {
        pointer.rest = 0.0;
    }
    for ev in typed.read() {
        if ev.state != ButtonState::Pressed {
            continue;
        }
        let key = match &ev.logical_key {
            Key::ArrowUp => menu::Key::Up,
            Key::ArrowDown => menu::Key::Down,
            Key::Enter => menu::Key::Enter,
            Key::Escape => menu::Key::Back,
            Key::Backspace => menu::Key::Erase,
            _ => match ev.text.as_ref().and_then(|t| t.chars().next()) {
                Some(c) if !c.is_control() => menu::Key::Char(c),
                _ => continue,
            },
        };
        asked.push(menu.key(key));
    }
    for action in asked.into_iter().flatten() {
        match action {
            menu::Action::Quit => pending.exit = true,
            menu::Action::Edit(path) => menu.note = Some(menu::edit(&path)),
            menu::Action::Load(dir) => pending.open = Some(Open::Load(dir)),
            menu::Action::New { scenario, dir } => {
                pending.open = Some(Open::New { scenario, dir });
            }
        }
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
            f64::from(p.x)
                + 0.5
                + f64::from(layout.width - layout.panel_x) / 2.0 / f64::from(layout.cell),
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
    screen: Res<Screen>,
    config: Res<SimConfig>,
    edge: Option<Res<MenuEdge>>,
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
    let menu = *screen == Screen::Menu;
    // `Zoom` holds the cell to a few dozen logical pixels and the scale is a
    // DPI factor: a small whole number of pixels, at least 1. The menu fits
    // the gallery's whole initial region in the window instead.
    #[allow(clippy::cast_possible_truncation)]
    let cell = if menu {
        (width / config.initial_width.max(1))
            .min(height / config.initial_height.max(1))
            .max(1)
    } else {
        ((zoom.cell_logical as f32) * scale).round().max(1.0) as u32
    };
    if glyphs.cell != cell {
        glyphs.tileset = images.add(GlyphAtlas::build(cell).tileset());
        glyphs.cell = cell;
    }
    // A DPI factor times a dozen logical pixels: small, at least 1.
    #[allow(clippy::cast_possible_truncation)]
    let text_h = ((PANEL_TEXT_LOGICAL as f32) * scale).round().max(1.0) as u32;
    let text_w = GlyphAtlas::text_width(text_h);
    if glyphs.text_h != text_h {
        glyphs.text = images.add(GlyphAtlas::text(text_h).tileset());
        glyphs.text_h = text_h;
    }
    let status_h = if menu {
        0
    } else {
        (STATUS_ROWS * cell).min(height)
    };
    let map_h = height - status_h;
    // The menu over the gallery: the gallery whole, stretched to the window's
    // edges (its cells a little off square), not a fitted box in a margin.
    let fill = menu && edge.as_ref().is_some_and(|e| e.0.is_some());
    let (map, map_scale) = if fill {
        let (w, h) = (config.initial_width.max(1), config.initial_height.max(1));
        let map = MapLayout {
            origin: Pos::new(0, 0),
            shift_x: 0,
            shift_y: 0,
            cols: w as usize,
            rows: h as usize,
        };
        // A window a few thousand pixels wide over a few hundred cells.
        #[allow(clippy::cast_precision_loss)]
        let s = (
            width as f32 / (w * cell) as f32,
            height as f32 / (h * cell) as f32,
        );
        (map, s)
    } else {
        (
            map_layout(&camera, cell as usize, width as usize, map_h as usize),
            (1.0, 1.0),
        )
    };
    let status_cols = width.div_ceil(cell);
    // The menu box: centred, as wide as it can be while its right edge stays
    // left of the gallery's split (the right column then stands clear).
    let split_px = edge
        .and_then(|e| e.0)
        .filter(|_| fill)
        .and_then(|x| u32::try_from(x).ok())
        .map(|x| x * width / config.initial_width.max(1))
        .filter(|&px| px > width / 2 + 8 * text_w);
    let (menu_cols, menu_rows) = if menu {
        let room = split_px.map_or_else(
            || (width / text_w).saturating_sub(4).min(MENU_COLS),
            |px| (2 * px - width) / text_w,
        );
        (room, (height / text_h).saturating_sub(2).min(MENU_ROWS))
    } else {
        (0, 0)
    };
    let (panel_cols, panel_rows) = if selection.uid.is_some() {
        (
            (width * 2 / 5 / text_w)
                .clamp(PANEL_MIN, PANEL_MAX)
                .min(width / text_w),
            // Down to the status rows: the last row may go under them.
            map_h.div_ceil(text_h).max(1),
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
        map_scale,
        map_h,
        status_cols,
        status_h,
        panel_x: width - panel_cols * text_w,
        panel_cols,
        panel_rows,
        panel_cell: (text_w, text_h),
        menu_x: (width - menu_cols * text_w) / 2,
        menu_y: (height - menu_rows * text_h) / 2,
        menu_cols,
        menu_rows,
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
        menu: mn,
        menu_shown,
        status_shown,
    } = &mut *grids;
    let show_menu = menu_cols > 0 && menu_rows > 0;
    if show_menu && (mn.cols, mn.rows, mn.cell, mn.cell_h) != (menu_cols, menu_rows, text_w, text_h)
    {
        mn.configure(
            &mut commands,
            menu_cols,
            menu_rows,
            (text_w, text_h),
            &glyphs.text,
        );
    }
    for (layer, shown, want) in [(&*mn, menu_shown, show_menu), (&*s, status_shown, !menu)] {
        if *shown != want {
            *shown = want;
            let v = if want {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            for e in [layer.bg, layer.fg] {
                commands.entity(e).insert(v);
            }
        }
    }
    if shown && (p.cols, p.rows, p.cell, p.cell_h) != (panel_cols, panel_rows, text_w, text_h) {
        p.configure(
            &mut commands,
            panel_cols,
            panel_rows,
            (text_w, text_h),
            &glyphs.text,
        );
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
        m.configure(&mut commands, cols, rows, (cell, cell), &glyphs.tileset);
    }
    if !menu && (s.cols, s.rows, s.cell) != (status_cols, STATUS_ROWS, cell) {
        s.configure(
            &mut commands,
            status_cols,
            STATUS_ROWS,
            (cell, cell),
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
    // The gallery has no save: what leaves the view is dropped.
    let store = world.remove_resource::<Store>();
    let streamed = sim::ensure_loaded(world, focus, policy, store.as_ref());
    if let Some(store) = store {
        world.insert_resource(store);
    }
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

    let screen = *world.resource::<Screen>();
    let leaving = pending.quit || pending.exit || pending.open.is_some();
    if screen == Screen::Play && (pending.save || leaving) {
        match save(world) {
            Ok(n) => info!("saved {n} chunks"),
            Err(e) => {
                error!("saving: {e:#}");
                if leaving {
                    world.write_message(AppExit::error());
                    return;
                }
            }
        }
    }
    let home = world.resource::<Home>().0;
    if pending.exit || (pending.quit && !(home && screen == Screen::Play)) {
        world.write_message(AppExit::Success);
        return;
    }
    let open = if pending.quit {
        Some(Open::Gallery)
    } else {
        pending.open
    };
    // The gallery starts over once its busy beaver has filled its panel.
    let open = open.or_else(|| {
        (screen == Screen::Menu && sim::tick(world) >= time::START_TICK + GALLERY_TICKS)
            .then_some(Open::Gallery)
    });
    if let Some(open) = open
        && let Err(e) = switch(world, open)
    {
        let text = format!("{e:#}");
        error!("{text}");
        match world.get_resource_mut::<Menu>() {
            Some(mut m) => m.note = Some((text, false)),
            None => world.resource_mut::<Notice>().show(text),
        }
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
    if sel.uid != world.resource::<Selection>().uid {
        let mut view = world.resource_mut::<PanelView>();
        view.row = 0;
        view.rest = 0.0;
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
                    for span in ins.lines.iter_mut().flat_map(|l| &mut l.spans) {
                        span.1 = FAINT;
                    }
                    ins.lines.insert(
                        0,
                        Line::plain(
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
    (screen, home, menu): (Res<Screen>, Res<Home>, Option<ResMut<Menu>>),
    mut pview: ResMut<PanelView>,
    mut frames: ResMut<Frames>,
) {
    if !layout.visible {
        return;
    }
    let on_menu = *screen == Screen::Menu;
    let map = layout.map;
    frames.map.resize(map.cols, map.rows);
    let view = Viewport {
        origin: map.origin,
        width: u32::try_from(map.cols).expect("the map is a window wide"),
        height: u32::try_from(map.rows).expect("the map is a window tall"),
    };
    // The gallery is shown in full daylight, whatever its hour.
    let light = if on_menu {
        255
    } else {
        brightness(time::daylight(tick.0))
    };
    // The gallery on a purple-black ground: its pictures stand out.
    // On cells too small for a glyph, whoever stands is a square of colour.
    let looks = Looks {
        soil: if on_menu { NIGHT_SOIL } else { SOIL },
        tiny: layout.cell < GLYPH_MIN,
        ..Looks::of(&kinds)
    };
    render_cells(|c| stage.chunk(c), looks, view, light, &mut frames.map);
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
        // The rows that fit above the status bar: the text, then the keys;
        // a row past them lies under the status rows, blank.
        let ch = layout.panel_cell.1.max(1);
        let fit = usize::try_from(layout.map_h / ch).map_or(1, |f| f.clamp(1, pr));
        let body = fit - 1;
        let rows = inspect::rows(&inspector.lines, pc - 1, |r| pview.open.contains(&r));
        pview.row = pview.row.min(rows.len().saturating_sub(body));
        let first = pview.row;
        let hovered = pview
            .hover
            .filter(|&h| h < body)
            .and_then(|h| rows.get(first + h))
            .filter(|r| !r.cells.is_empty())
            .map(|r| r.line);
        pview.shown.clear();
        for i in 0..pr {
            match rows.get(first + i).filter(|_| i < body) {
                Some(r) => {
                    let bg = if Some(r.line) == hovered {
                        HOVER_BG
                    } else {
                        TEXT_BG
                    };
                    // A column of margin, then the text.
                    let mut cells = vec![(b' ', TEXT_FG)];
                    cells.extend_from_slice(&r.cells);
                    frames.panel.put_cells(i, &cells, bg);
                    pview.shown.push((r.line, r.rule));
                }
                None if i == body => {
                    let footer = panel_footer(rows.len(), body, first);
                    frames.panel.put_text(i, &footer, DIM, TEXT_BG);
                }
                None => frames.panel.put_text(i, "", TEXT_FG, TEXT_BG),
            }
        }
    }

    let (mc, mr) = (layout.menu_cols as usize, layout.menu_rows as usize);
    frames.menu.resize(mc, mr);
    if let Some(mut menu) = menu.filter(|_| on_menu && mc > 0 && mr > 0) {
        let rows = menu.rows(mc, mr);
        for i in 0..mr {
            match rows.get(i) {
                Some(r) => frames.menu.put_cells(i, &r.cells, r.bg),
                None => frames.menu.put_text(i, "", TEXT_FG, TEXT_BG),
            }
        }
    }
    if on_menu {
        return;
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
    let help = if home.0 { HELP_HOME } else { HELP };
    frames
        .status
        .put_text(3, notice.current().unwrap_or(help), TEXT_FG, TEXT_BG);
}

/// The panel's last row: the keys, and which rows are in view when there
/// are more than `body` (`rows 12-40 of 75`), from row `first`.
fn panel_footer(rows: usize, body: usize, first: usize) -> String {
    let mut footer = format!(" {KEYS}");
    if rows > body {
        footer.push_str(&format!(
            " | scroll: rows {}-{} of {rows}",
            (first + 1).min(rows),
            (first + body).min(rows)
        ));
    }
    footer
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
    // Stretched (the menu's gallery): the layer fills the window, centred on it.
    let stretched = layout.map_scale != (1.0, 1.0);
    let map_at = if stretched {
        (
            Vec3::new(0.0, 0.0, grids.map.z),
            Vec3::new(0.0, 0.0, grids.map.z + 0.5),
        )
    } else {
        grids
            .map
            .place(-(map.shift_x as f32), -(map.shift_y as f32), w, h)
    };
    let map_scale = Vec3::new(layout.map_scale.0, layout.map_scale.1, 1.0);
    let status_at = grids
        .status
        .place(0.0, (layout.height - layout.status_h) as f32, w, h);
    let panel_at = grids.panel.place(layout.panel_x as f32, 0.0, w, h);
    let menu_at = grids
        .menu
        .place(layout.menu_x as f32, layout.menu_y as f32, w, h);
    for (layer, frame, (bg_at, fg_at), alpha) in [
        (&grids.map, &frames.map, map_at, 1.0),
        (&grids.status, &frames.status, status_at, 1.0),
        (&grids.panel, &frames.panel, panel_at, PANEL_ALPHA),
        (&grids.menu, &frames.menu, menu_at, MENU_ALPHA),
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
        let scale = if layer.bg == grids.map.bg {
            map_scale
        } else {
            Vec3::ONE
        };
        bg_tf.scale = scale;
        fg_tf.scale = scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `menu_input` against a menu on screen: a click on a row opens it,
    /// the wheel moves the choice, a right click goes back. (Synthetic
    /// clicks posted to the window do not reach it, so this is the check.)
    #[test]
    fn the_menu_takes_clicks_the_wheel_and_a_right_click() {
        use bevy::ecs::message::Messages;
        use bevy::ecs::system::RunSystemOnce;
        use bevy::math::DVec2;
        let root = std::env::temp_dir().join(format!("wmc-menu-input-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scenarios")).unwrap();
        for n in ["a", "b", "c", "d"] {
            std::fs::write(root.join(format!("scenarios/{n}.scenario")), "seed 1\n").unwrap();
        }
        let mut menu = Menu::scan(&root);
        let (cw, ch, x0, y0) = (8u32, 16u32, 100u32, 50u32);
        let text: Vec<String> = menu
            .rows(100, 26)
            .iter()
            .map(|r| r.cells.iter().map(|c| char::from(c.0)).collect())
            .collect();
        let row = text.iter().position(|l| l.contains("New world")).unwrap();
        let mut world = World::new();
        world.init_resource::<Messages<KeyboardInput>>();
        world.init_resource::<Messages<WindowCloseRequested>>();
        world.insert_resource(menu);
        world.insert_resource(Pending::default());
        world.insert_resource(AccumulatedMouseScroll::default());
        world.insert_resource(Layout {
            visible: true,
            scale: 1.0,
            panel_cell: (cw, ch),
            menu_x: x0,
            menu_y: y0,
            menu_cols: 100,
            menu_rows: 26,
            ..Layout::default()
        });
        let mut window = Window::default();
        let y = f64::from(y0 + ch * u32::try_from(row).unwrap() + ch / 2);
        window.set_physical_cursor_position(Some(DVec2::new(f64::from(x0 + 5 * cw), y)));
        world.spawn((window, PrimaryWindow));
        let mut buttons = ButtonInput::<MouseButton>::default();
        buttons.press(MouseButton::Left);
        world.insert_resource(buttons);
        world.run_system_once(menu_input).unwrap();
        assert_eq!(
            world.resource::<Menu>().page,
            menu::Page::New,
            "clicked open"
        );

        // Two notches down the wheel, the pointer off the box (each run here
        // is a new system, which takes a still pointer for a move): the
        // third scenario.
        let mut windows = world.query::<&mut Window>();
        windows
            .single_mut(&mut world)
            .unwrap()
            .set_physical_cursor_position(None);
        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        world.resource_mut::<AccumulatedMouseScroll>().delta = Vec2::new(0.0, -2.0);
        world.resource_mut::<Menu>().rows(100, 26);
        world.run_system_once(menu_input).unwrap();
        assert_eq!(world.resource::<Menu>().at, 2);

        // A right click: back to the first page.
        world.resource_mut::<AccumulatedMouseScroll>().delta = Vec2::ZERO;
        let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
        buttons.release_all();
        buttons.clear();
        buttons.press(MouseButton::Right);
        world.run_system_once(menu_input).unwrap();
        assert_eq!(world.resource::<Menu>().page, menu::Page::Main);
        assert_eq!(
            world.resource::<Pending>(),
            &Pending::default(),
            "nothing to open"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

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
    fn panel_view_scrolls_in_fractions_and_toggles_rules() {
        let mut v = PanelView::default();
        v.scroll(3.0);
        assert_eq!(v.row, 3);
        // A trackpad: a third of a row at a time adds up.
        for _ in 0..3 {
            v.scroll(1.0 / 3.0 + 1e-4);
        }
        assert_eq!(v.row, 4);
        v.scroll(-100.0);
        assert_eq!(v.row, 0, "never before the first row");
        v.toggle(9);
        v.toggle(4);
        assert!(v.open.contains(&9) && v.open.contains(&4));
        v.toggle(9);
        assert_eq!(v.open.iter().copied().collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn the_footer_says_where_the_view_is_only_when_it_scrolls() {
        assert_eq!(panel_footer(10, 20, 0), format!(" {KEYS}"));
        let f = panel_footer(75, 20, 0);
        assert!(f.ends_with("scroll: rows 1-20 of 75"), "{f}");
        let f = panel_footer(75, 20, 60);
        assert!(f.ends_with("rows 61-75 of 75"), "{f}");
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
