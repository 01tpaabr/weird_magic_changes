//! Windowed front end: a resizable `winit` window, a `softbuffer` framebuffer
//! and the keyboard camera.
//!
//! A frame is: advance the camera -> stream chunks around it -> run the
//! ticks the [`Clock`] owes (bounded by [`TICK_BUDGET`]) -> `render_cells`
//! (row-parallel, dimmed by the sim's daylight) -> `blit` (band-parallel Zig
//! kernel) -> present. The loop is event-driven: a redraw follows input or a
//! resize, and while the camera moves or the sim runs each frame requests the
//! next one, which AppKit paces to the display refresh.
//!
//! Keys: hold `w a s d` / arrows to glide (two keys = diagonal), Shift for
//! x4 speed; `space` pauses/resumes the sim; `.` runs one tick and pauses;
//! `[` / `]` slow down / speed up (1x .. 16x, max); `p` saves; `+`/`-` zoom;
//! `q` / `Esc` / close saves and quits.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use sim_core::{CHUNK_SIZE, LoadPolicy, Pos, Store, StreamStats, World, time};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::camera::{Camera, Input};
use crate::clock::Clock;
use crate::render::atlas::GlyphAtlas;
use crate::render::blit::{Target, blit};
use crate::render::cells::{CellFrame, Viewport, render_cells};
use crate::render::palette::{TEXT_BG, TEXT_FG, brightness};

/// Cell edge in logical pixels at startup; multiplied by the window's scale
/// factor (2 on Retina) to get the physical cell the atlas is built for.
pub const DEFAULT_CELL_LOGICAL: u32 = 16;
const MIN_CELL_LOGICAL: u32 = 6;
const MAX_CELL_LOGICAL: u32 = 64;
const ZOOM_STEP: u32 = 2;
/// Rows reserved under the map for status text.
const STATUS_ROWS: usize = 2;
/// Longest frame time fed to the camera: a stall becomes a small step, not a leap.
const MAX_DT: f64 = 0.1;
/// Sim time per frame. The rest of a 60 Hz frame is for streaming and drawing;
/// at max speed this is how long each frame ticks for.
pub const TICK_BUDGET: Duration = Duration::from_millis(10);
const HELP: &str =
    "wasd/arrows move (shift x4) | space pause | . step | [ ] speed | p save | +/- zoom | q quit";

/// Load radius that keeps the whole view plus one chunk of margin loaded.
fn policy_for(view_w: u32, view_h: u32) -> LoadPolicy {
    let half_chunks = |cells: u32| (cells / 2).div_ceil(CHUNK_SIZE as u32) as i32;
    let load = half_chunks(view_w).max(half_chunks(view_h)) + 1;
    LoadPolicy {
        load,
        unload: load + 2,
    }
}

/// Where the map grid sits so that a fractional camera centre lands on the
/// middle of a `width x height` pixel window: the first visible cell and the
/// pixel shift of the grid (0..cell), plus how many cells cover the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MapLayout {
    origin: Pos,
    shift_x: usize,
    shift_y: usize,
    cols: usize,
    rows: usize,
}

fn map_layout(camera: &Camera, cell: usize, width: usize, height: usize) -> MapLayout {
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

pub fn run(world: &mut World, camera: &mut Camera, store: &Store) -> anyhow::Result<()> {
    let event_loop = EventLoop::new().context("creating event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        world,
        camera,
        store,
        title: format!("wmc - {}", store.dir().display()),
        cell_logical: DEFAULT_CELL_LOGICAL,
        gpu: None,
        atlas: None,
        map: CellFrame::new(),
        status: CellFrame::new(),
        buf_width: 0,
        buf_height: 0,
        held: Held::default(),
        modifiers: ModifiersState::empty(),
        clock: Clock::new(),
        last_frame: None,
        last_stats: None,
        error: None,
    };
    event_loop.run_app(&mut app).context("event loop")?;
    let error = app.error.take();
    // Window is gone; save whatever state we reached, even after an error.
    save(app.world, app.camera, app.store)?;
    error.map_or(Ok(()), Err)
}

fn save(world: &mut World, camera: &Camera, store: &Store) -> anyhow::Result<()> {
    world.save(store).context("saving world")?;
    camera.save(store.dir()).context("saving camera")?;
    Ok(())
}

/// Softbuffer surface: hands us a `u32` buffer per frame and presents it.
type Surface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

struct Gpu {
    window: Arc<Window>,
    surface: Surface,
}

/// Movement keys currently down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Held {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl Held {
    fn any(self) -> bool {
        self.up || self.down || self.left || self.right
    }

    fn input(self, fast: bool) -> Input {
        Input {
            dx: i8::from(self.right) - i8::from(self.left),
            dy: i8::from(self.down) - i8::from(self.up),
            fast,
        }
    }
}

