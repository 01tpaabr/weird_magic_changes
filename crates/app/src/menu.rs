//! The main menu: what `wmc` with no arguments opens. It lists what is on
//! disk under the working directory and loads, makes or opens it:
//!
//! - **Continue**: the save played last (`saves/*`, by its world file's time).
//! - **Load world**: any save in `saves/`.
//! - **New world**: a scenario (`scenarios/*.scenario`), then a name for its
//!   save directory (`saves/<name>`).
//! - **Scenarios**, **Packs**: the scenario and rules files, each with what
//!   its header comment says and whether it compiles (the lint); Enter opens
//!   it in the user's editor (`WMC_EDITOR`, else the system's text editor).
//!   `r` in a world reloads edited rules.
//! - **Quit**.
//!
//! Behind it the gallery runs (`scenarios/gallery.scenario`, else the
//! built-in world). This module is the menu as data: its pages, what a key
//! does to them ([`Menu::key`], which `play` turns into a world to open),
//! and its text ([`Menu::rows`]). It reads files but never the sim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sim_core::scenario::{Placement, Start};
use sim_core::store::WorldMeta;
use sim_core::{Store, time};

use crate::render::palette::{Color, TEXT_BG, TEXT_FG};

/// Where saves live, under the working directory.
pub const SAVES: &str = "saves";
/// The gallery behind the menu.
pub const GALLERY: &str = "scenarios/gallery.scenario";

const TITLE_FG: Color = Color::rgb(0xf0, 0xc6, 0x74);
const DIM: Color = Color::rgb(0xa0, 0xa0, 0xa8);
const OK: Color = Color::rgb(0x7e, 0xd3, 0x8b);
const BAD: Color = Color::rgb(0xff, 0x7b, 0x72);
const CHOSEN_BG: Color = Color::rgb(0x3a, 0x3a, 0x4c);
/// Header paragraphs shown for a file.
const ABOUT_LINES: usize = 12;
/// The longest save name.
const NAME_MAX: usize = 48;

/// A key the menu understands, already mapped from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    /// Esc: back a page.
    Back,
    /// Backspace: a character off the name, else back a page.
    Erase,
    /// A typed character (`q` quits, from the first page).
    Char(char),
}

/// What the menu asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the world in this save directory.
    Load(PathBuf),
    /// Make a world from this scenario file in this (new) save directory.
    New {
        scenario: PathBuf,
        dir: PathBuf,
    },
    /// Open this file in the user's editor.
    Edit(PathBuf),
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    Main,
    Load,
    New,
    /// The name of a new world's save, for this scenario.
    Name {
        scenario: PathBuf,
        text: String,
    },
    Scenarios,
    Packs,
}

/// A save directory, newest first.
#[derive(Debug, Clone)]
pub struct Save {
    pub dir: PathBuf,
    pub modified: SystemTime,
    pub meta: Option<WorldMeta>,
}

/// A rules file and the pack it belongs to (its directory, or itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesFile {
    pub path: PathBuf,
    pub pack: PathBuf,
}

/// One row of the menu's text: cells and a background.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub cells: Vec<(u8, Color)>,
    pub bg: Color,
}

#[derive(bevy::prelude::Resource, Debug, Clone)]
pub struct Menu {
    root: PathBuf,
    pub page: Page,
    /// The chosen row of the page.
    pub at: usize,
    pub saves: Vec<Save>,
    pub scenarios: Vec<PathBuf>,
    pub rules: Vec<RulesFile>,
    /// A line for the user, until the next key.
    pub note: Option<(String, bool)>,
    /// What each file says and whether it compiles, read when first shown.
    about: BTreeMap<PathBuf, Vec<(String, Color)>>,
}

const MAIN: [&str; 6] = [
    "Continue",
    "Load world",
    "New world",
    "Scenarios",
    "Packs",
    "Quit",
];

impl Menu {
    /// The menu over what is on disk under `root`.
    pub fn scan(root: &Path) -> Menu {
        let mut m = Menu {
            root: root.to_path_buf(),
            page: Page::Main,
            at: 0,
            saves: Vec::new(),
            scenarios: Vec::new(),
            rules: Vec::new(),
            note: None,
            about: BTreeMap::new(),
        };
        m.rescan();
        m
    }

