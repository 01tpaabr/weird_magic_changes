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
//!   `r` in a world reloads edited rules. Their first row makes a new one
//!   from a template ([`scenario_template`], [`pack_template`]): a world that
//!   runs, every line explained, to change.
//! - **Quit**.
//!
//! The keyboard or the mouse: pointing at a row chooses it, a click opens
//! it, the wheel moves the choice, a right click goes back.
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

/// What a new file from a template is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Make {
    Scenario,
    Pack,
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
    /// What to copy: a scenario, or a pack.
    Pick {
        what: Make,
    },
    /// The name of a new scenario or pack: from its template, or a copy of
    /// `from`.
    Make {
        what: Make,
        text: String,
        from: Option<PathBuf>,
    },
}

/// Rows before the files on the Scenarios and Packs pages: `+ New`, `+ Copy`.
const FIRST: usize = 2;

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
    /// The list's first row in view: it follows the chosen row, not the
    /// pointer.
    top: usize,
    /// For each row of the text last drawn, the list item it shows: where
    /// the pointer is.
    shown: Vec<Option<usize>>,
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
            top: 0,
            shown: Vec::new(),
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

    /// The packs the rules files belong to, in their order, each once.
    fn packs(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for r in &self.rules {
            if out.last() != Some(&r.pack) {
                out.push(r.pack.clone());
            }
        }
        out
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
            Page::New => self.scenarios.len(),
            // The first row makes a new one.
            Page::Scenarios => self.scenarios.len() + FIRST,
            Page::Packs => self.rules.len() + FIRST,
            Page::Pick {
                what: Make::Scenario,
            } => self.scenarios.len(),
            Page::Pick { what: Make::Pack } => self.packs().len(),
            Page::Name { .. } | Page::Make { .. } => 0,
        }
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        self.at = 0;
        self.top = 0;
    }

    /// The list item the row `row` of the text last drawn shows, if any.
    pub fn item_at(&self, row: usize) -> Option<usize> {
        self.shown.get(row).copied().flatten()
    }

    /// The pointer moved onto list item `i`: it is the chosen one.
    pub fn hover(&mut self, i: usize) {
        if i < self.len() {
            self.at = i;
        }
    }

    /// A click on list item `i`: chosen, then opened, as Enter.
    pub fn click(&mut self, i: usize) -> Option<Action> {
        // The name pages list one row: the name.
        let naming = matches!(self.page, Page::Name { .. } | Page::Make { .. });
        if (naming && i == 0) || i < self.len() {
            self.hover(i);
            return self.key(Key::Enter);
        }
        None
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
        if let Page::Make { what, text, from } = &mut self.page {
            let (what, from) = (*what, from.clone());
            let back = match what {
                Make::Scenario => Page::Scenarios,
                Make::Pack => Page::Packs,
            };
            match key {
                Key::Char(c) if ident_char(c, text.is_empty()) && text.len() < NAME_MAX => {
                    text.push(c);
                }
                Key::Char(_) | Key::Up | Key::Down => {}
                Key::Erase if !text.is_empty() => {
                    text.pop();
                }
                Key::Erase | Key::Back => self.go(back),
                Key::Enter => {
                    let name = text.clone();
                    let made = match &from {
                        Some(from) => make_copy(&self.root, what, from, &name),
                        None => make(&self.root, what, &name),
                    };
                    match made {
                        Ok(open) => {
                            self.go(back);
                            self.rescan();
                            self.at = match what {
                                Make::Scenario => self.scenarios.iter().position(|f| *f == open),
                                Make::Pack => self.rules.iter().position(|r| r.path == open),
                            }
                            .map_or(0, |i| i + FIRST);
                            self.note = Some((format!("made {}", shown(&open)), true));
                            return Some(Action::Edit(open));
                        }
                        Err(e) => self.note = Some((e, false)),
                    }
                }
            }
            return None;
        }
        let n = self.len();
        match key {
            Key::Up => self.at = self.at.checked_sub(1).unwrap_or(n.saturating_sub(1)),
            Key::Down => self.at = if self.at + 1 >= n { 0 } else { self.at + 1 },
            Key::Back | Key::Erase => match self.page {
                Page::Pick {
                    what: Make::Scenario,
                } => self.go(Page::Scenarios),
                Page::Pick { what: Make::Pack } => self.go(Page::Packs),
                _ => self.go(Page::Main),
            },
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
            Page::Scenarios | Page::Packs if self.at < FIRST => {
                let what = if self.page == Page::Scenarios {
                    Make::Scenario
                } else {
                    Make::Pack
                };
                if self.at == 0 {
                    self.page = Page::Make {
                        what,
                        text: String::new(),
                        from: None,
                    };
                } else {
                    self.go(Page::Pick { what });
                }
            }
            Page::Scenarios => {
                return self
                    .scenarios
                    .get(self.at - FIRST)
                    .cloned()
                    .map(Action::Edit);
            }
            Page::Packs => {
                return self
                    .rules
                    .get(self.at - FIRST)
                    .map(|r| Action::Edit(r.path.clone()));
            }
            Page::Pick { what } => {
                let from = match what {
                    Make::Scenario => self.scenarios.get(self.at).cloned(),
                    Make::Pack => self.packs().get(self.at).cloned(),
                };
                if let Some(from) = from {
                    let text = copy_name(&self.root, what, &from);
                    self.go(Page::Make {
                        what,
                        text,
                        from: Some(from),
                    });
                }
            }
            Page::Name { .. } | Page::Make { .. } => {}
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
            &format!("{:^cols$}", "an engine and a language for emergent worlds"),
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
        // Scroll only as far as keeps the chosen row in view.
        if self.at < self.top {
            self.top = self.at;
        } else if body > 0 && self.at >= self.top + body {
            self.top = self.at + 1 - body;
        }
        let first = self.top;
        self.shown = vec![None; out.len()];
        for i in 0..body {
            self.shown.push(items.get(first + i).map(|_| first + i));
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
            Page::Main => "up/down or point: choose | enter or click: open | q quit",
            Page::Load | Page::New => {
                "up/down, wheel or point: choose | enter or click: open | esc or right click: back"
            }
            Page::Name { .. } => "type a name | enter or click: make the world | esc: back",
            Page::Make { .. } => "type a name | enter or click: make it and open it | esc: back",
            Page::Pick { .. } => "choose what to copy | enter or click: name the copy | esc: back",
            Page::Scenarios | Page::Packs => {
                "choose | enter or click: edit (WMC_EDITOR, else the system's) | esc: back"
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
                ["+ New scenario", "+ Copy of a scenario"]
                    .into_iter()
                    .map(|t| (t.to_string(), OK))
                    .chain(self.scenarios.iter().map(|f| item(stem(f))))
                    .collect(),
            ),
            Page::Packs => (
                "Packs: their rules files; enter opens one".into(),
                ["+ New pack", "+ Copy of a pack"]
                    .into_iter()
                    .map(|t| (t.to_string(), OK))
                    .chain(self.rules.iter().map(|r| item(rules_name(r))))
                    .collect(),
            ),
            Page::Pick {
                what: Make::Scenario,
            } => (
                "Copy of a scenario: which one".into(),
                self.scenarios.iter().map(|f| item(stem(f))).collect(),
            ),
            Page::Pick { what: Make::Pack } => (
                "Copy of a pack: which one".into(),
                self.packs().iter().map(|p| item(name_of(p))).collect(),
            ),
            Page::Make { what, text, from } => (
                match (what, from) {
                    (Make::Scenario, None) => "New scenario: its name".to_string(),
                    (Make::Pack, None) => "New pack: its name".to_string(),
                    (_, Some(f)) => format!("Copy of {}: its name", name_of(f)),
                },
                vec![(
                    match what {
                        Make::Scenario => format!("scenarios/{text}_"),
                        Make::Pack => format!("packs/{text}_"),
                    },
                    TITLE_FG,
                )],
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
            Page::Scenarios | Page::Packs if self.at < FIRST => {
                let text = match (&self.page, self.at) {
                    (Page::Scenarios, 0) => {
                        "A new scenario from a template: a world that runs, every line \
                         explained, to change."
                    }
                    (Page::Scenarios, _) => {
                        "A copy of a scenario, to change: the same world under another name."
                    }
                    (_, 0) => {
                        "A new pack from a template: two kinds that run, every line \
                         explained, and a scenario to try them in."
                    }
                    _ => {
                        "A copy of a pack, to change, and a scenario to try it. Its names \
                         are the original's: a world runs one or the other."
                    }
                };
                vec![(text.into(), DIM)]
            }
            Page::Pick { what: Make::Pack } => {
                let first = self
                    .packs()
                    .get(self.at)
                    .and_then(|p| self.rules.iter().find(|r| r.pack == *p).cloned());
                match first {
                    Some(r) => self
                        .about
                        .entry(r.path.clone())
                        .or_insert_with(|| rules_about(&r))
                        .clone(),
                    None => vec![],
                }
            }
            Page::Make { what, from, .. } => vec![(
                match (what, from) {
                    (Make::Scenario, _) => "Lowercase letters, digits and _: the file is \
                                            scenarios/<name>.scenario."
                        .to_string(),
                    (Make::Pack, None) => "Lowercase letters, digits and _: the pack is \
                                           packs/<name>/, its kinds <name>_walker and \
                                           <name>_moss, and scenarios/<name>.scenario tries it."
                        .to_string(),
                    (Make::Pack, Some(f)) => format!(
                        "Lowercase letters, digits and _: the copy is packs/<name>/, and \
                         scenarios/<name>.scenario tries it. Its names are {}'s.",
                        name_of(f)
                    ),
                },
                DIM,
            )],
            Page::New | Page::Scenarios | Page::Name { .. } | Page::Pick { .. } => {
                let f = match &self.page {
                    Page::Name { scenario, .. } => Some(scenario.clone()),
                    Page::Scenarios => self.scenarios.get(self.at - FIRST).cloned(),
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
            Page::Packs => match self.rules.get(self.at - FIRST).cloned() {
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

/// A character of a new scenario's or pack's name: it names kinds and
/// constants too, so an identifier, lowercase.
fn ident_char(c: char, first: bool) -> bool {
    c.is_ascii_lowercase() || (!first && (c.is_ascii_digit() || c == '_'))
}

/// Make a scenario or a pack (and a scenario to try it) named `name` from its
/// template under `root`; the file to open, or why not.
pub fn make(root: &Path, what: Make, name: &str) -> Result<PathBuf, String> {
    check_name(name)?;
    let scenario = root.join("scenarios").join(format!("{name}.scenario"));
    let (open, files) = match what {
        Make::Scenario => (scenario.clone(), vec![(scenario, scenario_template(name))]),
        Make::Pack => {
            let rules = root.join("packs").join(name).join(format!("{name}.rules"));
            let (pack, try_it) = pack_template(name);
            (rules.clone(), vec![(rules, pack), (scenario, try_it)])
        }
    };
    if let Some((taken, _)) = files.iter().find(|(f, _)| f.exists()) {
        return Err(format!("{} is taken", shown(taken)));
    }
    if what == Make::Pack && root.join("packs").join(name).exists() {
        return Err(format!("packs/{name} is taken"));
    }
    for (f, text) in files {
        let dir = f.parent().expect("a file in a directory");
        std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&f, text))
            .map_err(|e| format!("could not write {}: {e}", shown(&f)))?;
    }
    Ok(open)
}

/// A lowercase identifier, else why it is not one.
fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| ident_char(c, true)) || !chars.all(|c| ident_char(c, false)) {
        return Err("a name of lowercase letters, digits and _, from a letter".into());
    }
    Ok(())
}

/// The name a copy of `from` is offered: its own as an identifier, then
/// `_copy`, then a number until it is free.
pub fn copy_name(root: &Path, what: Make, from: &Path) -> String {
    let base: String = stem(from)
        .to_lowercase()
        .chars()
        .map(|c| if ident_char(c, false) { c } else { '_' })
        .collect();
    let base = if base.starts_with(|c: char| c.is_ascii_lowercase()) {
        format!("{base}_copy")
    } else {
        format!("copy_{base}")
    };
    let taken = |n: &str| {
        root.join("scenarios")
            .join(format!("{n}.scenario"))
            .exists()
            || (what == Make::Pack && root.join("packs").join(n).exists())
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base}{i}"))
        .find(|n| !taken(n))
        .expect("some number is free")
}

/// Copy the scenario or pack `from` under `root` as `name`; the file to
/// open, or why not. A scenario is copied as it is (its packs, relative to
/// `scenarios/`, still found). A pack's rules files go to
/// `packs/<name>/`, its names unchanged (a world runs it or the original,
/// not both), with `scenarios/<name>.scenario` to try it: a copy of the
/// scenario that plays the original pack alone, if one does, else a bare one.
pub fn make_copy(root: &Path, what: Make, from: &Path, name: &str) -> Result<PathBuf, String> {
    check_name(name)?;
    let scenario = root.join("scenarios").join(format!("{name}.scenario"));
    let read = |f: &Path| {
        std::fs::read_to_string(f).map_err(|e| format!("could not read {}: {e}", shown(f)))
    };
    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    match what {
        Make::Scenario => {
            let text = format!("# Copied from {}.\n{}", shown(from), read(from)?);
            writes.push((scenario.clone(), text));
        }
        Make::Pack => {
            let dir = root.join("packs").join(name);
            if dir.exists() {
                return Err(format!("{} is taken", shown(&dir)));
            }
            let note = format!(
                "# Copied from {}. Its kinds, traits, subs and constants have the \
                 original's names,\n# so a world runs this pack or that one, not both.\n",
                shown(from)
            );
            let sources = if from.is_dir() {
                files(from, "rules")
            } else {
                vec![from.to_path_buf()]
            };
            for f in &sources {
                let to = if from.is_dir() {
                    dir.join(f.file_name().expect("a file"))
                } else {
                    dir.join(format!("{name}.rules"))
                };
                writes.push((to, format!("{note}{}", read(f)?)));
            }
            if writes.is_empty() {
                return Err(format!("{} holds no rules files", shown(from)));
            }
            writes.push((scenario.clone(), try_pack(root, from, name)));
        }
    }
    if let Some((taken, _)) = writes.iter().find(|(f, _)| f.exists()) {
        return Err(format!("{} is taken", shown(taken)));
    }
    let open = writes[0].0.clone();
    for (f, text) in writes {
        let dir = f.parent().expect("a file in a directory");
        std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&f, text))
            .map_err(|e| format!("could not write {}: {e}", shown(&f)))?;
    }
    Ok(open)
}

/// A scenario for a copy of the pack `from` named `name`: the one under
/// `root/scenarios` that plays `from` alone (the built-in rules: one that
/// names no packs), its `rules` lines now the copy's; else a bare world.
fn try_pack(root: &Path, from: &Path, name: &str) -> String {
    let same = |a: &Path, b: &Path| {
        a.canonicalize()
            .ok()
            .is_some_and(|a| Some(a) == b.canonicalize().ok())
    };
    let builtin = same(from, &root.join("rules"));
    let plays = |f: &Path| {
        crate::read_scenario(&f.to_string_lossy())
            .ok()
            .is_some_and(|s| match &s.packs[..] {
                [] => builtin,
                [p] => same(Path::new(p), from),
                _ => false,
            })
    };
    let rules = format!("rules ../packs/{name}");
    match files(&root.join("scenarios"), "scenario")
        .into_iter()
        .find(|f| plays(f))
    {
        Some(f) => {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let mut out = format!("# Copied from {}, to try packs/{name}.\n", shown(&f));
            let mut placed = false;
            for line in text.lines() {
                let t = line.trim_start();
                if t == "rules" || t.starts_with("rules ") {
                    continue;
                }
                if !placed && !t.is_empty() && !t.starts_with('#') {
                    out.push_str(&rules);
                    out.push('\n');
                    placed = true;
                }
                out.push_str(line);
                out.push('\n');
            }
            if !placed {
                out.push_str(&rules);
                out.push('\n');
            }
            out
        }
        None => format!(
            "# A world of packs/{name}, a copy of {}: add `start` lines for its kinds\n\
             # (docs/RULES.md, section 14).\n{rules}\nseed 1\nsize 128 96\n",
            shown(from)
        ),
    }
}

/// A new scenario: the built-in kinds on a noise terrain, every line
/// explained.
pub fn scenario_template(name: &str) -> String {
    format!(
        r##"# {name}: a new world. Every line is in docs/RULES.md, section 14 (scenarios).
# Change it and save; the menu's New world makes a world of it, Scenarios
# says whether it fits its rules.
#
# The rule packs the world runs, relative to this file. None: the built-in
# kinds (chickens, foxes, bees, grass, flowers, trees). Several add up:
# rules ../packs/life
seed 1                          # the dice: terrain, who starts where, every think
size 128 96                     # cells made at the start; the rest as the camera comes
terrain water_scale 12 water_level 0.25 rock_on_soil 0.03 rock_on_water 0.01

# Who starts where: a share of the walkable cells (everywhere, or `in (x0, y0)
# to (x1, y1)`), or one at a cell: `start fox at (10, 20) with (food = 6h)`.
start grass   1 / 6
start seed    1 / 80
start flower  1 / 200
start chicken 1 / 300
start fox     1 / 3000

# A test, if you like: step, then check (`wmc scenario scenarios/{name}.scenario`).
# run 1d
# expect count chicken > 0
"##
    )
}

/// A new pack, `packs/<name>/<name>.rules`, and a scenario to try it. Every
/// name in a world's rules is global, so its kinds and constant carry the
/// pack's name.
pub fn pack_template(name: &str) -> (String, String) {
    let upper = name.to_uppercase();
    let rules = format!(
        r##"# {name}: a new pack. Kinds, rules, senses and actions are in docs/RULES.md;
# `wmc lint packs/{name}` checks it, scenarios/{name}.scenario plays it, and in
# a world `r` reloads it as you change it. Every name is global across the
# packs a world runs, so this pack's carry its name.

const {upper}_SPREAD = 4        # chances in 100 that moss spreads, each of its thinks

# A walker: wanders, and leaves moss where it stands on bare ground.
kind {name}_walker {{
  glyph "@"
  color "#ffd166"
  cadence 4                     # thinks every 4 ticks
  sight 1                       # sees the cells around it

  when is(here, bare) => spawn {name}_moss at here
  when true => move random free
}}

# Moss: ground cover, a whole cell of green, that creeps onto bare ground.
kind {name}_moss {{
  glyph ","
  color "#5c9a3c"
  cover                         # lies under whoever stands on it
  solid                         # drawn as a whole cell of its colour
  cadence 64
  sight 1

  when chance({upper}_SPREAD) and nearest bare within 1 as b => spawn {name}_moss at b
}}
"##
    );
    let scenario = format!(
        r##"# {name}: a world of the pack in packs/{name}. New world in the menu makes it.
rules ../packs/{name}
seed 1
size 128 96
terrain water_scale 12 water_level 0.2 rock_on_soil 0.03 rock_on_water 0
start {name}_walker 1 / 400
"##
    );
    (rules, scenario)
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
    use sim_core::Scenario;

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
        for _ in 0..3 {
            m.key(Key::Down); // past "+ New pack", "+ Copy of a pack", rules/x.rules
        }
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
        m.key(Key::Down); // past "+ New scenario" and "+ Copy of a scenario"
        m.key(Key::Down);
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

    /// The rows drawn say which item each shows; a move over one chooses it,
    /// a click opens it; a long list scrolls with the choice, not the pointer.
    #[test]
    fn the_pointer_chooses_and_clicks_open() {
        let root = tree("mouse");
        for i in 0..30 {
            std::fs::write(root.join(format!("scenarios/s{i:02}.scenario")), "seed 1\n").unwrap();
        }
        let mut m = Menu::scan(&root);
        let text = |m: &mut Menu| -> Vec<String> {
            m.rows(100, 24)
                .iter()
                .map(|r| r.cells.iter().map(|c| char::from(c.0)).collect())
                .collect()
        };
        let lines = text(&mut m);
        let row_of =
            |lines: &[String], what: &str| lines.iter().position(|l| l.contains(what)).unwrap();
        let load = row_of(&lines, "Load world");
        assert_eq!(m.item_at(load), Some(1));
        assert_eq!(m.item_at(0), None, "the title is no item");
        m.hover(1);
        assert_eq!(m.at, 1);
        assert_eq!(m.click(2), None);
        assert_eq!(m.page, Page::New, "New world opened");
        // 32 scenarios, 15 rows of list: down to the 20th, then the pointer
        // on the first row in view chooses it without scrolling.
        let lines = text(&mut m);
        let first = (0..lines.len()).find(|&r| m.item_at(r).is_some()).unwrap();
        for _ in 0..20 {
            m.key(Key::Down);
        }
        text(&mut m);
        let top = m.item_at(first).unwrap();
        assert!(top > 0, "scrolled");
        m.hover(top);
        text(&mut m);
        assert_eq!(m.item_at(first), Some(top), "the list stays put");
        assert_eq!(m.click(top), None);
        assert!(matches!(m.page, Page::Name { .. }));
        assert!(m.click(0).is_some(), "the name's row makes the world");
        assert_eq!(m.click(999), None);
    }

    /// `+ New scenario` and `+ New pack`: a name that can name kinds, never
    /// over a file; the new file is listed, chosen and opened.
    #[test]
    fn new_files_come_from_templates() {
        let root = tree("make");
        let mut m = Menu::scan(&root);
        m.go(Page::Scenarios);
        assert_eq!(m.key(Key::Enter), None);
        assert_eq!(
            m.page,
            Page::Make {
                what: Make::Scenario,
                text: String::new(),
                from: None
            }
        );
        for c in "9My-new_1".chars() {
            m.key(Key::Char(c));
        }
        assert_eq!(
            m.page,
            Page::Make {
                what: Make::Scenario,
                text: "ynew_1".into(),
                from: None
            },
            "lowercase, digits and _, from a letter"
        );
        let made = root.join("scenarios/ynew_1.scenario");
        assert_eq!(m.key(Key::Enter), Some(Action::Edit(made.clone())));
        assert!(made.is_file());
        assert_eq!(m.page, Page::Scenarios);
        assert_eq!(
            m.scenarios.get(m.at - FIRST),
            Some(&made),
            "listed and chosen"
        );
        m.go(Page::Scenarios);
        m.key(Key::Enter);
        for c in "a".chars() {
            m.key(Key::Char(c));
        }
        assert_eq!(m.key(Key::Enter), None, "a.scenario is there already");
        assert!(m.note.as_ref().unwrap().0.contains("is taken"));
        m.key(Key::Back);
        m.go(Page::Packs);
        m.key(Key::Enter);
        for c in "fern".chars() {
            m.key(Key::Char(c));
        }
        let rules = root.join("packs/fern/fern.rules");
        assert_eq!(m.key(Key::Enter), Some(Action::Edit(rules.clone())));
        assert!(rules.is_file() && root.join("scenarios/fern.scenario").is_file());
        assert_eq!(m.rules.get(m.at - FIRST).map(|r| &r.path), Some(&rules));
        assert!(make(&root, Make::Pack, "fern").is_err(), "taken");
        assert!(make(&root, Make::Pack, "").is_err());
    }

    /// Both templates make worlds that run: their packs compile, their
    /// starts fit, nothing traps, and the pack's moss spreads.
    #[test]
    fn the_templates_make_worlds_that_run() {
        let root = tree("templates");
        let runs = |f: &Path, extra: &str| {
            let text = std::fs::read_to_string(f).unwrap() + extra;
            std::fs::write(f, text).unwrap();
            let s = crate::read_scenario(&f.to_string_lossy()).unwrap();
            let packs: Vec<PathBuf> = s.packs.iter().map(PathBuf::from).collect();
            let kinds = crate::compile(&packs).unwrap();
            assert!(
                kinds
                    .debug
                    .diagnostics
                    .iter()
                    .all(|d| d.level != sim_core::rules::Level::Warning),
                "{:?}",
                kinds.debug.diagnostics
            );
            let mut world = sim_core::sim::new_world_with(&s, kinds).unwrap();
            let mut failed = Vec::new();
            sim_core::sim::run_checks(&mut world, &s.checks, |e| {
                if let sim_core::sim::Event::Expected { text, result, .. } = e
                    && !matches!(result, Ok((true, _)))
                {
                    failed.push(format!("{text}: {result:?}"));
                }
            });
            assert!(failed.is_empty(), "{}: {failed:?}", f.display());
        };
        let f = make(&root, Make::Scenario, "meadow").unwrap();
        runs(
            &f,
            "run 2h\nexpect count chicken > 0\nexpect traps chicken == 0\nexpect traps fox == 0\n",
        );
        make(&root, Make::Pack, "moss").unwrap();
        runs(
            &root.join("scenarios/moss.scenario"),
            "run 1h\nexpect count moss_moss > 100\nexpect count moss_walker > 0\n\
             expect traps moss_walker == 0\nexpect traps moss_moss == 0\n",
        );
    }

    /// `+ Copy of`: pick, take the offered name, and the copy is made,
    /// listed and opened; a pack's copy gets a scenario to try it.
    #[test]
    fn copies_of_scenarios_and_packs() {
        let root = tree("copy");
        std::fs::write(
            root.join("scenarios/plays_p.scenario"),
            "# p's world\nrules ../packs/p\nseed 3\nstart k 1 / 9\n",
        )
        .unwrap();
        let mut m = Menu::scan(&root);
        m.go(Page::Scenarios);
        m.key(Key::Down);
        m.key(Key::Enter);
        assert_eq!(
            m.page,
            Page::Pick {
                what: Make::Scenario
            }
        );
        m.key(Key::Enter);
        assert!(matches!(&m.page, Page::Make { text, from: Some(_), .. } if text == "a_copy"));
        let made = root.join("scenarios/a_copy.scenario");
        assert_eq!(m.key(Key::Enter), Some(Action::Edit(made.clone())));
        let text = std::fs::read_to_string(&made).unwrap();
        assert!(text.starts_with("# Copied from ") && text.ends_with("# A\n# is first\nseed 1\n"));
        assert_eq!(
            copy_name(&root, Make::Scenario, &root.join("scenarios/a.scenario")),
            "a_copy2"
        );

        // A pack no scenario plays: a bare one to try it.
        m.go(Page::Packs);
        m.key(Key::Down);
        m.key(Key::Enter);
        assert_eq!(m.page, Page::Pick { what: Make::Pack });
        m.key(Key::Down); // rules, then packs/p
        m.key(Key::Enter);
        assert!(matches!(&m.page, Page::Make { text, .. } if text == "p_copy"));
        let lines: Vec<String> = m
            .rows(100, 30)
            .iter()
            .map(|r| r.cells.iter().map(|c| char::from(c.0)).collect())
            .collect();
        assert!(
            lines.iter().any(|l| l.contains("> packs/p_copy_")),
            "the name's row, chosen"
        );
        assert!(
            lines.iter().any(|l| l.contains("Its names are p's")),
            "{lines:?}"
        );
        let rules = root.join("packs/p_copy/k.rules");
        assert_eq!(m.key(Key::Enter), Some(Action::Edit(rules.clone())));
        assert!(
            std::fs::read_to_string(&rules)
                .unwrap()
                .contains("kind k { }")
        );
        // plays_p.scenario plays packs/p alone: copied, its rules the copy's.
        let s = std::fs::read_to_string(root.join("scenarios/p_copy.scenario")).unwrap();
        assert!(
            s.contains("rules ../packs/p_copy\nseed 3\nstart k 1 / 9"),
            "{s}"
        );
        assert!(!s.contains("../packs/p\n"), "{s}");
        let sc = crate::read_scenario(&root.join("scenarios/p_copy.scenario").to_string_lossy())
            .unwrap();
        let packs: Vec<PathBuf> = sc.packs.iter().map(PathBuf::from).collect();
        sim_core::sim::new_world_with(&sc, crate::compile(&packs).unwrap()).unwrap();
        // The built-in rules: the scenario that names no packs (a.scenario).
        let open = make_copy(&root, Make::Pack, &root.join("rules"), "mine").unwrap();
        assert_eq!(open, root.join("packs/mine/x.rules"));
        let s = std::fs::read_to_string(root.join("scenarios/mine.scenario")).unwrap();
        assert!(s.contains("# is first\nrules ../packs/mine\nseed 1"), "{s}");
        // Never over a file; and a pack with nothing to copy says so.
        assert!(make_copy(&root, Make::Pack, &root.join("packs/p"), "p_copy").is_err());
        assert!(make_copy(&root, Make::Scenario, &made, "a").is_err());
        std::fs::create_dir_all(root.join("packs/empty")).unwrap();
        assert!(make_copy(&root, Make::Pack, &root.join("packs/empty"), "e2").is_err());
    }

    /// The real Life pack and its scenario, copied: the copy makes a world
    /// of its own and runs.
    #[test]
    fn a_copy_of_life_runs() {
        let root = tree("life-copy");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::create_dir_all(root.join("packs/life")).unwrap();
        for (from, to) in [
            ("packs/life/life.rules", "packs/life/life.rules"),
            ("scenarios/life.scenario", "scenarios/life.scenario"),
        ] {
            std::fs::copy(repo.join(from), root.join(to)).unwrap();
        }
        make_copy(&root, Make::Pack, &root.join("packs/life"), "mylife").unwrap();
        let f = root.join("scenarios/mylife.scenario");
        let s = crate::read_scenario(&f.to_string_lossy()).unwrap();
        assert_eq!(
            s.packs,
            [root.join("scenarios/../packs/mylife").to_string_lossy()]
        );
        let packs: Vec<PathBuf> = s.packs.iter().map(PathBuf::from).collect();
        let mut world = sim_core::sim::new_world_with(
            &Scenario {
                width: 64,
                height: 64,
                ..s
            },
            crate::compile(&packs).unwrap(),
        )
        .unwrap();
        for _ in 0..10 {
            sim_core::sim::step(&mut world);
        }
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
