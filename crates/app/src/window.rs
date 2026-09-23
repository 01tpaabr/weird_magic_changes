//! Windowed front end: a resizable `winit` window, a `pixels` framebuffer and
//! the keyboard camera.
//!
//! A frame is: stream chunks around the camera -> `render_cells` (row-parallel)
//! -> `blit` (band-parallel Zig kernel) -> present. The loop is event-driven:
//! it redraws after input or resize only, and the sim ticks on demand. When
//! the sim runs continuously this becomes a fixed-timestep loop; the frame
//! pipeline does not change.
//!
//! Keys: `w a s d` / arrows pan by 1, with Shift by 8; `space` steps the sim;
//! `p` saves; `+`/`-` zoom; `q` / `Esc` / close saves and quits.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use pixels::{Pixels, SurfaceTexture};
use sim_core::{CHUNK_SIZE, LoadPolicy, Store, StreamStats, World};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::camera::Camera;
use crate::render::atlas::GlyphAtlas;
use crate::render::blit::blit;
use crate::render::cells::{CellFrame, Viewport, render_cells};
use crate::render::palette::{TEXT_BG, TEXT_FG, VOID_BG};

/// Cell edge in logical pixels at startup; multiplied by the window's scale
/// factor (2 on Retina) to get the physical cell the atlas is built for.
pub const DEFAULT_CELL_LOGICAL: u32 = 16;
const MIN_CELL_LOGICAL: u32 = 6;
const MAX_CELL_LOGICAL: u32 = 64;
const ZOOM_STEP: u32 = 2;
/// Rows reserved under the map for status text.
const STATUS_ROWS: u32 = 2;
const FAST_STEP: i32 = 8;
const HELP: &str = "wasd/arrows move, shift x8, space tick, p save, +/- zoom, q quit";