    /// Read the saves, scenarios and rules files again (after a world was
    /// played, or files edited).
    pub fn rescan(&mut self) {
        self.saves = saves(&self.root.join(SAVES));
        self.scenarios = files(&self.root.join("scenarios"), "scenario");
        self.rules = rules_files(&self.root);
        self.about.clear();
        self.at = self.at.min(self.len().saturating_sub(1));
    }

    /// Back to the first page, the lists read again: after a world was played.
    pub fn home(&mut self) {
        self.go(Page::Main);
        self.rescan();
    }

    /// Rows the current page lists.
    fn len(&self) -> usize {
        match &self.page {
            Page::Main => MAIN.len(),
            Page::Load => self.saves.len(),
            Page::New | Page::Scenarios => self.scenarios.len(),
            Page::Name { .. } => 0,
            Page::Packs => self.rules.len(),
        }
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        self.at = 0;
    }

    /// What a key does: moves, changes the page, or asks for an [`Action`].
    pub fn key(&mut self, key: Key) -> Option<Action> {
        self.note = None;
        if let Page::Name { scenario, text } = &mut self.page {
            match key {
                Key::Char(c) if name_char(c) && text.len() < NAME_MAX => text.push(c),
                Key::Char(_) | Key::Up | Key::Down => {}
                Key::Erase if !text.is_empty() => {
                    text.pop();
                }
                Key::Erase | Key::Back => self.go(Page::New),
                Key::Enter => {
                    let dir = self.root.join(SAVES).join(text.as_str());
                    if text.is_empty() || text.starts_with('.') {
                        self.note = Some(("a name of letters, digits, - _ or .".into(), false));
                    } else if dir.exists() {
                        self.note = Some((
                            format!("{} is taken: Load world opens it", shown(&dir)),
                            false,
                        ));
                    } else {
                        return Some(Action::New {
                            scenario: scenario.clone(),
                            dir,
                        });
                    }
                }
            }
            return None;
        }
        let n = self.len();
        match key {
            Key::Up => self.at = self.at.checked_sub(1).unwrap_or(n.saturating_sub(1)),
            Key::Down => self.at = if self.at + 1 >= n { 0 } else { self.at + 1 },
            Key::Back | Key::Erase => self.go(Page::Main),
            Key::Char('q') if self.page == Page::Main => return Some(Action::Quit),
            Key::Char(_) => {}
            Key::Enter => return self.enter(),
        }
        None
    }

    fn enter(&mut self) -> Option<Action> {
        match self.page {
            Page::Main => match MAIN[self.at] {
                "Continue" => match self.saves.first() {
                    Some(s) => return Some(Action::Load(s.dir.clone())),
                    None => self.note = Some(("no saves yet: New world makes one".into(), false)),
                },
                "Load world" => self.go(Page::Load),
                "New world" => self.go(Page::New),
                "Scenarios" => self.go(Page::Scenarios),
                "Packs" => self.go(Page::Packs),
                _ => return Some(Action::Quit),
            },
            Page::Load => return self.saves.get(self.at).map(|s| Action::Load(s.dir.clone())),
            Page::New => {
                if let Some(f) = self.scenarios.get(self.at) {
                    let text = free_name(&self.root.join(SAVES), &stem(f));
                    self.page = Page::Name {
                        scenario: f.clone(),
                        text,
                    };
                }
            }
            Page::Scenarios => return self.scenarios.get(self.at).cloned().map(Action::Edit),
            Page::Packs => {
                return self
                    .rules
                    .get(self.at)
                    .map(|r| Action::Edit(r.path.clone()));
            }
            Page::Name { .. } => {}
        }
        None
    }

