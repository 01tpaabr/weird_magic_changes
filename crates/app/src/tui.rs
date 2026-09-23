//! Interactive terminal front end: WASD camera, chunk streaming, save.
//!
//! Keys: `w a s d` / arrows pan by 1, `W A S D` pan by 8, `space` steps the
//! sim one tick, `p` saves, `q` / `Esc` saves and quits.

use std::io::{self, Write};
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue};
use sim_core::{CHUNK_SIZE, LoadPolicy, Store, World};

use crate::camera::Camera;
use crate::render::ascii::{Viewport, render_into};

/// Rows reserved under the map for status text.
const STATUS_ROWS: u16 = 2;
const FAST_STEP: i32 = 8;

/// Puts the terminal back the way it was, even on panic.
struct RawGuard;

impl RawGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

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
    let _guard = RawGuard::enter().context("raw mode")?;
    let mut out = io::stdout();
    let mut frame = String::new();
    let mut last_stats = None;

    loop {
        let (cols, rows) = terminal::size().context("terminal size")?;
        let view_w = u32::from(cols.max(1));
        let view_h = u32::from(rows.saturating_sub(STATUS_ROWS).max(1));
        let policy = policy_for(view_w, view_h);
        let stats = world
            .ensure_loaded(camera.center, policy, Some(store))
            .context("streaming chunks")?;
        if stats.generated + stats.read + stats.unloaded + stats.written > 0 {
            last_stats = Some(stats);
        }

        let view = Viewport::centered(camera.center, view_w, view_h);
        render_into(&world.stage, view, "\r\n", &mut frame);
        queue!(out, cursor::MoveTo(0, 0))?;
        out.write_all(frame.as_bytes())?;
        let (cc, _) = camera.center.split();
        let status = format!(
            "cam ({}, {}) chunk ({}, {}) | loaded {} chunks | tick {} | last stream: {}",
            camera.center.x,
            camera.center.y,
            cc.x,
            cc.y,
            world.stage.loaded_count(),
            world.tick,
            last_stats.map_or_else(
                || "-".to_string(),
                |s| format!(
                    "gen {} read {} unload {} write {}",
                    s.generated, s.read, s.unloaded, s.written
                )
            ),
        );
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        out.write_all(status.as_bytes())?;
        out.write_all(b"\r\n")?;
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        out.write_all(b"wasd/arrows move, WASD x8, space tick, p save, q quit")?;
        queue!(out, terminal::Clear(ClearType::FromCursorDown))?;
        out.flush()?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let fast = key.modifiers.contains(KeyModifiers::SHIFT);
        let step = if fast { FAST_STEP } else { 1 };
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Char('w' | 'W') | KeyCode::Up => camera.pan(0, -step),
            KeyCode::Char('s' | 'S') | KeyCode::Down => camera.pan(0, step),
            KeyCode::Char('a' | 'A') | KeyCode::Left => camera.pan(-step, 0),
            KeyCode::Char('d' | 'D') | KeyCode::Right => camera.pan(step, 0),
            KeyCode::Char(' ') => world.step(),
            KeyCode::Char('p') => save(world, camera, store)?,
            _ => {}
        }
    }
    save(world, camera, store)?;
    Ok(())
}

fn save(world: &mut World, camera: &Camera, store: &Store) -> anyhow::Result<()> {
    world.save(store).context("saving world")?;
    camera.save(store.dir()).context("saving camera")?;
    Ok(())
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