/// Load radius that keeps the whole view plus one chunk of margin loaded.
fn policy_for(view_w: u32, view_h: u32) -> LoadPolicy {
    let half_chunks = |cells: u32| (cells / 2).div_ceil(CHUNK_SIZE as u32) as i32;
    let load = half_chunks(view_w).max(half_chunks(view_h)) + 1;
    LoadPolicy {
        load,
        unload: load + 2,
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
        frame: CellFrame::new(),
        cols: 0,
        rows: 0,
        buf_width: 0,
        modifiers: ModifiersState::empty(),
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

struct Gpu {
    window: Arc<Window>,
    pixels: Pixels<'static>,
}

struct App<'a> {
    world: &'a mut World,
    camera: &'a mut Camera,
    store: &'a Store,
    title: String,
    cell_logical: u32,
    gpu: Option<Gpu>,
    atlas: Option<GlyphAtlas>,
    frame: CellFrame,
    /// Grid that fits the current buffer, in cells (0 while minimised).
    cols: u32,
    rows: u32,
    /// Pixel buffer width = row stride, in pixels.
    buf_width: usize,
    modifiers: ModifiersState,
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

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(LogicalSize::new(240.0, 160.0));
        let window = Arc::new(event_loop.create_window(attrs).context("creating window")?);
        let size = window.inner_size();
        let surface = SurfaceTexture::new(size.width.max(1), size.height.max(1), window.clone());
        let pixels = Pixels::new(size.width.max(1), size.height.max(1), surface)
            .map_err(|e| anyhow!("creating pixel buffer: {e}"))?;
        self.gpu = Some(Gpu { window, pixels });
        self.layout()
    }

    /// Recompute the grid after a resize, DPI change or zoom; rebuild the
    /// atlas if the physical cell size changed.
    fn layout(&mut self) -> anyhow::Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let size = gpu.window.inner_size();
        if size.width == 0 || size.height == 0 {
            self.cols = 0;
            self.rows = 0;
            return Ok(());
        }
        let cell = (f64::from(self.cell_logical) * gpu.window.scale_factor()).round() as u32;
        let cell = cell.max(1);
        if self
            .atlas
            .as_ref()
            .is_none_or(|a| a.cell() != cell as usize)
        {
            self.atlas = Some(GlyphAtlas::build(cell));
        }
        gpu.pixels
            .resize_surface(size.width, size.height)
            .map_err(|e| anyhow!("resizing surface: {e}"))?;
        gpu.pixels
            .resize_buffer(size.width, size.height)
            .map_err(|e| anyhow!("resizing pixel buffer: {e}"))?;
        self.buf_width = size.width as usize;
        self.cols = size.width / cell;
        self.rows = size.height / cell;
        // The blit never touches the partial cell at the right/bottom edge.
        for px in gpu.pixels.frame_mut().chunks_exact_mut(4) {
            px.copy_from_slice(&VOID_BG.bytes());
        }
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
        if self.cols == 0 || self.rows == 0 {
            return Ok(());
        }
        let map_rows = self.rows.saturating_sub(STATUS_ROWS).max(1);
        let stats = self
            .world
            .ensure_loaded(
                self.camera.center,
                policy_for(self.cols, map_rows),
                Some(self.store),
            )
            .context("streaming chunks")?;
        if stats.generated + stats.read + stats.unloaded + stats.written > 0 {
            self.last_stats = Some(stats);
        }

        self.frame.resize(self.cols as usize, self.rows as usize);
        let view = Viewport::centered(self.camera.center, self.cols, map_rows);
        render_cells(&self.world.stage, view, &mut self.frame);

        let (cc, _) = self.camera.center.split();
        let status = format!(
            "cam ({}, {}) chunk ({}, {}) | {}x{} cells @ {}px | loaded {} | tick {} | last stream: {}",
            self.camera.center.x,
            self.camera.center.y,
            cc.x,
            cc.y,
            self.cols,
            map_rows,
            atlas.cell(),
            self.world.stage.loaded_count(),
            self.world.tick,
            self.last_stats.map_or_else(
                || "-".to_string(),
                |s| format!(
                    "gen {} read {} unload {} write {}",
                    s.generated, s.read, s.unloaded, s.written
                )
            ),
        );
        self.frame
            .put_text(map_rows as usize, &status, TEXT_FG, TEXT_BG);
        self.frame
            .put_text(map_rows as usize + 1, HELP, TEXT_FG, TEXT_BG);

        blit(&self.frame, atlas, gpu.pixels.frame_mut(), self.buf_width);
        gpu.pixels
            .render()
            .map_err(|e| anyhow!("presenting frame: {e}"))
    }

    /// Returns `Ok(true)` when the key asks to quit.
    fn key(&mut self, code: KeyCode) -> anyhow::Result<bool> {
        let step = if self.modifiers.shift_key() {
            FAST_STEP
        } else {
            1
        };
        match code {
            KeyCode::KeyQ | KeyCode::Escape => return Ok(true),
            KeyCode::KeyW | KeyCode::ArrowUp => self.camera.pan(0, -step),
            KeyCode::KeyS | KeyCode::ArrowDown => self.camera.pan(0, step),
            KeyCode::KeyA | KeyCode::ArrowLeft => self.camera.pan(-step, 0),
            KeyCode::KeyD | KeyCode::ArrowRight => self.camera.pan(step, 0),
            KeyCode::Space => self.world.step(),
            KeyCode::KeyP => save(self.world, self.camera, self.store)?,
            KeyCode::Equal | KeyCode::NumpadAdd => self.zoom(ZOOM_STEP as i32)?,
            KeyCode::Minus | KeyCode::NumpadSubtract => self.zoom(-(ZOOM_STEP as i32))?,
            _ => return Ok(false),
        }
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
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
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.physical_key {
                    PhysicalKey::Code(code) => self.key(code).map(|quit| {
                        if quit {
                            event_loop.exit();
                        }
                    }),
                    PhysicalKey::Unidentified(_) => Ok(()),
                }
            }
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
}