    /// The menu's text, `cols x rows` cells: the title, the page's list on
    /// the left and what the chosen row is on the right, the keys.
    pub fn rows(&mut self, cols: usize, rows: usize) -> Vec<Row> {
        let mut out: Vec<Row> = Vec::new();
        let blank = || Row {
            cells: Vec::new(),
            bg: TEXT_BG,
        };
        out.push(blank());
        if cols >= TITLE_WIDTH + 4 {
            for line in TITLE {
                out.push(title_row(line, (cols - TITLE_WIDTH) / 2));
            }
        } else {
            out.push(text_row("  weird magic changes", TITLE_FG, TEXT_BG));
        }
        out.push(text_row(
            &format!(
                "{:^cols$}",
                "an engine and a language for deterministic worlds"
            ),
            DIM,
            TEXT_BG,
        ));
        out.push(blank());
        let (heading, items) = self.items();
        out.push(text_row(&format!("  {heading}"), TITLE_FG, TEXT_BG));
        out.push(blank());
        let left = 34.min(cols / 2);
        let about = self.about_chosen(cols.saturating_sub(left + 4));
        let body = rows.saturating_sub(out.len() + 3);
        // Scroll so the chosen row stays in view.
        let first = self.at.saturating_sub(body.saturating_sub(1));
        for i in 0..body {
            let item = items.get(first + i);
            let chosen = item.is_some() && first + i == self.at;
            let bg = if chosen { CHOSEN_BG } else { TEXT_BG };
            let mut cells = Vec::new();
            let mark = if chosen { "> " } else { "  " };
            push(&mut cells, &format!("  {mark}"), TITLE_FG);
            if let Some((text, fg)) = item {
                push(&mut cells, &fit(text, left.saturating_sub(4)), *fg);
            }
            cells.resize(left, (b' ', TEXT_FG));
            if let Some((text, fg)) = about.get(i) {
                push(&mut cells, "  ", TEXT_FG);
                push(&mut cells, text, *fg);
            }
            out.push(Row { cells, bg });
        }
        out.push(blank());
        match &self.note {
            Some((text, good)) => out.push(text_row(
                &format!("  {text}"),
                if *good { OK } else { BAD },
                TEXT_BG,
            )),
            None => out.push(blank()),
        }
        out.push(text_row(&format!("  {}", self.keys()), DIM, TEXT_BG));
        out.truncate(rows);
        out
    }

    fn keys(&self) -> &'static str {
        match self.page {
            Page::Main => "up/down choose | enter open | q quit",
            Page::Load | Page::New => "up/down choose | enter open | esc back",
            Page::Name { .. } => "type a name | enter make the world | esc back",
            Page::Scenarios | Page::Packs => {
                "up/down choose | enter edit (WMC_EDITOR, else the system's) | esc back"
            }
        }
    }

    /// The page's heading and its rows' text.
    fn items(&self) -> (String, Vec<(String, Color)>) {
        let item = |s: String| (s, TEXT_FG);
        match &self.page {
            Page::Main => (
                "".into(),
                MAIN.iter()
                    .map(|&m| match (m, self.saves.first()) {
                        ("Continue", Some(s)) => item(format!("Continue   {}", name_of(&s.dir))),
                        ("Continue", None) => ("Continue".into(), DIM),
                        _ => item(m.into()),
                    })
                    .collect(),
            ),
            Page::Load => (
                format!("Load world: {} saves in {SAVES}/", self.saves.len()),
                self.saves.iter().map(|s| item(name_of(&s.dir))).collect(),
            ),
            Page::New => (
                "New world from a scenario".into(),
                self.scenarios.iter().map(|f| item(stem(f))).collect(),
            ),
            Page::Scenarios => (
                "Scenarios: enter opens the file".into(),
                self.scenarios.iter().map(|f| item(stem(f))).collect(),
            ),
            Page::Packs => (
                "Packs: their rules files; enter opens one".into(),
                self.rules.iter().map(|r| item(rules_name(r))).collect(),
            ),
            Page::Name { scenario, text } => (
                format!("New world from {}: its name", stem(scenario)),
                vec![(format!("{}/{text}_", SAVES), TITLE_FG)],
            ),
        }
    }

    /// What the chosen row is, in lines at most `width` long.
    fn about_chosen(&mut self, width: usize) -> Vec<(String, Color)> {
        let lines = match &self.page {
            Page::Main => match MAIN[self.at] {
                "Continue" => match self.saves.first() {
                    Some(s) => save_about(s),
                    None => vec![("No saves yet.".into(), DIM)],
                },
                "Load world" => vec![(format!("Open a world saved in {SAVES}/."), DIM)],
                "New world" => vec![
                    ("Make a world from a scenario: its terrain, who".into(), DIM),
                    ("starts where, and the rule packs it runs.".into(), DIM),
                ],
                "Scenarios" => vec![("Read and edit the scenario files.".into(), DIM)],
                "Packs" => vec![("Read and edit the rule packs: the kinds.".into(), DIM)],
                _ => vec![],
            },
            Page::Load => self.saves.get(self.at).map(save_about).unwrap_or_default(),
            Page::New | Page::Scenarios | Page::Name { .. } => {
                let f = match &self.page {
                    Page::Name { scenario, .. } => Some(scenario.clone()),
                    _ => self.scenarios.get(self.at).cloned(),
                };
                match f {
                    Some(f) => self
                        .about
                        .entry(f.clone())
                        .or_insert_with(|| scenario_about(&f))
                        .clone(),
                    None => vec![],
                }
            }
            Page::Packs => match self.rules.get(self.at).cloned() {
                Some(r) => self
                    .about
                    .entry(r.path.clone())
                    .or_insert_with(|| rules_about(&r))
                    .clone(),
                None => vec![],
            },
        };
        lines
            .into_iter()
            .flat_map(|(text, fg)| wrap(&text, width).into_iter().map(move |l| (l, fg)))
            .collect()
    }
}