struct App<'a> {
    world: &'a mut World,
    camera: &'a mut Camera,
    store: &'a Store,
    title: String,
    cell_logical: u32,
    gpu: Option<Gpu>,
    atlas: Option<GlyphAtlas>,
    map: CellFrame,
    status: CellFrame,
    /// Pixel buffer size; width is also the row stride, in pixels.
    buf_width: usize,
    buf_height: usize,
    held: Held,
    modifiers: ModifiersState,
    clock: Clock,
    /// When the previous frame was drawn, while the camera is animating or
    /// the sim is running.
    last_frame: Option<Instant>,
    last_stats: Option<StreamStats>,
    /// First fatal error; the loop exits and `run` returns it.
    error: Option<anyhow::Error>,
}

impl App<'_> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, e: anyhow::Error) {
        if self.error.is_none() {
            self.error = Some(e);
        }
        event_loop.exit();
    }

    fn request_redraw(&self) {
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(LogicalSize::new(240.0, 160.0));
        let window = Arc::new(event_loop.create_window(attrs).context("creating window")?);
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("creating softbuffer context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("creating softbuffer surface: {e}"))?;
        self.gpu = Some(Gpu { window, surface });
        self.layout()
    }

    /// Recompute the buffer after a resize, DPI change or zoom; rebuild the
    /// atlas if the physical cell size changed.
    fn layout(&mut self) -> anyhow::Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let size = gpu.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            // Minimised: nothing to draw until the next resize.
            self.buf_width = 0;
            self.buf_height = 0;
            return Ok(());
        };
        let cell = (f64::from(self.cell_logical) * gpu.window.scale_factor()).round() as u32;
        let cell = cell.max(1);
        if self
            .atlas
            .as_ref()
            .is_none_or(|a| a.cell() != cell as usize)
        {
            self.atlas = Some(GlyphAtlas::build(cell));
        }
        gpu.surface
            .resize(w, h)
            .map_err(|e| anyhow!("resizing surface: {e}"))?;
        self.buf_width = size.width as usize;
        self.buf_height = size.height as usize;
        gpu.window.request_redraw();
        Ok(())
    }

    fn zoom(&mut self, delta: i32) -> anyhow::Result<()> {
        let next = self
            .cell_logical
            .saturating_add_signed(delta)
            .clamp(MIN_CELL_LOGICAL, MAX_CELL_LOGICAL);
        if next != self.cell_logical {
            self.cell_logical = next;
            self.layout()?;
        }
        Ok(())
    }

    fn draw(&mut self) -> anyhow::Result<()> {
        let (Some(gpu), Some(atlas)) = (self.gpu.as_mut(), self.atlas.as_ref()) else {
            return Ok(());
        };
        let (width, height) = (self.buf_width, self.buf_height);
        if width == 0 || height == 0 {
            return Ok(());
        }

        // Advance the camera by the time since the last animated frame.
        let now = Instant::now();
        let dt = self
            .last_frame
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.camera
            .update(dt.min(MAX_DT), self.held.input(self.modifiers.shift_key()));
        let animating = self.held.any() || self.camera.moving() || self.clock.running();
        self.last_frame = animating.then_some(now);

        let cell = atlas.cell();
        let status_h = (STATUS_ROWS * cell).min(height);
        let map_h = height - status_h;
        let layout = map_layout(self.camera, cell, width, map_h);

        let stats = self
            .world
            .ensure_loaded(
                self.camera.cell(),
                policy_for(layout.cols as u32, layout.rows as u32),
                Some(self.store),
            )
            .context("streaming chunks")?;
        if stats.generated + stats.read + stats.unloaded + stats.written > 0 {
            self.last_stats = Some(stats);
        }

        // Sim: the ticks this frame owes, on the loaded set.
        let world = &mut *self.world;
        self.clock.run(dt, TICK_BUDGET, || world.step());

        // Phase 1: cells, dimmed by the sim's daylight.
        self.map.resize(layout.cols, layout.rows);
        let view = Viewport {
            origin: layout.origin,
            width: layout.cols as u32,
            height: layout.rows as u32,
        };
        let light = brightness(time::daylight(self.world.tick));
        render_cells(&self.world.stage, view, light, &mut self.map);

        let status = format!(
            "{} | {} | tick {} | cam ({:.1}, {:.1}) chunk ({}, {}) | {}x{} @ {}px | loaded {} | stream: {}",
            time::Clock::at(self.world.tick),
            self.clock.label(),
            self.world.tick,
            self.camera.x,
            self.camera.y,
            self.camera.cell().split().0.x,
            self.camera.cell().split().0.y,
            layout.cols,
            layout.rows,
            cell,
            self.world.stage.loaded_count(),
            self.last_stats.map_or_else(
                || "-".to_string(),
                |s| format!(
                    "gen {} read {} unload {} write {}",
                    s.generated, s.read, s.unloaded, s.written
                )
            ),
        );
        self.status.resize(width.div_ceil(cell), STATUS_ROWS);
        self.status.put_text(0, &status, TEXT_FG, TEXT_BG);
        self.status.put_text(1, HELP, TEXT_FG, TEXT_BG);

        // Phase 2: pixels. The map covers its window completely (the layout
        // adds a cell of slack for the shift); the status bar is clipped.
        let mut buffer = gpu
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("acquiring frame buffer: {e}"))?;
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut buffer);
        let (map_px, status_px) = bytes.split_at_mut(map_h * width * 4);
        blit(
            &self.map,
            atlas,
            Target {
                pixels: map_px,
                stride_px: width,
                width,
                height: map_h,
                origin_x: -(layout.shift_x as isize),
                origin_y: -(layout.shift_y as isize),
            },
        );
        blit(
            &self.status,
            atlas,
            Target {
                pixels: status_px,
                stride_px: width,
                width,
                height: status_h,
                origin_x: 0,
                origin_y: 0,
            },
        );
        buffer
            .present()
            .map_err(|e| anyhow!("presenting frame: {e}"))?;

        if animating {
            gpu.window.request_redraw();
        }
        Ok(())
    }

    /// Returns `Ok(true)` when the key asks to quit.
    fn key(&mut self, code: KeyCode, state: ElementState) -> anyhow::Result<bool> {
        let pressed = state == ElementState::Pressed;
        let movement = match code {
            KeyCode::KeyW | KeyCode::ArrowUp => Some(&mut self.held.up),
            KeyCode::KeyS | KeyCode::ArrowDown => Some(&mut self.held.down),
            KeyCode::KeyA | KeyCode::ArrowLeft => Some(&mut self.held.left),
            KeyCode::KeyD | KeyCode::ArrowRight => Some(&mut self.held.right),
            _ => None,
        };
        if let Some(flag) = movement {
            *flag = pressed;
            self.request_redraw();
            return Ok(false);
        }
        if !pressed {
            return Ok(false);
        }
        match code {
            KeyCode::KeyQ | KeyCode::Escape => return Ok(true),
            KeyCode::Space => self.clock.toggle_pause(),
            KeyCode::Period => {
                self.clock.pause();
                self.world.step();
            }
            KeyCode::BracketLeft => self.clock.slower(),
            KeyCode::BracketRight => self.clock.faster(),
            KeyCode::KeyP => save(self.world, self.camera, self.store)?,
            KeyCode::Equal | KeyCode::NumpadAdd => self.zoom(ZOOM_STEP as i32)?,
            KeyCode::Minus | KeyCode::NumpadSubtract => self.zoom(-(ZOOM_STEP as i32))?,
            _ => return Ok(false),
        }
        self.request_redraw();
        Ok(false)
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        if let Err(e) = self.create_window(event_loop) {
            self.fail(event_loop, e);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let result = match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                Ok(())
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.layout(),
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                Ok(())
            }
            WindowEvent::Focused(false) => {
                // Key releases are lost while unfocused; don't keep gliding.
                self.held = Held::default();
                self.modifiers = ModifiersState::empty();
                Ok(())
            }
            WindowEvent::KeyboardInput { event, .. } => match event.physical_key {
                PhysicalKey::Code(code) => self.key(code, event.state).map(|quit| {
                    if quit {
                        event_loop.exit();
                    }
                }),
                PhysicalKey::Unidentified(_) => Ok(()),
            },
            WindowEvent::RedrawRequested => self.draw(),
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.fail(event_loop, e);
        }
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
        let cam = Camera::new(Pos::new(10, 5)); // (10.5, 5.5)
        let l = map_layout(&cam, 16, 160, 96); // 10 x 6 cells
        assert_eq!(l.origin, Pos::new(5, 2));
        assert_eq!((l.shift_x, l.shift_y), (8, 8));
        assert_eq!((l.cols, l.rows), (11, 7));
        // Any camera (off exact cell boundaries, where a half pixel rounds
        // either way): the grid must cover the whole window.
        for (x, y) in [(0.02, 0.03), (-3.7, 2.2), (1234.9, -777.01), (0.49, 0.51)] {
            let mut cam = Camera::new(Pos::new(0, 0));
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
    fn held_keys_become_input() {
        let h = Held {
            up: true,
            left: true,
            ..Held::default()
        };
        assert_eq!(
            h.input(true),
            Input {
                dx: -1,
                dy: -1,
                fast: true
            }
        );
        let h = Held {
            up: true,
            down: true,
            ..Held::default()
        };
        assert_eq!(
            h.input(false),
            Input {
                dx: 0,
                dy: 0,
                fast: false
            }
        );
        assert!(!Held::default().any());
    }
}