/// Open `path` in the user's editor: `WMC_EDITOR` (a command, the path
/// appended), else the system's default text editor. Returns the line for
/// the menu.
pub fn edit(path: &Path) -> (String, bool) {
    let mut cmd = match std::env::var("WMC_EDITOR") {
        Ok(e) if !e.trim().is_empty() => {
            let mut words = e.split_whitespace();
            let mut c = std::process::Command::new(words.next().expect("not blank"));
            c.args(words);
            c
        }
        _ if cfg!(target_os = "macos") => {
            let mut c = std::process::Command::new("open");
            c.arg("-t");
            c
        }
        _ if cfg!(target_os = "windows") => {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", ""]);
            c
        }
        _ => std::process::Command::new("xdg-open"),
    };
    match cmd.arg(path).spawn() {
        Ok(_) => (
            format!(
                "opened {} (in a world, r reloads edited rules)",
                shown(path)
            ),
            true,
        ),
        Err(e) => (format!("could not open {}: {e}", shown(path)), false),
    }
}

/// The save directories in `dir` (those with a world file), newest first,
/// then by name.
pub fn saves(dir: &Path) -> Vec<Save> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Save> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter_map(|dir| {
            let modified = std::fs::metadata(dir.join("world.wmc"))
                .and_then(|m| m.modified())
                .ok()?;
            let meta = Store::at(&dir).read_meta().ok().flatten();
            Some(Save {
                dir,
                modified,
                meta,
            })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.dir.cmp(&b.dir)));
    out
}

/// The files in `dir` ending in `.ext`, by name.
fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == ext))
        .collect();
    out.sort();
    out
}

/// The rules files: the built-in kinds' (`rules/`), then each pack's
/// (`packs/<pack>/*.rules`, or `packs/<pack>.rules`), by name.
pub fn rules_files(root: &Path) -> Vec<RulesFile> {
    fn add_dir(out: &mut Vec<RulesFile>, pack: &Path) {
        for path in files(pack, "rules") {
            out.push(RulesFile {
                path,
                pack: pack.to_path_buf(),
            });
        }
    }
    let mut out = Vec::new();
    add_dir(&mut out, &root.join("rules"));
    let Ok(entries) = std::fs::read_dir(root.join("packs")) else {
        return out;
    };
    let mut packs: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    packs.sort();
    for p in packs {
        if p.is_dir() {
            add_dir(&mut out, &p);
        } else if p.extension().is_some_and(|e| e == "rules") {
            out.push(RulesFile {
                path: p.clone(),
                pack: p,
            });
        }
    }
    out
}

/// `base`, else `base-2`, `base-3`, ...: the first name not taken in `saves`.
pub fn free_name(saves: &Path, base: &str) -> String {
    let base: String = base.chars().filter(|&c| name_char(c)).collect();
    let base = if base.is_empty() {
        "world".into()
    } else {
        base
    };
    if !saves.join(&base).exists() {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|n| !saves.join(n).exists())
        .expect("some number is free")
}

/// A rules file as `pack/file.rules` (a one-file pack: its name).
fn rules_name(r: &RulesFile) -> String {
    if r.pack == r.path {
        return name_of(&r.path);
    }
    format!("{}/{}", name_of(&r.pack), name_of(&r.path))
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn stem(f: &Path) -> String {
    f.file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}

fn name_of(dir: &Path) -> String {
    dir.file_name()
        .map_or_else(|| shown(dir), |s| s.to_string_lossy().into_owned())
}

/// A path as the user would type it: relative to the working directory
/// when it is under it.
fn shown(p: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| p.to_path_buf())
        .display()
        .to_string()
}

fn save_about(s: &Save) -> Vec<(String, Color)> {
    let mut out = vec![(shown(&s.dir), TEXT_FG)];
    match &s.meta {
        Some(m) => {
            out.push((format!("{}", time::Clock::at(m.tick)), TEXT_FG));
            out.push((
                format!(
                    "seed {}, first {}x{} cells",
                    m.seed, m.initial_width, m.initial_height
                ),
                DIM,
            ));
            let packs = if m.packs.is_empty() {
                "the built-in rules".to_string()
            } else {
                m.packs
                    .iter()
                    .map(|p| name_of(Path::new(p)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out.push((format!("rules: {packs}"), DIM));
            let kinds: Vec<&str> = m.kinds.iter().map(|k| k.name.as_str()).collect();
            out.push((format!("{} kinds: {}", kinds.len(), kinds.join(" ")), DIM));
        }
        None => out.push(("its world file does not read".into(), BAD)),
    }
    out
}

/// The `#` lines a file starts with, without the `#`, as paragraphs: lines
/// run together until a blank one, which stays; an indented line (a
/// command) stands alone.
fn header(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut para = String::new();
    for line in text.lines().take_while(|l| l.trim_start().starts_with('#')) {
        let line = line.trim_start().trim_start_matches('#').trim_end();
        let line = line.strip_prefix(' ').unwrap_or(line);
        if line.is_empty() || line.starts_with(' ') {
            if !para.is_empty() {
                out.push(std::mem::take(&mut para));
            }
            out.push(line.to_string());
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(line);
        }
    }
    if !para.is_empty() {
        out.push(para);
    }
    out.truncate(ABOUT_LINES);
    out
}

/// A scenario's header, and whether it makes a world: its packs compile and
/// its starts fit them.
fn scenario_about(f: &Path) -> Vec<(String, Color)> {
    let text = std::fs::read_to_string(f).unwrap_or_default();
    let mut out: Vec<(String, Color)> = header(&text).into_iter().map(|l| (l, DIM)).collect();
    out.push((String::new(), DIM));
    let lint = (|| -> anyhow::Result<String> {
        let s = crate::read_scenario(&f.to_string_lossy())?;
        let packs: Vec<PathBuf> = s.packs.iter().map(PathBuf::from).collect();
        let kinds = crate::compile(&packs)?;
        Placement::resolve(&s.starts, &kinds, &s.terrain())
            .map_err(|e| anyhow::anyhow!("line {}: {e}", s.line_of(&e).unwrap_or(0)))?;
        let shares = s
            .starts
            .iter()
            .filter(|st| matches!(st, Start::Share { .. }))
            .count();
        Ok(format!(
            "ok: {} kinds, seed {}, {}x{}, {shares} shares, {} starts{}",
            kinds.len(),
            s.seed,
            s.width,
            s.height,
            s.starts.len() - shares,
            if s.checks.is_empty() { "" } else { ", a test" }
        ))
    })();
    out.push(match lint {
        Ok(t) => (t, OK),
        Err(e) => (format!("{e:#}"), BAD),
    });
    out
}

/// A rules file's header, and whether its pack compiles.
fn rules_about(r: &RulesFile) -> Vec<(String, Color)> {
    let text = std::fs::read_to_string(&r.path).unwrap_or_default();
    let mut out: Vec<(String, Color)> = header(&text).into_iter().map(|l| (l, DIM)).collect();
    out.push((String::new(), DIM));
    out.push(match crate::compile(std::slice::from_ref(&r.pack)) {
        Ok(k) => {
            let warnings = k
                .debug
                .diagnostics
                .iter()
                .filter(|d| d.level == sim_core::rules::Level::Warning)
                .count();
            (
                format!(
                    "pack {}: {} kinds, {warnings} warnings",
                    name_of(&r.pack),
                    k.len()
                ),
                if warnings == 0 { OK } else { TITLE_FG },
            )
        }
        Err(e) => (format!("{e:#}"), BAD),
    });
    out
}

/// `text` in lines of at most `width` characters, broken between words
/// where it can be (a blank line stays one).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    // The indent of a line (a command in a comment) starts each of its lines.
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    for word in text.split_whitespace() {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if line.is_empty() {
            line.push_str(&indent);
        } else {
            line.push(' ');
        }
        line.push_str(word);
        while line.chars().count() > width {
            out.push(line.chars().take(width).collect());
            line = line.chars().skip(width).collect();
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// `text`, cut to `width` characters.
fn fit(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

fn push(cells: &mut Vec<(u8, Color)>, text: &str, fg: Color) {
    cells.extend(
        text.chars()
            .map(|c| (if c.is_ascii_graphic() { c as u8 } else { b' ' }, fg)),
    );
}

fn text_row(text: &str, fg: Color, bg: Color) -> Row {
    let mut cells = Vec::new();
    push(&mut cells, text, fg);
    Row { cells, bg }
}

/// The title, a column of colour each: the hues of `packs/cyclic`.
fn title_row(line: &str, indent: usize) -> Row {
    let mut cells = vec![(b' ', TEXT_FG); indent];
    let n = line.len().max(1);
    for (i, c) in line.bytes().enumerate() {
        cells.push((if c == b'#' { b'#' } else { b' ' }, hue(i * 12 / n)));
    }
    Row { cells, bg: TEXT_BG }
}

fn hue(i: usize) -> Color {
    const HUES: [Color; 12] = [
        Color::rgb(0xf2, 0x3c, 0x3c),
        Color::rgb(0xf2, 0x80, 0x3c),
        Color::rgb(0xf2, 0xc4, 0x3c),
        Color::rgb(0xdb, 0xf2, 0x3c),
        Color::rgb(0x97, 0xf2, 0x3c),
        Color::rgb(0x3c, 0xf2, 0x69),
        Color::rgb(0x3c, 0xf2, 0xae),
        Color::rgb(0x3c, 0xf2, 0xf2),
        Color::rgb(0x3c, 0xae, 0xf2),
        Color::rgb(0x3c, 0x69, 0xf2),
        Color::rgb(0x97, 0x3c, 0xf2),
        Color::rgb(0xdb, 0x3c, 0xf2),
    ];
    HUES[i.min(HUES.len() - 1)]
}

const TITLE_WIDTH: usize = 89;
const TITLE: [&str; 5] = [
    "#   # #### ### ###  ###    #   #  ##   ### ###  ###    ### #  #  ##  #   #  ### ####  ###",
    "#   # #     #  #  # #  #   ## ## #  # #     #  #      #    #  # #  # ##  # #    #    #   ",
    "# # # ###   #  ###  #  #   # # # #### # ##  #  #      #    #### #### # # # # ## ###   ## ",
    "## ## #     #  # #  #  #   #   # #  # #  #  #  #      #    #  # #  # #  ## #  # #       #",
    "#   # #### ### #  # ###    #   # #  #  ### ###  ###    ### #  # #  # #   #  ### #### ### ",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory with two saves, two scenarios and a pack, under the
    /// target dir (removed first).
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wmc-menu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "saves/old/chunks",
            "saves/new/chunks",
            "scenarios",
            "packs/p",
            "rules",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("saves/old/world.wmc"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("saves/new/world.wmc"), b"x").unwrap();
        std::fs::write(root.join("scenarios/b.scenario"), "# B\nseed 2\n").unwrap();
        std::fs::write(
            root.join("scenarios/a.scenario"),
            "# A\n# is first\nseed 1\n",
        )
        .unwrap();
        std::fs::write(root.join("scenarios/notes.txt"), "no").unwrap();
        std::fs::write(root.join("packs/p/k.rules"), "# a kind\nkind k { }\n").unwrap();
        std::fs::write(root.join("rules/x.rules"), "kind x { }\n").unwrap();
        root
    }

    #[test]
    fn it_finds_saves_newest_first_and_files_by_name() {
        let root = tree("scan");
        let m = Menu::scan(&root);
        let names: Vec<String> = m.saves.iter().map(|s| name_of(&s.dir)).collect();
        assert_eq!(names, ["new", "old"]);
        assert_eq!(
            m.scenarios,
            [
                root.join("scenarios/a.scenario"),
                root.join("scenarios/b.scenario")
            ]
        );
        let rules: Vec<(PathBuf, PathBuf)> = m
            .rules
            .iter()
            .map(|r| (r.path.clone(), r.pack.clone()))
            .collect();
        assert_eq!(
            rules,
            [
                (root.join("rules/x.rules"), root.join("rules")),
                (root.join("packs/p/k.rules"), root.join("packs/p")),
            ]
        );
        assert_eq!(header("# A\n#  two\nseed 1\n# not"), ["A", " two"]);
        assert_eq!(
            header("# one\n# two\n#\n#   cmd\n# three"),
            ["one two", "", "  cmd", "three"]
        );
    }

    #[test]
    fn keys_walk_the_pages_and_ask_for_worlds() {
        let root = tree("keys");
        let mut m = Menu::scan(&root);
        assert_eq!(
            m.key(Key::Enter),
            Some(Action::Load(root.join("saves/new")))
        );
        m.key(Key::Up);
        assert_eq!(MAIN[m.at], "Quit", "up from the top wraps");
        m.key(Key::Down);
        m.key(Key::Down);
        m.key(Key::Enter);
        assert_eq!(m.page, Page::Load);
        m.key(Key::Down);
        assert_eq!(
            m.key(Key::Enter),
            Some(Action::Load(root.join("saves/old")))
        );
        m.key(Key::Back);
        m.key(Key::Down);
        m.key(Key::Down);
        m.key(Key::Enter);
        assert_eq!(m.page, Page::New);
        m.key(Key::Enter);
        assert_eq!(
            m.page,
            Page::Name {
                scenario: root.join("scenarios/a.scenario"),
                text: "a".into()
            }
        );
        for c in "-1 /x".chars() {
            m.key(Key::Char(c));
        }
        m.key(Key::Erase);
        assert_eq!(
            m.key(Key::Enter),
            Some(Action::New {
                scenario: root.join("scenarios/a.scenario"),
                dir: root.join("saves/a-1"),
            }),
            "only name characters are typed"
        );
        // A name that is taken is not made over.
        for _ in 0..3 {
            m.key(Key::Erase);
        }
        for c in "old".chars() {
            m.key(Key::Char(c));
        }
        assert_eq!(m.key(Key::Enter), None);
        assert!(m.note.as_ref().unwrap().0.contains("is taken"));
        m.key(Key::Back);
        assert_eq!(m.page, Page::New);
        m.key(Key::Back);
        assert_eq!(m.key(Key::Char('q')), Some(Action::Quit));
    }

    #[test]
    fn the_editing_pages_open_files_and_say_what_they_hold() {
        let root = tree("edit");
        let mut m = Menu::scan(&root);
        m.go(Page::Packs);
        m.key(Key::Down);
        assert_eq!(
            m.key(Key::Enter),
            Some(Action::Edit(root.join("packs/p/k.rules")))
        );
        let text: Vec<String> = m
            .rows(100, 40)
            .iter()
            .map(|r| r.cells.iter().map(|c| char::from(c.0)).collect())
            .collect();
        assert!(text.iter().any(|l| l.contains("> p/k.rules")));
        assert!(text.iter().any(|l| l.contains("a kind")), "its header");
        assert!(
            text.iter()
                .any(|l| l.contains("pack p: 1 kinds, 0 warnings"))
        );
        m.go(Page::Scenarios);
        assert_eq!(
            m.key(Key::Enter),
            Some(Action::Edit(root.join("scenarios/a.scenario")))
        );
        let text: Vec<String> = m
            .rows(100, 40)
            .iter()
            .map(|r| r.cells.iter().map(|c| char::from(c.0)).collect())
            .collect();
        assert!(text.iter().any(|l| l.contains("is first")));
        assert!(
            text.iter()
                .any(|l| l.contains("ok: ") && l.contains("seed 1"))
        );
    }

    #[test]
    fn about_lines_wrap_between_words() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("", 9), [""]);
        assert_eq!(
            wrap("  make run ARGS", 8),
            ["  make", "  run", "  ARGS"],
            "indent kept"
        );
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn a_free_name_counts_up() {
        let root = tree("names");
        let saves = root.join(SAVES);
        assert_eq!(free_name(&saves, "fresh"), "fresh");
        assert_eq!(free_name(&saves, "old"), "old-2");
        assert_eq!(free_name(&saves, "a b/c"), "abc");
        assert_eq!(free_name(&saves, "//"), "world");
    }

    #[test]
    fn the_title_rows_are_one_width() {
        for line in TITLE {
            assert_eq!(line.len(), TITLE_WIDTH, "{line:?}");
        }
    }
}
