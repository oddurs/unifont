// SPDX-License-Identifier: GPL-3.0-or-later
//
// fontina — a font manager.
// Copyright (C) 2026 Oddur Sigurdsson
//
// This program is free software: you can redistribute it and/or modify it under the
// terms of the GNU General Public License as published by the Free Software Foundation,
// either version 3 of the License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with this
// program. If not, see <https://www.gnu.org/licenses/>.

//! `fontina ui`: a keyboard-first browser over the index. Facets on the left, families
//! or faces in the middle, details and a shaped preview on the right. Every action is
//! one the CLI can do, and the status line shows the equivalent command.
//!
//! The palette is the terminal's own 16 colours; truecolor is used only for the
//! preview, so the screen looks native in any theme.

mod controls;
mod glyphs;
mod preview;
mod sheet;

use anyhow::Result;
use fontina_core::index::FacetCount;
use fontina_core::model::EmbeddingLevel;
use fontina_core::render::RenderOptions;
use fontina_core::{ActivationState, FaceFilter, FaceMetadata, FaceSummary, Facets, Family, Index};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Which facet dimension a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Facet {
    Weight,
    Width,
    Style,
    Variable,
    Color,
    Spacing,
    Script,
    Language,
    License,
    Freedom,
    Vendor,
    Tag,
    Collection,
    Activation,
    Container,
    Source,
}

impl Facet {
    fn label(self) -> &'static str {
        match self {
            Facet::Weight => "Weight",
            Facet::Width => "Width",
            Facet::Style => "Style",
            Facet::Variable => "Variable",
            Facet::Color => "Color",
            Facet::Spacing => "Spacing",
            Facet::Script => "Script",
            Facet::Language => "Language",
            Facet::License => "License",
            Facet::Freedom => "Freedom",
            Facet::Vendor => "Vendor",
            Facet::Tag => "Tag",
            Facet::Collection => "Collection",
            Facet::Activation => "Activation",
            Facet::Container => "Container",
            Facet::Source => "Source",
        }
    }
    fn flag(self) -> &'static str {
        match self {
            Facet::Weight => "--weight",
            Facet::Width => "--width",
            Facet::Style => "--italic",
            Facet::Variable => "--variable",
            Facet::Color => "--color",
            Facet::Spacing => "--mono",
            Facet::Script => "--script",
            Facet::Language => "--lang",
            Facet::License => "--license",
            Facet::Freedom => "--freedom",
            Facet::Vendor => "--vendor",
            Facet::Tag => "--tag",
            Facet::Collection => "--collection",
            Facet::Activation => "--activation",
            Facet::Container => "--container",
            Facet::Source => "--under",
        }
    }
}

struct FacetRow {
    facet: Facet,
    value: String,
    count: i64,
    header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Facets,
    List,
    /// The axis and feature controls, only reachable when the face offers any.
    Controls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    Search,
    Tag,
    Collection,
    Text,
    /// A codepoint or a block name, in the glyph map.
    Glyph,
}

struct Input {
    kind: InputKind,
    buf: String,
}

pub struct App {
    index: Index,
    query: String,
    selected: BTreeMap<Facet, String>,
    facets: Facets,
    rows: Vec<FacetRow>,
    families: Vec<Family>,
    faces: Vec<FaceSummary>,
    /// `Some(name)` while a family is open.
    open_family: Option<String>,
    focus: Focus,
    list: ListState,
    facet_list: ListState,
    input: Option<Input>,
    status: String,
    help: bool,
    preview_text: Option<String>,
    preview_size: f32,
    detail: Option<FaceMetadata>,
    detail_id: Option<i64>,
    /// The listing row for `detail_id`: tags and activation state, joined once per
    /// selection rather than once per frame.
    detail_summary: Option<FaceSummary>,
    /// Axis positions and feature toggles for the face on show. Rebuilt whenever the
    /// selection changes, because they describe that face and no other.
    controls: controls::Controls,
    /// The glyph map, while it is open. It covers the whole screen, so it is a mode
    /// rather than a pane: there is no room to browse and read coverage at once.
    glyphs: Option<glyphs::Glyphs>,
    /// Columns the glyph grid used when it was last drawn, so a PageDown moves by what
    /// the reader can see rather than by a guess.
    glyph_cols: usize,
    /// The waterfall or comparison, while it is open. Full-screen for the same reason
    /// the glyph map is: rendered type needs the width.
    sheet: Option<sheet::Sheet>,
    /// Terminal lines the sheet had on the last frame, so a PageDown moves by a screen.
    sheet_visible: usize,
    preview: preview::Cache,
    /// How the browser registers a font with the operating system.
    ///
    /// A field rather than a call to `fontina_platform::activator()` at each use, so a
    /// test can drive `a`, `i`, `d` and `u` without touching the machine it runs on. It
    /// is not a nicety: the soak below presses every key hundreds of times, and against
    /// the real backend that meant copying fixtures into the developer's own font
    /// directory and registering them with the running session.
    activator: Box<dyn fontina_platform::FontActivator>,
}

pub fn run(db: &Path) -> Result<()> {
    let index = Index::open(db)?;
    let mut app = App::new(index)?;
    let mut terminal = ratatui::try_init()?;
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

/// What the event loop should do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

/// How many families a specimen sheet will draw before it stops.
///
/// Each row is a query, a font read from disk and a rasterisation, all in the frame
/// that opens the sheet, and all thrown away on the next terminal resize. Sixty-four is
/// far more than fits on a screen and small enough that opening it on a library of
/// thousands is instant rather than a hang.
const SPECIMEN_CAP: usize = 64;

impl App {
    fn new(index: Index) -> Result<Self> {
        Self::with_activator(index, fontina_platform::activator())
    }

    fn with_activator(
        index: Index,
        activator: Box<dyn fontina_platform::FontActivator>,
    ) -> Result<Self> {
        let mut app = App {
            index,
            query: String::new(),
            selected: BTreeMap::new(),
            facets: Facets::default(),
            rows: Vec::new(),
            families: Vec::new(),
            faces: Vec::new(),
            open_family: None,
            focus: Focus::List,
            list: ListState::default(),
            facet_list: ListState::default(),
            input: None,
            status: String::new(),
            help: false,
            preview_text: None,
            preview_size: 28.0,
            detail: None,
            detail_id: None,
            detail_summary: None,
            controls: controls::Controls::default(),
            glyphs: None,
            glyph_cols: 16,
            sheet: None,
            sheet_visible: 20,
            preview: preview::Cache::default(),
            activator,
        };
        app.reload()?;
        if app.families.is_empty() && app.selected.is_empty() && app.query.is_empty() {
            app.status =
                "index is empty: run `fontina scan <dir>` or `fontina scan --system`".into();
        }
        Ok(app)
    }

    // ----- data -----

    fn filter(&self) -> FaceFilter {
        let mut f = FaceFilter {
            query: (!self.query.is_empty()).then(|| self.query.clone()),
            family: self.open_family.clone(),
            ..Default::default()
        };
        for (facet, v) in &self.selected {
            match facet {
                Facet::Weight => {
                    let b: u16 = v.parse().unwrap_or(400);
                    f.weight = Some((b.saturating_sub(50), b + 49));
                }
                Facet::Width => {
                    let b: f32 = v.parse().unwrap_or(100.0);
                    f.width = Some(((b - 6.0).max(0.0) as u16, (b + 6.0) as u16));
                }
                Facet::Style => f.italic = Some(v == "italic"),
                Facet::Variable => f.variable = Some(true),
                Facet::Color => f.color = Some(true),
                Facet::Spacing => f.monospace = Some(v == "monospace"),
                Facet::Script => f.scripts = vec![v.clone()],
                Facet::Language => f.lang = Some(v.clone()),
                Facet::License => f.license = Some(v.clone()),
                Facet::Freedom => f.freedom = v.parse().ok(),
                Facet::Vendor => f.vendor = Some(v.clone()),
                Facet::Tag => f.tag = Some(v.clone()),
                Facet::Collection => f.collection = Some(v.clone()),
                Facet::Activation => {
                    if v == "none" {
                        f.active = Some(false);
                    } else {
                        f.activation = v.parse().ok();
                    }
                }
                Facet::Container => f.container = Some(v.clone()),
                Facet::Source => f.path_prefix = Some(v.clone()),
            }
        }
        f
    }

    /// The CLI command that shows what the screen shows.
    fn command_line(&self) -> String {
        let mut s = String::from(if self.open_family.is_some() {
            "fontina list"
        } else {
            "fontina families"
        });
        if !self.query.is_empty() {
            s.push_str(&format!(" {:?}", self.query));
        }
        if let Some(f) = &self.open_family {
            s.push_str(&format!(" --family {f:?}"));
        }
        for (facet, v) in &self.selected {
            match facet {
                Facet::Variable | Facet::Color => s.push_str(&format!(" {}", facet.flag())),
                // Two flags, neither taking a value. Without an arm here the generic one
                // below emits `--mono proportional`, which clap reads as `--mono` plus a
                // full-text search for "proportional": the opposite of what is on screen,
                // and it returns nothing rather than erring.
                Facet::Spacing => s.push_str(if v == "monospace" {
                    " --mono"
                } else {
                    " --proportional"
                }),
                Facet::Style => s.push_str(&format!(" --italic={}", v == "italic")),
                Facet::Weight => {
                    let b: u16 = v.parse().unwrap_or(400);
                    s.push_str(&format!(" --weight {}-{}", b.saturating_sub(50), b + 49));
                }
                Facet::Width => {
                    let b: f32 = v.parse().unwrap_or(100.0);
                    s.push_str(&format!(
                        " --width {}-{}",
                        (b - 6.0).max(0.0) as u16,
                        (b + 6.0) as u16
                    ));
                }
                Facet::Activation if v == "none" => s.push_str(" --active=false"),
                _ => s.push_str(&format!(" {} {}", facet.flag(), shell_quote(v))),
            }
        }
        s
    }

    fn reload(&mut self) -> Result<()> {
        let filter = self.filter();
        self.facets = self.index.facets(&FaceFilter {
            family: None,
            ..filter.clone()
        })?;
        if self.open_family.is_some() {
            self.faces = self.index.list(&filter)?;
            self.families.clear();
        } else {
            self.families = self.index.families(&filter)?;
            self.faces.clear();
        }
        self.rows = build_rows(&self.facets, &self.selected);
        let len = self.list_len();
        let sel = self.list.selected().unwrap_or(0).min(len.saturating_sub(1));
        self.list.select((len > 0).then_some(sel));
        if self.facet_list.selected().is_none() && !self.rows.is_empty() {
            self.facet_list
                .select(Some(first_selectable(&self.rows, 0)));
        }
        // Anything the pane shows may have moved underneath it — a tag added, a face
        // activated, a rescan — so the cached detail is dropped and read again.
        self.detail = None;
        self.detail_summary = None;
        self.refresh_detail()?;
        Ok(())
    }

    fn list_len(&self) -> usize {
        if self.open_family.is_some() {
            self.faces.len()
        } else {
            self.families.len()
        }
    }

    /// The face the right pane describes: the selected face, or a family's representative.
    fn current_face_id(&self) -> Option<i64> {
        let i = self.list.selected()?;
        if self.open_family.is_some() {
            self.faces.get(i).map(|f| f.id)
        } else {
            self.families.get(i).map(|f| f.representative)
        }
    }

    /// Every face the current selection stands for (all faces of a family).
    fn current_face_ids(&self) -> Vec<i64> {
        let Some(i) = self.list.selected() else {
            return Vec::new();
        };
        if self.open_family.is_some() {
            self.faces.get(i).map(|f| vec![f.id]).unwrap_or_default()
        } else {
            self.families
                .get(i)
                .map(|f| f.ids.clone())
                .unwrap_or_default()
        }
    }

    /// Load everything the detail pane shows, once per selection. The draw path runs on
    /// every frame and only borrows what this leaves behind.
    fn refresh_detail(&mut self) -> Result<()> {
        let id = self.current_face_id();
        if id == self.detail_id && self.detail.is_some() {
            return Ok(());
        }
        let face = match id {
            Some(id) => self.index.get_face(id)?,
            None => None,
        };
        self.detail_summary = match (id, &face) {
            (Some(id), Some(_)) => self.index.summaries(&[id])?.into_iter().next(),
            _ => None,
        };
        // A row that has gone since the listing was built leaves no detail, so it must
        // leave no id either: the two always describe the same face.
        let next_id = id.filter(|_| face.is_some());
        // Rebuild the controls only when the face itself changes. `reload` clears
        // `detail` on every tag, activation, search and rescan, so rebuilding whenever
        // it is None would throw away axes and toggles the reader had set on a face
        // they never left.
        if next_id != self.detail_id || next_id.is_none() {
            self.controls = match &face {
                Some(f) => controls::Controls::for_face(f),
                None => controls::Controls::default(),
            };
        }
        self.detail_id = next_id;
        if self.focus == Focus::Controls && self.controls.is_empty() {
            self.focus = Focus::List;
        }
        self.detail = face;
        Ok(())
    }

    // ----- events -----

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|f| self.draw(f))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if self.on_key(key)? == Flow::Quit {
                return Ok(());
            }
        }
    }

    /// Apply one key press. Split out of the event loop so the browser can be driven
    /// without a terminal: every key a person can press reaches the same code a test
    /// does, which is what `tests::a_long_run_of_arbitrary_keys_keeps_every_invariant`
    /// relies on.
    fn on_key(&mut self, key: event::KeyEvent) -> Result<Flow> {
        if self.input.is_some() {
            self.handle_input_key(key.code)?;
            return Ok(Flow::Continue);
        }
        if self.help {
            self.help = false;
            return Ok(Flow::Continue);
        }
        // Ctrl-C still quits from anywhere; a full-screen mode takes every other key,
        // so nothing underneath can move while it covers the panes.
        if !ctrl_c(&key) {
            if self.glyphs.is_some() {
                self.handle_glyph_key(key.code)?;
                return Ok(Flow::Continue);
            }
            if self.sheet.is_some() {
                self.handle_sheet_key(key.code)?;
                return Ok(Flow::Continue);
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => return Ok(Flow::Quit),
            KeyCode::Char('c') if ctrl => return Ok(Flow::Quit),
            KeyCode::Esc => {
                if self.open_family.is_some() {
                    self.close_family()?;
                } else if !self.query.is_empty() || !self.selected.is_empty() {
                    self.query.clear();
                    self.selected.clear();
                    self.reload()?;
                } else {
                    return Ok(Flow::Quit);
                }
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('m') => self.open_glyphs(),
            KeyCode::Char('w') => self.open_sheet(sheet::Kind::Waterfall)?,
            KeyCode::Char('C') => self.open_sheet(sheet::Kind::Compare)?,
            KeyCode::Char('P') => self.open_sheet(sheet::Kind::Specimen)?,
            KeyCode::Char('s') => self.open_specimen()?,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Facets => Focus::List,
                    Focus::List if !self.controls.is_empty() => Focus::Controls,
                    Focus::List | Focus::Controls => Focus::Facets,
                }
            }
            KeyCode::Char('/') => self.start_input(InputKind::Search, self.query.clone()),
            KeyCode::Char('e') => {
                let text = self.preview_text.clone().unwrap_or_default();
                self.start_input(InputKind::Text, text)
            }
            KeyCode::Char('t') => self.start_input(InputKind::Tag, String::new()),
            KeyCode::Char('c') => self.start_input(InputKind::Collection, String::new()),
            KeyCode::Char('x') => {
                self.selected.clear();
                self.query.clear();
                self.reload()?;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.preview_size = (self.preview_size + 4.0).min(160.0)
            }
            KeyCode::Char('-') => self.preview_size = (self.preview_size - 4.0).max(8.0),
            KeyCode::Char('a') => self.activate(ActivationState::User)?,
            KeyCode::Char('A') => self.activate(ActivationState::Session)?,
            KeyCode::Char('i') => self.activate(ActivationState::Installed)?,
            KeyCode::Char('d') => self.deactivate(false)?,
            KeyCode::Char('u') => self.deactivate(true)?,
            KeyCode::Char('R') => self.rescan()?,
            KeyCode::Down | KeyCode::Char('j') => self.step(1)?,
            KeyCode::Up | KeyCode::Char('k') => self.step(-1)?,
            KeyCode::PageDown | KeyCode::Char('f') if ctrl || key.code == KeyCode::PageDown => {
                self.step(15)?
            }
            KeyCode::PageUp | KeyCode::Char('b') if ctrl || key.code == KeyCode::PageUp => {
                self.step(-15)?
            }
            KeyCode::Home | KeyCode::Char('g') => self.jump(0)?,
            KeyCode::End | KeyCode::Char('G') => self.jump(usize::MAX)?,
            // In the controls the arrows move an axis, so they cannot also open a
            // family; Space and Enter still toggle the row under the cursor.
            KeyCode::Right | KeyCode::Char('l') if self.focus == Focus::Controls => {
                self.controls.adjust(1);
            }
            KeyCode::Left | KeyCode::Char('h') if self.focus == Focus::Controls => {
                self.controls.adjust(-1);
            }
            KeyCode::Char('L') if self.focus == Focus::Controls => {
                self.controls.adjust(10);
            }
            KeyCode::Char('H') if self.focus == Focus::Controls => {
                self.controls.adjust(-10);
            }
            KeyCode::Char('n') if self.focus == Focus::Controls => {
                self.controls.cycle_instance(1);
            }
            KeyCode::Char('p') if self.focus == Focus::Controls => {
                self.controls.cycle_instance(-1);
            }
            KeyCode::Char('0') if self.focus == Focus::Controls => self.controls.reset(),
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => {
                match self.focus {
                    Focus::Facets => self.toggle_facet()?,
                    Focus::List => self.open_family()?,
                    Focus::Controls => {
                        self.controls.toggle();
                    }
                }
            }
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h')
                if self.focus == Focus::List =>
            {
                self.close_family()?
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Open the glyph map on the face on show. A face with no coverage at all — a
    /// broken font, or one still being scanned — has nothing to map.
    fn open_glyphs(&mut self) {
        let Some(face) = &self.detail else {
            self.status = "no face on show".into();
            return;
        };
        let map = glyphs::Glyphs::for_face(face);
        if map.is_empty() {
            self.status = "this face maps no codepoints".into();
            return;
        }
        self.status = format!(
            "{} codepoints in {} block(s)   (fontina glyphs {})",
            map.covered(),
            map.blocks().len(),
            self.detail_id.map(|id| id.to_string()).unwrap_or_default()
        );
        self.glyphs = Some(map);
    }

    /// Open a waterfall over the selected face, or a comparison across everything the
    /// selection stands for.
    fn open_sheet(&mut self, kind: sheet::Kind) -> Result<()> {
        let ids: Vec<i64> = match kind {
            sheet::Kind::Waterfall => self.current_face_id().into_iter().collect(),
            // Inside an open family the listing is the family's own faces, and that is
            // exactly the view a reader presses `C` from. `current_face_ids` narrows to
            // the selected face there, which would compare a face with itself.
            sheet::Kind::Compare if self.open_family.is_some() => {
                self.faces.iter().map(|f| f.id).collect()
            }
            sheet::Kind::Compare => self.current_face_ids(),
            // Inside an open family `reload` clears `families`, so collecting
            // representatives there yields nothing and `P` becomes a dead key on a full
            // index. The listing is the family's own faces, so specimen those instead —
            // the same hazard `Compare` has an arm for, two lines above.
            sheet::Kind::Specimen if self.open_family.is_some() => {
                self.faces.iter().map(|f| f.id).collect()
            }
            // Every family the current filter left, in the order the listing has them,
            // represented by the face the listing already chose to stand for it.
            sheet::Kind::Specimen => self.families.iter().map(|f| f.representative).collect(),
        };
        // A waterfall is nine rows and a comparison is one family's faces. A specimen
        // is every family in the index, and `filter` sets no limit — so on a real
        // library this is thousands of queries, thousands of file reads and thousands of
        // rasterisations, in one frame, thrown away again on the next terminal resize.
        // Cap it and say so in the title rather than hang.
        let total = ids.len();
        let ids: Vec<i64> = ids.into_iter().take(SPECIMEN_CAP).collect();
        // Read every face once, here. The sheet is drawn on every frame and holds what
        // it needs; querying per row per frame is the mistake #36 fixed for the pane.
        let mut faces = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(face) = self.index.get_face(*id)? {
                faces.push(face);
            }
        }
        if faces.is_empty() {
            self.status = match kind {
                sheet::Kind::Specimen => "no families on show".into(),
                _ => "no face on show".into(),
            };
            return Ok(());
        }
        let sheet = match kind {
            sheet::Kind::Waterfall => sheet::Sheet::waterfall(
                faces.remove(0),
                self.controls.variations(),
                self.controls.forced_features(),
            ),
            sheet::Kind::Compare => sheet::Sheet::compare(faces, self.preview_size),
            sheet::Kind::Specimen => {
                sheet::Sheet::specimen(faces, self.preview_size, self.open_family.is_some())
            }
        };
        if kind == sheet::Kind::Specimen && total > SPECIMEN_CAP {
            self.status = format!(
                "showing the first {SPECIMEN_CAP} of {total}; narrow the filter to see the rest"
            );
            self.sheet = Some(sheet);
            return Ok(());
        }
        self.status = format!(
            "{}   (fontina specimen {})",
            sheet.title(),
            ids.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        self.sheet = Some(sheet);
        Ok(())
    }

    /// Write a specimen for the selection and hand it to the user's browser.
    ///
    /// A terminal is at its worst at exactly what choosing a typeface needs: real
    /// antialiasing at text sizes, spacing you can trust, hinting. `specimen.rs` already
    /// renders all of that, in a file that makes no network request and needs nothing
    /// installed. This is the one keystroke that reaches it — the whole of the graphical
    /// escape hatch, and the reason there is no second interface to maintain.
    fn open_specimen(&mut self) -> Result<()> {
        let ids = self.current_face_ids();
        if ids.is_empty() {
            self.status = "no face on show".into();
            return Ok(());
        }
        let mut faces = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(face) = self.index.get_face(*id)? {
                faces.push(face);
            }
        }
        if faces.is_empty() {
            self.status = "no face on show".into();
            return Ok(());
        }
        let html = fontina_core::specimen::render(
            &faces,
            &fontina_core::specimen::SpecimenOptions {
                text: self.preview_text.clone(),
                link: false,
                title: None,
            },
        )?;
        // One file per fontina, overwritten each time: a browser tab the user reloads is
        // better than a temp directory that fills up with every press of the key.
        let path =
            std::env::temp_dir().join(format!("fontina-specimen-{}.html", std::process::id()));
        std::fs::write(&path, &html)?;
        let ran = format!(
            "fontina specimen {} -o {}",
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(" "),
            path.display()
        );
        self.status = match fontina_platform::open::file(&path) {
            Ok(opened) => format!("opened in {}   ({ran})", opened.with),
            // A machine with no desktop is an ordinary place to run this. The file is
            // written either way, and its path is the useful half of the answer.
            Err(e) => format!("wrote {} but could not open it: {e}", path.display()),
        };
        Ok(())
    }

    /// Keys the sheet owns while it is open. Everything else is swallowed, for the same
    /// reason the glyph map swallows: it covers the panes underneath.
    fn handle_sheet_key(&mut self, code: KeyCode) -> Result<()> {
        let visible = self.sheet_visible.max(1);
        let Some(sheet) = self.sheet.as_mut() else {
            return Ok(());
        };
        match code {
            KeyCode::Esc
            | KeyCode::Char('q')
            | KeyCode::Char('w')
            | KeyCode::Char('C')
            | KeyCode::Char('P') => {
                self.sheet = None;
            }
            KeyCode::Down | KeyCode::Char('j') => sheet.scroll_by(1, visible),
            KeyCode::Up | KeyCode::Char('k') => sheet.scroll_by(-1, visible),
            // A page is a screen less one line of overlap, and at least one line: in a
            // pane one line tall the overlap would be the whole page, and PageDown,
            // PageUp and Space were dead keys.
            KeyCode::PageDown | KeyCode::Char(' ') => {
                sheet.scroll_by((visible as i32 - 1).max(1), visible)
            }
            KeyCode::PageUp => sheet.scroll_by(-(visible as i32 - 1).max(1), visible),
            KeyCode::Home | KeyCode::Char('g') => sheet.scroll_by(i32::MIN / 2, visible),
            KeyCode::End | KeyCode::Char('G') => sheet.scroll_by(i32::MAX / 2, visible),
            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') => {
                let delta = if code == KeyCode::Char('-') {
                    -4.0
                } else {
                    4.0
                };
                let fixed = !sheet.resize(delta) && sheet.kind() == sheet::Kind::Waterfall;
                if fixed {
                    // Silence would read as a broken key; the ladder is deliberate.
                    self.status =
                        "a waterfall is the size ladder; C compares faces at one size".into();
                }
            }
            KeyCode::Char('e') => {
                let text = self.preview_text.clone().unwrap_or_default();
                self.start_input(InputKind::Text, text);
            }
            KeyCode::Char('s') => self.open_specimen()?,
            _ => {}
        }
        Ok(())
    }

    /// Keys the glyph map owns while it is open. Returns whether it took the key.
    fn handle_glyph_key(&mut self, code: KeyCode) -> Result<bool> {
        // The grid is laid out at draw time; the columns used for scrolling are the
        // ones the last frame used, which is what the reader is looking at.
        let cols = self.glyph_cols.max(1);
        let Some(map) = self.glyphs.as_mut() else {
            return Ok(false);
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => self.glyphs = None,
            KeyCode::Down | KeyCode::Char('j') => map.scroll_by(1, cols),
            KeyCode::Up | KeyCode::Char('k') => map.scroll_by(-1, cols),
            KeyCode::PageDown => map.scroll_by(10, cols),
            KeyCode::PageUp => map.scroll_by(-10, cols),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => map.select(1),
            KeyCode::Left | KeyCode::Char('h') => map.select(-1),
            KeyCode::Home | KeyCode::Char('g') => map.scroll_by(i32::MIN / 2, cols),
            KeyCode::End | KeyCode::Char('G') => map.scroll_by(i32::MAX / 2, cols),
            KeyCode::Char('/') => self.start_input(InputKind::Glyph, String::new()),
            // Everything else is swallowed. The map covers the panes, so a key that
            // activated a font or opened a family would leave it showing the coverage of
            // a face that is no longer on show, with no way to tell.
            _ => {}
        }
        Ok(true)
    }

    fn start_input(&mut self, kind: InputKind, buf: String) {
        if matches!(kind, InputKind::Tag | InputKind::Collection)
            && self.current_face_ids().is_empty()
        {
            self.status = "nothing selected".into();
            return;
        }
        self.input = Some(Input { kind, buf });
    }

    fn handle_input_key(&mut self, code: KeyCode) -> Result<()> {
        let Some(input) = self.input.as_mut() else {
            return Ok(());
        };
        match code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                input.buf.pop();
                if input.kind == InputKind::Search {
                    self.query = self
                        .input
                        .as_ref()
                        .map(|i| i.buf.clone())
                        .unwrap_or_default();
                    self.reload()?;
                }
            }
            KeyCode::Enter => {
                let Input { kind, buf } = self.input.take().expect("checked");
                let value = buf.trim().to_string();
                match kind {
                    InputKind::Search => {
                        self.query = value;
                        self.reload()?;
                    }
                    InputKind::Text => {
                        self.preview_text = (!value.is_empty()).then_some(value);
                    }
                    InputKind::Glyph => {
                        let cols = self.glyph_cols.max(1);
                        if let Some(map) = self.glyphs.as_mut() {
                            self.status = if map.find(&value, cols) {
                                match map.found() {
                                    Some(cp) => {
                                        format!("U+{cp:04X} in {}", map.selected_map_name())
                                    }
                                    None => map.selected_map_name(),
                                }
                            } else {
                                format!("nothing covered matches {value:?}")
                            };
                        }
                    }
                    InputKind::Tag => {
                        if !value.is_empty() {
                            let ids = self.current_face_ids();
                            let n = self.index.tag(&ids, &value)?;
                            self.status = format!(
                                "tagged {n} face(s) with {value:?}   (fontina tag add {} <targets>)",
                                shell_quote(&value)
                            );
                            self.reload()?;
                        }
                    }
                    InputKind::Collection => {
                        if !value.is_empty() {
                            let ids = self.current_face_ids();
                            let n = self.index.add_to_collection(&value, &ids)?;
                            self.status = format!(
                                "added {n} face(s) to {value:?}   (fontina collection add {} <targets>)",
                                shell_quote(&value)
                            );
                            self.reload()?;
                        }
                    }
                }
            }
            KeyCode::Char(c) => {
                input.buf.push(c);
                if input.kind == InputKind::Search {
                    self.query = self
                        .input
                        .as_ref()
                        .map(|i| i.buf.clone())
                        .unwrap_or_default();
                    self.reload()?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn step(&mut self, delta: i32) -> Result<()> {
        match self.focus {
            Focus::List => {
                let len = self.list_len();
                if len == 0 {
                    return Ok(());
                }
                let cur = self.list.selected().unwrap_or(0) as i32;
                let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
                self.list.select(Some(next));
                self.refresh_detail()?;
            }
            Focus::Facets => {
                if self.rows.is_empty() {
                    return Ok(());
                }
                let cur = self.facet_list.selected().unwrap_or(0) as i32;
                let mut next = (cur + delta).clamp(0, self.rows.len() as i32 - 1) as usize;
                // Skip headers in the direction of travel. Row 0 is always a header, so
                // running off an end has to fall back to the nearest selectable row
                // rather than abandoning the move: PageUp from row 8 used to do nothing
                // at all, while Home on the same row settled on row 1.
                let mut ran_off = false;
                while self.rows[next].header {
                    let n = next as i32 + delta.signum();
                    if n < 0 || n >= self.rows.len() as i32 {
                        ran_off = true;
                        break;
                    }
                    next = n as usize;
                }
                if ran_off {
                    next = first_selectable(&self.rows, next);
                }
                self.facet_list.select(Some(next));
            }
            Focus::Controls => self.controls.move_cursor(delta),
        }
        Ok(())
    }

    fn jump(&mut self, to: usize) -> Result<()> {
        match self.focus {
            Focus::List => {
                let len = self.list_len();
                if len > 0 {
                    self.list.select(Some(to.min(len - 1)));
                    self.refresh_detail()?;
                }
            }
            Focus::Facets => {
                if !self.rows.is_empty() {
                    let i = to.min(self.rows.len() - 1);
                    self.facet_list
                        .select(Some(first_selectable(&self.rows, i)));
                }
            }
            Focus::Controls => {
                let last = self.controls.len().saturating_sub(1);
                self.controls
                    .move_cursor(to.min(last) as i32 - self.controls.cursor() as i32);
            }
        }
        Ok(())
    }

    fn toggle_facet(&mut self) -> Result<()> {
        let Some(i) = self.facet_list.selected() else {
            return Ok(());
        };
        let Some(row) = self.rows.get(i) else {
            return Ok(());
        };
        if row.header {
            return Ok(());
        }
        let (facet, value) = (row.facet, row.value.clone());
        if self.selected.get(&facet) == Some(&value) {
            self.selected.remove(&facet);
        } else {
            self.selected.insert(facet, value);
        }
        self.list.select(Some(0));
        self.reload()
    }

    fn open_family(&mut self) -> Result<()> {
        if self.open_family.is_some() {
            return Ok(());
        }
        let Some(i) = self.list.selected() else {
            return Ok(());
        };
        let Some(fam) = self.families.get(i) else {
            return Ok(());
        };
        self.open_family = Some(fam.name.clone());
        self.list.select(Some(0));
        self.reload()
    }

    fn close_family(&mut self) -> Result<()> {
        let Some(name) = self.open_family.take() else {
            return Ok(());
        };
        self.reload()?;
        if let Some(i) = self.families.iter().position(|f| f.name == name) {
            self.list.select(Some(i));
            self.refresh_detail()?;
        }
        Ok(())
    }

    // ----- actions -----

    fn activate(&mut self, state: ActivationState) -> Result<()> {
        let ids = self.current_face_ids();
        if ids.is_empty() {
            self.status = "nothing selected".into();
            return Ok(());
        }
        let conflicts = crate::collect_conflicts(&self.index, &ids)?;
        if !conflicts.is_empty() {
            let c = &conflicts[0];
            self.status = format!(
                "{} conflict(s): {} {} ({}). Use `fontina activate --replace` to override.",
                conflicts.len(),
                c.face.family,
                c.face.subfamily,
                c.reason
            );
            return Ok(());
        }
        let verb = match state {
            ActivationState::Installed => "install",
            ActivationState::Session => "activate --session",
            ActivationState::User => "activate",
        };
        let mut n = 0;
        for (path, faces) in crate::files_for(&self.index, &ids)? {
            let result = match state {
                ActivationState::Installed => self.activator.install(&path).map(|p| {
                    self.index
                        .set_activation(&faces, state, Some(&p.to_string_lossy()))
                        .map(|_| ())
                        .map_err(|e| fontina_platform::PlatformError::Os(e.to_string()))
                }),
                _ => {
                    let scope = if state == ActivationState::Session {
                        fontina_platform::Scope::Session
                    } else {
                        fontina_platform::Scope::User
                    };
                    self.activator.activate(&path, scope).map(|_| {
                        self.index
                            .set_activation(&faces, state, None)
                            .map_err(|e| fontina_platform::PlatformError::Os(e.to_string()))
                    })
                }
            };
            match result.and_then(|r| r) {
                Ok(()) => n += faces.len(),
                Err(e) => {
                    self.status = format!("{}: {e}", path.display());
                    self.reload()?;
                    return Ok(());
                }
            }
        }
        self.status = format!("{verb}: {n} face(s)   (fontina {verb} <targets>)");
        self.reload()
    }

    fn deactivate(&mut self, uninstall: bool) -> Result<()> {
        let ids = self.current_face_ids();
        if ids.is_empty() {
            self.status = "nothing selected".into();
            return Ok(());
        }
        let mut n = 0;
        for (path, faces) in crate::files_for(&self.index, &ids)? {
            let record = self.index.activation(faces[0])?;
            let result = if uninstall {
                match record.and_then(|r| r.installed_path) {
                    Some(p) => self.activator.uninstall(Path::new(&p)).map(|()| true),
                    None => continue,
                }
            } else {
                if record.is_none() {
                    continue;
                }
                self.activator.deactivate(&path)
            };
            match result {
                Ok(_) => {
                    self.index.clear_activation(&faces)?;
                    n += faces.len();
                }
                Err(e) => {
                    self.status = format!("{}: {e}", path.display());
                    self.reload()?;
                    return Ok(());
                }
            }
        }
        let verb = if uninstall { "uninstall" } else { "deactivate" };
        self.status = format!("{verb}: {n} face(s)   (fontina {verb} <targets>)");
        self.reload()
    }

    fn rescan(&mut self) -> Result<()> {
        let roots: Vec<std::path::PathBuf> = self
            .index
            .sources()?
            .into_iter()
            .filter(|s| Path::new(&s.path).is_dir())
            .map(|s| s.path.into())
            .collect();
        if roots.is_empty() {
            self.status = "no sources to rescan".into();
            return Ok(());
        }
        let report = fontina_core::scan::scan(
            &mut self.index,
            &roots,
            &fontina_core::ScanOptions {
                prune: true,
                ..Default::default()
            },
        )?;
        self.status = format!(
            "rescanned {} source(s): {} parsed, {} unchanged, {} removed, {} failed   (fontina scan --prune)",
            roots.len(),
            report.parsed,
            report.unchanged,
            report.removed,
            report.failed.len()
        );
        self.preview.clear();
        self.reload()
    }

    // ----- drawing -----

    fn draw(&mut self, f: &mut ratatui::Frame) {
        let area = f.area();
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(26),
                Constraint::Percentage(40),
                Constraint::Min(30),
            ])
            .split(vertical[0]);
        self.draw_facets(f, columns[0]);
        self.draw_list(f, columns[1]);
        self.draw_detail(f, columns[2]);
        self.draw_status(f, vertical[1]);
        self.draw_keys(f, vertical[2]);
        if self.sheet.is_some() {
            self.draw_sheet(f, vertical[0]);
        }
        if self.glyphs.is_some() {
            self.draw_glyphs(f, vertical[0]);
        }
        if self.help {
            self.draw_help(f, area);
        }
    }

    /// The waterfall or the comparison: each row rendered, labelled, and stacked, with
    /// the whole sheet scrolling by terminal lines.
    fn draw_sheet(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let Some(sheet) = &self.sheet else { return };
        f.render_widget(Clear, area);
        // A waterfall runs to a few hundred lines, so where you are in it is worth
        // saying; without it the only cue is that scrolling stopped.
        let position = match sheet.lines() {
            0 => String::new(),
            total => format!(" [{}/{}]", sheet.scroll_row() + 1, total),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(
                " {}{position} — e sets the text, Esc closes ",
                sheet.title()
            ));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.width < 8 || inner.height < 2 {
            return;
        }

        let visible = inner.height as usize;
        self.sheet_visible = visible;
        let width = inner.width;
        // A specimen's words live on its rows, so the sheet's sample text is not its to
        // use. Withholding it here as well as ignoring it there keeps `is_built_for`
        // honest: otherwise pressing `e` rebuilds a sheet whose output cannot change.
        let text = match self.sheet.as_ref().map(sheet::Sheet::kind) {
            Some(sheet::Kind::Specimen) => None,
            _ => self.preview_text.clone(),
        };

        // Lay the sheet out once per pane width and sample text, not once per frame:
        // a waterfall is nine rasterisations and a comparison is one per face.
        if !self
            .sheet
            .as_ref()
            .is_some_and(|s| s.is_built_for(width, text.as_deref()))
        {
            let Some(sheet) = self.sheet.take() else {
                return;
            };
            let mut lines: Vec<Line> = Vec::new();
            for row in sheet.rows() {
                let words = sheet.text_for(row, text.as_deref());
                let opts = sheet.options(row, words, width as u32);
                lines.push(Line::from(Span::styled(
                    row.label.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
                let px_rows = (row.size.ceil() as u32 * 2).max(2);
                lines.extend(self.preview.lines(&row.face, &opts, px_rows));
                lines.push(Line::from(""));
            }
            let mut sheet = sheet;
            sheet.set_built(width, text, lines);
            self.sheet = Some(sheet);
        }

        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        // Re-clamp against the height being drawn: a pane that grew since the last
        // keypress must not start past the end and render blank.
        sheet.scroll_by(0, visible);
        f.render_widget(Paragraph::new(sheet.window(visible)), inner);
    }

    /// The glyph map: covered blocks down the left, the characters of the selected one
    /// in a grid on the right. Full width, because a coverage grid needs the room.
    fn draw_glyphs(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let Some(map) = &self.glyphs else { return };
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" glyph map — / to search, m or Esc to close ");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(34), Constraint::Min(20)])
            .split(inner);

        // Blocks, with the selected one marked and its coverage as a fraction.
        let selected = map.selected_index();
        let visible = columns[0].height as usize;
        let offset = selected.saturating_sub(visible.saturating_sub(1));
        let rows: Vec<Line> = map
            .blocks()
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(i, b)| {
                let style = if i == selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(
                    format!(
                        "{} {:<22} {:>4}/{}",
                        if i == selected { ">" } else { " " },
                        truncate(&b.block, 22),
                        b.codepoints.len(),
                        b.block_size
                    ),
                    style,
                ))
            })
            .collect();
        f.render_widget(Paragraph::new(rows), columns[0]);

        let grid = columns[1];
        // Every cell is two columns wide whatever it holds, so the grid lines up whether
        // the block is Latin, CJK or combining marks. `LABEL` covers the widest possible
        // codepoint label, `10FFFF`, plus its separating space.
        const LABEL: usize = 7;
        const CELL: usize = 2;
        let cols = ((grid.width as usize).saturating_sub(LABEL) / CELL).max(1);
        self.glyph_cols = cols;
        // The pane may have been resized since the last scroll, so re-clamp against the
        // width being drawn now; otherwise a scrolled block can render blank.
        let Some(map) = self.glyphs.as_mut() else {
            return;
        };
        map.clamp_scroll(cols);
        let (start, found) = (map.scroll_row() * cols, map.found());
        let Some(current) = map.selected() else {
            return;
        };
        let mut lines: Vec<Line> = Vec::new();
        for row in current.codepoints[start.min(current.codepoints.len())..]
            .chunks(cols)
            .take(grid.height as usize)
        {
            let mut spans = vec![Span::styled(
                format!("{:<LABEL$}", format!("{:04X}", row[0])),
                Style::default().fg(Color::DarkGray),
            )];
            for &cp in row {
                let cell = fontina_core::unicode::cell_for(cp);
                let style = if Some(cp) == found {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default()
                };
                // Pad each cell out to CELL columns, so a double-width glyph takes the
                // space of one cell rather than pushing the rest of the row along.
                spans.push(Span::styled(
                    format!("{}{}", cell.glyph, " ".repeat(CELL - cell.width)),
                    style,
                ));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), grid);
    }

    fn border(&self, focused: bool) -> Style {
        if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    }

    fn draw_facets(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                if r.header {
                    ListItem::new(Line::from(Span::styled(
                        r.facet.label().to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )))
                } else {
                    let on = self.selected.get(&r.facet) == Some(&r.value);
                    let mark = if on { "●" } else { " " };
                    let label = facet_value_label(r.facet, &r.value);
                    let width = area.width.saturating_sub(4) as usize;
                    let count = r.count.to_string();
                    let room = width.saturating_sub(count.len() + 2);
                    let text = format!(
                        "{mark} {:<room$} {count}",
                        truncate(&label, room),
                        room = room
                    );
                    let style = if on {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::from(Span::styled(text, style)))
                }
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.border(self.focus == Focus::Facets))
                    .title(format!(" {} faces ", self.facets.faces)),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, area, &mut self.facet_list);
    }

    fn draw_list(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let width = area.width.saturating_sub(2) as usize;
        let items: Vec<ListItem> = if let Some(fam) = &self.open_family {
            let _ = fam;
            self.faces
                .iter()
                .map(|face| {
                    let flags = format!(
                        "{}{}{}",
                        if face.variable { "V" } else { " " },
                        if face.color { "C" } else { " " },
                        activation_mark(face.activation),
                    );
                    let tags = if face.tags.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", face.tags.join(", "))
                    };
                    let left = format!("{} {}{}", face.subfamily, face.container, tags);
                    ListItem::new(Line::from(format!(
                        "{:<w$} {flags}",
                        truncate(&left, width.saturating_sub(5)),
                        w = width.saturating_sub(5)
                    )))
                })
                .collect()
        } else {
            self.families
                .iter()
                .map(|fam| {
                    let flags = format!(
                        "{}{}{}",
                        if fam.variable { "V" } else { " " },
                        if fam.color { "C" } else { " " },
                        if fam.active > 0 { "●" } else { " " },
                    );
                    let count = format!("{:>3}", fam.faces);
                    let room = width.saturating_sub(9);
                    ListItem::new(Line::from(format!(
                        "{:<room$} {count} {flags}",
                        truncate(&fam.name, room),
                        room = room
                    )))
                })
                .collect()
        };
        let title = match &self.open_family {
            Some(fam) => format!(" {} · {} face(s) ", fam, self.faces.len()),
            None => format!(" {} families ", self.families.len()),
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.border(self.focus == Focus::List))
                    .title(title),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, area, &mut self.list);
    }

    fn draw_detail(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.border(false))
            .title(" Details ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        // Borrowed, never cloned: this runs on every frame, four times a second while
        // the browser sits idle. `refresh_detail` is what queries the index.
        let Some(face) = self.detail.as_ref() else {
            f.render_widget(Paragraph::new("Nothing selected."), inner);
            return;
        };
        let mut lines: Vec<Line> = Vec::new();
        let bold = Style::default().add_modifier(Modifier::BOLD);
        lines.push(Line::from(vec![
            Span::styled(face.names.family.clone(), bold),
            Span::raw(" "),
            Span::raw(face.names.subfamily.clone()),
        ]));
        lines.push(kv(
            "style",
            format!(
                "weight {} · width {}% · {}",
                face.style.weight.round(),
                face.style.width.round(),
                face.style.css.style
            ),
        ));
        if let Some(v) = &face.variable {
            lines.push(kv(
                "axes",
                v.axes
                    .iter()
                    .map(|a| format!("{} {}–{} ({})", a.tag, a.min, a.max, a.default))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        lines.push(kv(
            "glyphs",
            format!(
                "{} · {} codepoints · {}",
                face.glyph_count,
                face.coverage.codepoints,
                face.coverage
                    .scripts
                    .iter()
                    .take(5)
                    .map(|s| s.script.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ));
        let feats = face.features.gsub.len() + face.features.gpos.len();
        if feats > 0 {
            lines.push(kv(
                "features",
                format!(
                    "{feats}: {}",
                    face.features
                        .gsub
                        .iter()
                        .chain(face.features.gpos.iter())
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            ));
        }
        lines.extend(license_lines(face));
        if let Some(d) = face
            .names
            .designer
            .as_deref()
            .or(face.names.manufacturer.as_deref())
        {
            lines.push(kv("designer", d.to_string()));
        }
        if let Some(s) = self.detail_summary.as_ref() {
            if !s.tags.is_empty() {
                lines.push(kv("tags", s.tags.join(", ")));
            }
            lines.push(kv(
                "state",
                match s.activation {
                    Some(a) => a.as_str().to_string(),
                    None => "not active".into(),
                },
            ));
        }
        lines.push(kv(
            "file",
            format!(
                "{}{}",
                face.file.path,
                if face.file.face_count > 1 {
                    format!(" #{}", face.index)
                } else {
                    String::new()
                }
            ),
        ));
        lines.push(Line::from(""));
        // Rows the block will actually occupy once wrapped, not how many lines were
        // pushed. The paragraph wraps, so a long value — a file path, a licence reason —
        // takes several rows, and counting them as one pushed the bottom of the pane off
        // the screen.
        let text_rows: u16 = lines.iter().map(|l| wrapped_rows(l, inner.width)).sum();
        // Controls take the rows they need, capped so the preview never disappears.
        // The pane asks for a title plus a row per control, but never takes so much that
        // the preview vanishes, and never less than a title plus one row: a pane Tab can
        // reach has to show the cursor sitting in it.
        let control_rows = if self.controls.is_empty() {
            0
        } else {
            let spare = inner.height.saturating_sub(text_rows + 4);
            // Either a title and at least one control, or nothing: a pane showing only
            // its own title would hide the cursor sitting in it.
            if spare < 2 {
                0
            } else {
                (self.controls.len() as u16 + 1).min(spare)
            }
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(text_rows.min(inner.height)),
                Constraint::Length(control_rows),
                Constraint::Min(0),
            ])
            .split(inner);
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[0]);
        if control_rows > 0 {
            self.draw_controls(f, chunks[1], face);
        }
        let preview_area = chunks[2];
        if preview_area.height < 2 || preview_area.width < 4 {
            return;
        }
        let text = self
            .preview_text
            .clone()
            .or_else(|| face.names.sample_text.clone())
            .unwrap_or_else(|| preview::sample_for(face));
        let opts = self.render_options(text, preview_area.width as u32);
        let lines = self
            .preview
            .lines(face, &opts, preview_area.height as u32 * 2);
        f.render_widget(Paragraph::new(lines), preview_area);
    }

    /// The preview's render settings: the sample text at the chosen size, positioned by
    /// whatever the reader has done to the axes and features.
    fn render_options(&self, text: String, cols: u32) -> RenderOptions {
        RenderOptions {
            text,
            size: self.preview_size,
            variations: self.controls.variations(),
            features: self.controls.forced_features(),
            padding: 1,
            max_width: Some(cols),
        }
    }

    /// Axes as `tag  value` with a bar, features as a checkbox, the selected row
    /// highlighted when the pane has focus.
    fn draw_controls(&self, f: &mut ratatui::Frame, area: Rect, face: &FaceMetadata) {
        let focused = self.focus == Focus::Controls;
        let mut lines: Vec<Line> = Vec::new();
        let title = match self.controls.instance_name(face) {
            Some(name) => format!("axes & features — {name}"),
            None if self.controls.is_variable() => "axes & features — custom".to_string(),
            // "custom" would be nonsense for a face with no axes to be custom about.
            None => "features".to_string(),
        };
        lines.push(Line::from(Span::styled(
            title,
            Style::default().fg(Color::DarkGray),
        )));
        // Scroll so the cursor is always on screen; without this a reader moves down,
        // the marker disappears, and the arrows adjust an axis they cannot see.
        let body = area.height.saturating_sub(1) as usize;
        let offset = self
            .controls
            .cursor()
            .saturating_sub(body.saturating_sub(1));
        for (i, row) in self.controls.rows().enumerate().skip(offset).take(body) {
            let selected = focused && i == self.controls.cursor();
            let marker = if selected { ">" } else { " " };
            let style = if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let text = match row {
                controls::Row::Axis(a) => {
                    let span = (a.max - a.min).max(f32::EPSILON);
                    let filled = (((a.value - a.min) / span) * 12.0).round() as usize;
                    format!(
                        "{marker} {:<4} {:>8}  [{}{}] {}",
                        a.tag,
                        fmt_axis(a.value),
                        "=".repeat(filled.min(12)),
                        " ".repeat(12 - filled.min(12)),
                        // The designer's own name for the axis, when it differs from
                        // the tag; `wght` labelled "wght" is noise.
                        if a.label == a.tag { "" } else { &a.label },
                    )
                }
                controls::Row::Feature(feature) => format!(
                    "{marker} {:<4} [{}] {}",
                    feature.tag,
                    if feature.on { "x" } else { " " },
                    feature.label
                ),
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_status(&self, f: &mut ratatui::Frame, area: Rect) {
        let line = if let Some(input) = &self.input {
            let prompt = match input.kind {
                InputKind::Search => "search",
                InputKind::Tag => "tag",
                InputKind::Collection => "collection",
                InputKind::Text => "preview text",
                InputKind::Glyph => "codepoint or block",
            };
            Line::from(vec![
                Span::styled(format!(" {prompt}: "), Style::default().fg(Color::Cyan)),
                Span::raw(input.buf.clone()),
                Span::styled("▏", Style::default().fg(Color::Cyan)),
            ])
        } else if !self.status.is_empty() {
            Line::from(Span::raw(format!(" {}", self.status)))
        } else {
            Line::from(Span::styled(
                format!(" $ {}", self.command_line()),
                Style::default().fg(Color::DarkGray),
            ))
        };
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_keys(&self, f: &mut ratatui::Frame, area: Rect) {
        let keys = " / search  ⇥ facets  ⏎ open  ⌫ back  t tag  c collection  a/A activate  d deactivate  i install  u uninstall  e text  +/- size  P specimens  s export  R rescan  ? help  q quit";
        f.render_widget(
            Paragraph::new(Span::styled(keys, Style::default().fg(Color::DarkGray))),
            area,
        );
    }

    fn draw_help(&self, f: &mut ratatui::Frame, area: Rect) {
        let text = "\
 fontina ui

 Move        j/k ↑/↓ PgUp/PgDn g/G        Tab cycles facets, list, controls
 Filter      / type to search  Esc clears   Enter/Space toggles a facet   x clears all
 Families    Enter opens a family, Backspace/Esc closes it
 Organise    t tag the selection   c add it to a collection
 Activate    a for the user, A until logout, i install a copy, d deactivate, u uninstall
 Preview     e sets the sample text   + / - change the size
 Controls    h/l ←/→ move an axis (H/L by ten)   Space toggles a feature
             n/p step through named instances   0 resets everything
 Glyphs      m opens the glyph map: h/l pick a block, j/k scroll, / finds a
             codepoint (U+0041, 0x41, 41) or a block by name
 Sheets      w waterfalls the face down the size ladder; C compares every face
             the selection stands for; P sets every family in its own face, which
             is the one view that answers what a typeface looks like without
             opening it. j/k scroll, +/- resize
 Specimen    s writes an HTML specimen for the selection and opens it in your
             browser, for the things a terminal cannot show honestly
 Index       R rescans every source (fontina scan --prune)
 Quit        q

 The status line shows the CLI command for what you see. Everything here is a command.

 any key to close";
        let w = 90.min(area.width);
        // Two spare lines. `Paragraph` without `.wrap()` truncates in silence, so a box
        // sized exactly to the text means the next line anyone adds deletes "any key to
        // close" with no error anywhere.
        let h = 28.min(area.height);
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            rect,
        );
    }
}

fn build_rows(facets: &Facets, selected: &BTreeMap<Facet, String>) -> Vec<FacetRow> {
    let mut rows = Vec::new();
    let mut section = |facet: Facet, counts: &[FacetCount], cap: usize| {
        if counts.is_empty() {
            return;
        }
        rows.push(FacetRow {
            facet,
            value: String::new(),
            count: 0,
            header: true,
        });
        let chosen = selected.get(&facet);
        for c in counts.iter().take(cap) {
            rows.push(FacetRow {
                facet,
                value: c.value.clone(),
                count: c.count,
                header: false,
            });
        }
        // Keep a selected value visible even when it is past the cap.
        if let Some(v) = chosen
            && !counts.iter().take(cap).any(|c| &c.value == v)
            && let Some(c) = counts.iter().find(|c| &c.value == v)
        {
            rows.push(FacetRow {
                facet,
                value: c.value.clone(),
                count: c.count,
                header: false,
            });
        }
    };
    let flags = [FacetCount {
        value: "variable".into(),
        count: facets.variable,
    }];
    let color = [FacetCount {
        value: "color".into(),
        count: facets.color,
    }];
    section(Facet::Weight, &facets.weight, 9);
    section(Facet::Width, &facets.width, 9);
    section(Facet::Style, &facets.style, 2);
    if facets.variable > 0 {
        section(Facet::Variable, &flags, 1);
    }
    if facets.color > 0 {
        section(Facet::Color, &color, 1);
    }
    section(Facet::Spacing, &facets.spacing, 2);
    section(Facet::Script, &facets.script, 8);
    section(Facet::Language, &facets.language, 8);
    section(Facet::License, &facets.license, 6);
    // Four states at most, so nothing is ever hidden behind a cap.
    section(Facet::Freedom, &facets.freedom, 4);
    section(Facet::Tag, &facets.tag, 10);
    section(Facet::Collection, &facets.collection, 10);
    section(Facet::Activation, &facets.activation, 4);
    section(Facet::Vendor, &facets.vendor, 6);
    section(Facet::Container, &facets.container, 5);
    section(Facet::Source, &facets.source, 6);
    rows
}

fn first_selectable(rows: &[FacetRow], from: usize) -> usize {
    (from..rows.len())
        .find(|&i| !rows[i].header)
        .or_else(|| (0..from).rev().find(|&i| !rows[i].header))
        .unwrap_or(0)
}

fn facet_value_label(facet: Facet, value: &str) -> String {
    match facet {
        Facet::Weight => format!(
            "{value} {}",
            fontina_core::index::weight_name(value.parse().unwrap_or(400))
        ),
        Facet::Width => format!(
            "{value}% {}",
            fontina_core::index::width_name(value.parse().unwrap_or(100.0))
        ),
        Facet::Source => Path::new(value)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

/// A labelled line in the details pane.
fn kv(k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<10}"), Style::default().fg(Color::DarkGray)),
        Span::raw(v),
    ])
}

/// What the details pane says about a face's licence.
///
/// The verdict and its reason, not an SPDX string on its own: whether a font may be
/// studied, changed and passed on is the fact that decides whether it can be used at
/// all, and an identifier only answers that for a reader who already knows the list.
fn license_lines(face: &FaceMetadata) -> Vec<Line<'static>> {
    let verdict = fontina_core::freedom::assess(face.license.spdx.as_deref());
    let mut lines = vec![
        kv(
            "license",
            face.license
                .spdx
                .clone()
                .unwrap_or_else(|| "none embedded".into()),
        ),
        Line::from(vec![
            Span::styled(
                format!("{:<10}", "freedom"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                verdict.freedom.to_string(),
                Style::default()
                    .fg(match verdict.freedom {
                        fontina_core::Freedom::Free => Color::Green,
                        fontina_core::Freedom::Nonfree => Color::Red,
                        _ => Color::Yellow,
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    if verdict.freedom != fontina_core::Freedom::Free {
        // "free" needs no explaining, and the pane shares its height with the controls
        // and the preview. Every other verdict is a reason to go and read something.
        lines.push(Line::from(Span::styled(
            format!("{:<10}{}", "", verdict.reason),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if !face.license.reserved_font_names.is_empty() {
        lines.push(kv("reserved", face.license.reserved_font_names.join(", ")));
    }
    if let Some(os2) = &face.os2
        && !matches!(os2.embedding.level, EmbeddingLevel::Installable)
    {
        // Reported, never acted on: these bits are the file's assertion about itself,
        // not a term of the licence. `freedom.rs` says why at length.
        lines.push(kv(
            "embedding",
            format!("{:?} (reported, not enforced)", os2.embedding.level),
        ));
    }
    lines
}

/// The one key that quits from anywhere, including out of a full-screen mode.
fn ctrl_c(key: &event::KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Axis values without trailing noise: `400`, not `400.0`; `87.5` kept as it is.
fn fmt_axis(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v:.1}")
    }
}

fn activation_mark(a: Option<ActivationState>) -> &'static str {
    match a {
        Some(ActivationState::Session) => "s",
        Some(ActivationState::User) => "●",
        Some(ActivationState::Installed) => "i",
        None => " ",
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || "-_./:@+=".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Rows a line takes once the paragraph has wrapped it.
///
/// Greedy, at whitespace, breaking a word only when it cannot fit on a line of its own,
/// which is how ratatui wraps. `width / columns` rounded up is a row short whenever a
/// break lands at a space: on an eighty column terminal that lost the `.ttf` off the end
/// of the file name.
///
/// Whitespace costs its own columns. The paragraph is drawn with `Wrap { trim: false }`,
/// and that keeps every space — including a run at the start of a line — so counting
/// words alone under-charges by the width of every gap. Two lines in this pane are made
/// almost entirely of such a gap: `kv` pads its label to ten columns, and the licence
/// reason is indented by ten. Under-counting there is what clips the bottom of the block,
/// which is the one thing this function exists to prevent.
fn wrapped_rows(line: &Line<'_>, cols: u16) -> u16 {
    let cols = cols.max(1) as usize;
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut rows = 1usize;
    let mut used = 0usize;
    // Alternating runs of whitespace and non-whitespace. A gap is laid down as it comes;
    // a word moves to the next line whole if it does not fit and could fit alone.
    for token in tokens(&text) {
        let w = token.chars().count();
        let space = token.starts_with(char::is_whitespace);
        if used + w <= cols {
            used += w;
        } else if space {
            // A gap that runs off the end fills the line and carries the rest over.
            let over = used + w - cols;
            rows += over.div_ceil(cols);
            used = over % cols;
        } else if w <= cols {
            rows += 1;
            used = w;
        } else {
            let taken = w.div_ceil(cols);
            rows += taken;
            used = w - (taken - 1) * cols;
        }
    }
    rows.try_into().unwrap_or(u16::MAX)
}

/// `text` split into runs of whitespace and runs of everything else, in order.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let space = rest.starts_with(char::is_whitespace);
        let end = rest
            .char_indices()
            .find(|(_, c)| c.is_whitespace() != space)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app over the fixture fonts, with nothing drawn.
    /// An activator that answers like the real one and touches nothing.
    ///
    /// The soak presses every key hundreds of times, and `a`, `A`, `i`, `d` and `u` are
    /// keys. Against the real backend that copied fixtures into the developer's own font
    /// directory and registered them with the running login session — on every
    /// `cargo test`, on every machine, on CI. It was also most of the run time: a
    /// CoreText registration and a four-hundred-kilobyte copy, ten thousand times over.
    ///
    /// It still answers truthfully enough for the index to be exercised: `install`
    /// returns a path, `deactivate` says something was registered.
    use std::path::PathBuf;

    struct Harmless;

    impl fontina_platform::FontActivator for Harmless {
        fn install(&self, file: &Path) -> fontina_platform::Result<PathBuf> {
            Ok(file.with_extension("installed"))
        }
        fn uninstall(&self, _installed: &Path) -> fontina_platform::Result<()> {
            Ok(())
        }
        fn activate(
            &self,
            _file: &Path,
            _scope: fontina_platform::Scope,
        ) -> fontina_platform::Result<()> {
            Ok(())
        }
        fn deactivate(&self, _file: &Path) -> fontina_platform::Result<bool> {
            Ok(true)
        }
    }

    /// Where this process keeps the scanned fixtures and the copies made from them.
    ///
    /// Emptied once, on first use, so a run leaves one run's worth behind rather than
    /// every run's: the copies cannot be deleted as they are finished with, because the
    /// browser holding one is still open and Windows will not unlink an open file.
    fn scratch() -> &'static Path {
        static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("fontina-ui-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        })
    }

    /// The fixtures, scanned once for the whole test binary.
    ///
    /// Parsing six fonts in a debug build costs seven seconds, and the soak builds a
    /// fresh browser for every key it holds down: fifty keys, six minutes of parsing to
    /// exercise key presses that take microseconds each. Scanning once and copying the
    /// result is the same index, arrived at the same way, without paying for it fifty
    /// times.
    fn template() -> &'static Path {
        static TEMPLATE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEMPLATE.get_or_init(|| {
            let db = scratch().join("template.db");
            let _ = std::fs::remove_file(&db);
            let mut index = Index::open(&db).unwrap();
            let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
            fontina_core::scan::scan(&mut index, &[fixtures], &Default::default()).unwrap();
            db
        })
    }

    /// A browser over its own copy of the scanned fixtures.
    ///
    /// A copy, not a shared file: several of these tests write to the index — a tag, an
    /// activation record, a removed file — and one test's writes must not be another
    /// test's starting point.
    fn app() -> App {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let db = scratch().join(format!("app-{n}.db"));
        std::fs::copy(template(), &db).expect("copying the scanned fixtures");
        App::with_activator(Index::open(&db).unwrap(), Box::new(Harmless)).unwrap()
    }

    /// The activation keys record what they did, and the keys that undo them clear it.
    ///
    /// Nothing tested this before, because it could not be tested: the browser reached
    /// for the real backend, so a test of `a` would have registered a fixture with the
    /// developer's own session. With the activator behind a field the whole path is
    /// exercised — including the part that only the browser has, which is that the state
    /// a reader sees in the listing is the state that was recorded.
    #[test]
    fn the_activation_keys_record_what_they_did() {
        let press = |app: &mut App, c: char| {
            app.on_key(event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap()
        };

        for (key, want) in [
            ('a', ActivationState::User),
            ('A', ActivationState::Session),
            ('i', ActivationState::Installed),
        ] {
            let mut app = app();
            let ids = app.current_face_ids();
            assert!(!ids.is_empty(), "the first family is selected");
            press(&mut app, key);
            for id in &ids {
                let record = app.index.activation(*id).unwrap();
                assert_eq!(
                    record.as_ref().map(|r| r.state),
                    Some(want),
                    "{key:?} recorded {:?}",
                    record.as_ref().map(|r| r.state)
                );
            }
            assert!(
                app.detail_summary
                    .as_ref()
                    .is_some_and(|s| s.activation == Some(want)),
                "and the pane the reader is looking at says so"
            );

            // `d` for the two in-place states, `u` for the installed one: what the
            // command line calls deactivate and uninstall.
            press(
                &mut app,
                if want == ActivationState::Installed {
                    'u'
                } else {
                    'd'
                },
            );
            for id in &ids {
                assert!(
                    app.index.activation(*id).unwrap().is_none(),
                    "the record survived the key that undoes it"
                );
            }
        }
    }

    /// A page in a pane one line tall still moves.
    ///
    /// A page is a screen less one line of overlap, and in a one-line pane that left
    /// nothing: PageDown, PageUp and Space did nothing at all. `draw_sheet` gives up
    /// below two lines and never records a height it gave up on, so no reader could
    /// reach it — which is exactly the kind of arithmetic that becomes reachable later
    /// and is nobody's suspect when it does.
    #[test]
    fn a_page_in_a_one_line_pane_still_moves() {
        let mut app = app();
        app.open_sheet(sheet::Kind::Waterfall).unwrap();
        app.sheet_visible = 1;
        // A sheet has to be laid out before it can scroll; one line per row is enough.
        let rows = app.sheet.as_ref().unwrap().rows().len();
        assert!(rows > 1, "a waterfall has rows to page through");
        app.sheet
            .as_mut()
            .unwrap()
            .set_built(40, None, vec![Line::from("x"); rows]);

        app.handle_sheet_key(KeyCode::PageDown).unwrap();
        assert_eq!(
            app.sheet.as_ref().unwrap().scroll_row(),
            1,
            "PageDown in a one-line pane moves one line, not none"
        );
        app.handle_sheet_key(KeyCode::PageUp).unwrap();
        assert_eq!(app.sheet.as_ref().unwrap().scroll_row(), 0);
        app.handle_sheet_key(KeyCode::Char(' ')).unwrap();
        assert_eq!(app.sheet.as_ref().unwrap().scroll_row(), 1);
    }

    /// Pressing an activation key with nothing selected says so and changes nothing.
    #[test]
    fn activating_nothing_is_a_message_rather_than_a_mistake() {
        let mut app = app();
        app.query = "no font is called this".into();
        app.reload().unwrap();
        assert_eq!(app.list_len(), 0, "the listing is empty");
        app.on_key(event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.status.contains("nothing selected"),
            "the status says why nothing happened: {:?}",
            app.status
        );
        assert!(app.index.activations().unwrap().is_empty());
    }

    /// The graphical escape hatch, and the whole of it: a specimen for what is selected,
    /// written and handed to the desktop.
    #[test]
    fn the_specimen_key_writes_a_file_for_the_selection() {
        // No desktop in a test runner, and `open_specimen` must survive that: the file is
        // the point and the handler is best-effort. `true` accepts the path and does
        // nothing, which is exactly the "it opened" branch without opening anything.
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("BROWSER", "true") };
        let mut app = app();
        app.preview_text = Some("Hamburgefonstiv".into());
        let ids = app.current_face_ids();
        assert!(!ids.is_empty(), "the first family is selected");

        app.open_specimen().unwrap();
        unsafe { std::env::remove_var("BROWSER") };

        let path =
            std::env::temp_dir().join(format!("fontina-specimen-{}.html", std::process::id()));
        let html = std::fs::read_to_string(&path).expect("the specimen was written");
        assert!(html.starts_with("<!doctype html>") || html.starts_with("<!DOCTYPE html>"));
        assert!(
            html.contains("Hamburgefonstiv"),
            "the sample text the user set is in it"
        );
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "a specimen makes no network request, which is why it can be opened from /tmp"
        );
        assert!(
            app.status.contains("fontina specimen"),
            "the status line names the command that would do the same thing: {}",
            app.status
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Pressing it with nothing selected must say so rather than writing an empty page.
    #[test]
    fn the_specimen_key_says_when_there_is_nothing_to_show() {
        let mut app = app();
        app.list.select(None);
        app.open_specimen().unwrap();
        assert_eq!(app.status, "no face on show");
    }

    use crate::{Cli, Command};
    use clap::Parser as _;

    /// The status line is a promise: it names the command that would give you what is
    /// on screen. A facet whose flag takes no value must not be emitted with one.
    ///
    /// `Facet::Spacing` was, and clap did not complain — `list` has a positional query,
    /// so `--mono proportional` parsed as `--mono` plus a full-text search for
    /// "proportional". The copyable command meant the opposite of the screen and
    /// returned nothing.
    #[test]
    fn every_facet_emits_a_command_that_means_what_the_screen_says() {
        let mut app = app();
        // Every facet the browser can select, with a value it could really hold.
        for (facet, value) in [
            (Facet::Weight, "400"),
            (Facet::Width, "100"),
            (Facet::Style, "upright"),
            (Facet::Variable, "variable"),
            (Facet::Color, "color"),
            (Facet::Spacing, "monospace"),
            (Facet::Spacing, "proportional"),
            (Facet::Script, "Latn"),
            (Facet::Language, "en"),
            (Facet::License, "OFL-1.1"),
            (Facet::Freedom, "free"),
            (Facet::Vendor, "RSMS"),
            (Facet::Tag, "favourite"),
            (Facet::Collection, "Editorial"),
            (Facet::Activation, "none"),
            (Facet::Container, "ttf"),
        ] {
            app.selected.clear();
            app.selected.insert(facet, value.to_string());
            let line = app.command_line();
            let args: Vec<&str> = line.split_whitespace().skip(1).collect();
            // Parsing is the assertion: clap rejects an unknown flag or a value where
            // none belongs. A stray positional would not be rejected, so check for one.
            let parsed =
                Cli::try_parse_from(std::iter::once("fontina").chain(args.iter().copied()));
            let parsed =
                parsed.unwrap_or_else(|e| panic!("{facet:?}={value:?} emitted {line:?}: {e}"));
            // Both subcommands the browser names take a positional query, and a flag
            // emitted with a value it does not take lands there instead of erroring.
            // Matching only one of them is how this assertion goes quietly dead — the
            // browser opens on the family list, so `Command::List` alone never matched.
            let query = match &parsed.command {
                Command::List(args) | Command::Families(args) => args.query.clone(),
                _ => panic!("{line:?} names a subcommand this test cannot check for a query"),
            };
            assert!(
                query.is_none(),
                "{facet:?}={value:?} emitted {line:?}, which clap read as a search for {query:?}"
            );
        }
    }

    /// The two spacing buckets emit the two flags, not one flag and a word.
    #[test]
    fn the_spacing_facet_picks_the_flag_that_matches_the_bucket() {
        let mut app = app();
        app.selected.insert(Facet::Spacing, "monospace".into());
        assert!(
            app.command_line().contains("--mono"),
            "{}",
            app.command_line()
        );
        assert!(
            !app.command_line().contains("--proportional"),
            "{}",
            app.command_line()
        );
        app.selected.insert(Facet::Spacing, "proportional".into());
        assert!(
            app.command_line().contains("--proportional"),
            "{}",
            app.command_line()
        );
    }

    /// A deterministic pseudo-random source. Seeded, so a failure is reproducible from
    /// the seed the message prints, and dependency-free.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // xorshift64*, good enough to shuffle key presses and small enough to read.
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Every key the browser reacts to, plus a few it does not, because a person's
    /// keyboard has more keys than the ones we documented.
    fn key_alphabet() -> Vec<event::KeyEvent> {
        let ctrl = |c: char| event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |c: char| event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let code = |k: KeyCode| event::KeyEvent::new(k, KeyModifiers::NONE);
        let mut keys: Vec<event::KeyEvent> = "jkhlgGfbaAiduRxetcmwCnp0+-/?LH \n"
            .chars()
            .map(plain)
            .collect();
        keys.extend([ctrl('f'), ctrl('b')]);
        keys.extend(
            [
                KeyCode::Down,
                KeyCode::Up,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Enter,
                KeyCode::Esc,
                KeyCode::Tab,
                KeyCode::Backspace,
                KeyCode::Home,
                KeyCode::End,
                KeyCode::PageUp,
                KeyCode::PageDown,
                KeyCode::Delete,
                KeyCode::Insert,
                KeyCode::F(1),
            ]
            .map(code),
        );
        // Text that ends up in the search, tag and collection prompts.
        keys.extend("Amiri فونت 変".chars().map(plain));
        keys
    }

    /// Everything that has to be true of the browser between one key and the next,
    /// whatever the person pressed and in whatever order.
    fn check_invariants(app: &App, whence: &str) {
        let len = app.list_len();
        if let Some(i) = app.list.selected() {
            assert!(
                i < len.max(1),
                "{whence}: selection {i} outside a list of {len}"
            );
        }
        if len > 0 {
            assert!(
                app.list.selected().is_some(),
                "{whence}: a filled list with nothing selected"
            );
        }
        assert_eq!(
            app.detail.is_some(),
            app.detail_id.is_some(),
            "{whence}: the detail pane and the id it belongs to disagree"
        );
        if let (Some(s), Some(id)) = (&app.detail_summary, app.detail_id) {
            assert_eq!(
                s.id, id,
                "{whence}: the cached summary belongs to another face"
            );
        }
        if app.focus == Focus::Controls {
            assert!(
                !app.controls.is_empty(),
                "{whence}: focus sits on a controls pane the face does not have"
            );
        }
        assert!(
            (8.0..=160.0).contains(&app.preview_size),
            "{whence}: preview size {} outside its bounds",
            app.preview_size
        );
        if let Some(g) = &app.glyphs {
            assert!(
                g.is_empty() || g.selected_index() < g.blocks().len(),
                "{whence}: the glyph map points at a block it does not have"
            );
            assert!(
                g.covered() <= 0x11_0000,
                "{whence}: the glyph map counts more codepoints than Unicode has"
            );
        }
    }

    /// The test a daily driver needs: press keys, a great many of them, in an order
    /// nobody would choose, and require that the browser neither panics nor tells a lie
    /// about its own state. Every scripted test above walks a path someone thought of;
    /// this walks the ones nobody did.
    ///
    /// Deterministic: the seed is fixed, and a failure prints the key sequence that
    /// produced it, so it can be replayed.
    /// Hold each key down. Two hundred presses of one key, from a fresh browser, for
    /// every key there is.
    ///
    /// This is the half of the soak that randomness cannot reach: pressing `+` past the
    /// top of the preview size range takes thirty-three presses in a row, and a uniform
    /// stream of key presses will not produce that inside a run of any length anyone
    /// would wait for. It is also what a person does, by resting a finger on a key.
    /// The world changes while the browser is open.
    ///
    /// `fontina watch` is meant to run as a user service, so fonts appear and vanish
    /// from the index under a browser that is already showing them. The browser reloads
    /// after every action; this presses keys while the index is edited between them, and
    /// requires that nothing it is holding — a selection, a detail id, a cached summary —
    /// outlives what it points at.
    #[test]
    fn the_browser_survives_the_index_changing_underneath_it() {
        let mut app = app();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let paths: Vec<String> = app
            .index
            .list(&Default::default())
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert!(
            paths.len() >= 6,
            "the fixtures are all indexed to begin with"
        );

        let keys = "jkGgmwC\nl h";
        let mut removed = 0;
        for (round, path) in paths.iter().enumerate() {
            // A watcher drops a font that was deleted on disk.
            if app.index.remove_file(path).unwrap() {
                removed += 1;
            }
            app.reload().unwrap();
            check_invariants(&app, &format!("after {removed} face(s) vanished"));
            for c in keys.chars() {
                let key = event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                app.on_key(key).unwrap();
                check_invariants(&app, &format!("round {round}, key {c:?}"));
            }
            let backend = ratatui::backend::TestBackend::new(100, 30);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.draw(f)).unwrap();
        }
        assert_eq!(app.list_len(), 0, "every face was removed");

        // And they all come back.
        fontina_core::scan::scan(&mut app.index, &[fixtures], &Default::default()).unwrap();
        app.reload().unwrap();
        check_invariants(&app, "after everything was scanned again");
        assert!(app.list_len() > 0, "the listing came back");
    }

    #[test]
    fn holding_any_single_key_down_keeps_every_invariant() {
        for key in key_alphabet() {
            let mut app = app();
            for press in 1..=200 {
                let whence = format!("{:?} held for {press} presses", key.code);
                match app.on_key(key) {
                    Ok(Flow::Quit) => break,
                    Ok(Flow::Continue) => {}
                    Err(e) => panic!("{whence}: {e}"),
                }
                check_invariants(&app, &whence);
            }
            // And the screen still draws afterwards, at a size with no room to spare.
            let backend = ratatui::backend::TestBackend::new(40, 12);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.draw(f))
                .unwrap_or_else(|e| panic!("drawing after {:?} was held: {e}", key.code));
        }
    }

    #[test]
    fn a_long_run_of_arbitrary_keys_keeps_every_invariant() {
        let alphabet = key_alphabet();
        // Sizes a person really has, including ones too small to lay anything out in.
        let sizes = [
            (80u16, 24u16),
            (120, 40),
            (200, 60),
            (40, 12),
            (20, 6),
            (8, 3),
            (1, 1),
        ];
        for seed in [0x5eed_0001, 0x00f0_111a, 0xdead_beef] {
            let mut rng = Rng(seed);
            let mut app = app();
            let mut pressed: Vec<String> = Vec::new();
            let mut step = 0usize;
            while step < 600 {
                // People hold keys down. A uniform stream of single presses never walks
                // to the end of a long list or the top of the preview size range, so a
                // bound that is wrong is never reached: an earlier version of this test
                // did not notice the size clamp raised from 160 to 1000. One press in
                // eight becomes a run.
                let key = alphabet[rng.below(alphabet.len())];
                let run = if rng.below(8) == 0 {
                    2 + rng.below(48)
                } else {
                    1
                };
                for _ in 0..run {
                    step += 1;
                    pressed.push(format!("{:?}", key.code));
                    let whence =
                        format!("seed {seed:#x}, step {step}, after [{}]", pressed.join(" "));
                    match app.on_key(key) {
                        Ok(Flow::Quit) => {
                            app = self::tests::app();
                            pressed.clear();
                            continue;
                        }
                        Ok(Flow::Continue) => {}
                        Err(e) => panic!("{whence}: {e}"),
                    }
                    check_invariants(&app, &whence);
                    // Draw now and then, at whatever size: the layout is where the
                    // arithmetic lives, and a pane too small to fit is where it breaks.
                    if step.is_multiple_of(7) {
                        let (w, h) = sizes[rng.below(sizes.len())];
                        let backend = ratatui::backend::TestBackend::new(w, h);
                        let mut term = ratatui::Terminal::new(backend).unwrap();
                        term.draw(|f| app.draw(f))
                            .unwrap_or_else(|e| panic!("{whence}: drawing {w}x{h}: {e}"));
                    }
                }
            }
        }
    }

    #[test]
    fn the_detail_summary_is_cached_alongside_the_face() {
        let mut app = app();
        assert!(app.detail.is_some(), "the first family is selected");
        assert_eq!(app.detail_summary.as_ref().map(|s| s.id), app.detail_id);
        // A new selection reloads both together.
        app.list.select(Some(1));
        app.refresh_detail().unwrap();
        assert!(app.detail.is_some());
        assert_eq!(app.detail_summary.as_ref().map(|s| s.id), app.detail_id);
        // Redrawing does not: the same selection is a no-op.
        let id = app.detail_id;
        app.refresh_detail().unwrap();
        assert_eq!(app.detail_id, id);
        // A change to the index still reaches the pane, through the reload that
        // follows every action.
        app.index.tag(&[id.unwrap()], "favourite").unwrap();
        app.reload().unwrap();
        assert_eq!(
            app.detail_summary.as_ref().unwrap().tags,
            ["favourite"],
            "the cache is dropped on reload"
        );
    }

    #[test]
    fn a_face_that_went_away_leaves_no_stale_id() {
        let mut app = app();
        let path = app.detail.as_ref().unwrap().file.path.clone();
        // The listing still names the face, but the index no longer holds it.
        assert!(app.index.remove_file(&path).unwrap());
        app.detail_id = None;
        app.refresh_detail().unwrap();
        assert!(app.detail.is_none());
        assert!(app.detail_id.is_none(), "a stale id outlived its face");
        assert!(app.detail_summary.is_none());
    }

    /// Select the first face whose family starts with `prefix`, as a reader would by
    /// walking the list.
    fn select_family(app: &mut App, prefix: &str) {
        let i = (0..app.list_len())
            .find(|i| {
                app.list.select(Some(*i));
                app.refresh_detail().is_ok()
                    && app
                        .detail
                        .as_ref()
                        .is_some_and(|f| f.names.family.starts_with(prefix))
            })
            .unwrap_or_else(|| panic!("no family starting with {prefix:?}"));
        app.list.select(Some(i));
        app.refresh_detail().unwrap();
    }

    #[test]
    fn the_controls_describe_the_face_on_show_and_no_other() {
        let mut app = app();
        select_family(&mut app, "Bricolage");
        assert!(!app.controls.is_empty(), "a variable face offers controls");
        let variable_len = app.controls.len();

        // Move an axis, then look at a different face: the setting does not follow it.
        app.focus = Focus::Controls;
        // This face's default sits at the top of its first axis, so the headroom is
        // downward — which is itself worth pinning: `adjust` at a bound is a no-op.
        assert!(!app.controls.adjust(1), "already at the axis maximum");
        assert!(app.controls.adjust(-5));
        let moved = app.controls.coords();
        select_family(&mut app, "Amiri");
        assert_ne!(app.controls.coords(), moved);
        select_family(&mut app, "Bricolage");
        assert_eq!(app.controls.len(), variable_len);
        assert_eq!(
            app.controls.coords(),
            fontina_core::typography::default_coords(
                app.detail.as_ref().unwrap().variable.as_ref().unwrap()
            ),
            "returning to a face starts it at its defaults again"
        );
    }

    /// Every fixture offers at least a feature toggle, so the fallback is reached by
    /// emptying the index instead: no face, no controls, nowhere for the focus to be.
    #[test]
    fn focus_never_rests_on_controls_a_face_does_not_have() {
        let mut app = app();
        select_family(&mut app, "Bricolage");
        app.focus = Focus::Controls;
        assert!(!app.controls.is_empty());

        let path = app.detail.as_ref().unwrap().file.path.clone();
        assert!(app.index.remove_file(&path).unwrap());
        app.detail_id = None;
        app.refresh_detail().unwrap();

        assert!(
            app.controls.is_empty(),
            "a face that is gone offers nothing"
        );
        assert_ne!(
            app.focus,
            Focus::Controls,
            "focus must leave a pane that no longer exists"
        );
    }

    /// Tagging, activating, searching and rescanning all call `reload`, which clears the
    /// detail. None of them changes which face is selected, so none of them may undo
    /// what the reader set on it.
    #[test]
    fn an_action_on_the_selected_face_keeps_its_axes_and_toggles() {
        let mut app = app();
        select_family(&mut app, "Bricolage");
        app.focus = Focus::Controls;
        assert!(app.controls.adjust(-4));
        app.controls.move_cursor(app.controls.len() as i32);
        assert!(app.controls.toggle(), "the last row is a feature");
        let (coords, features) = (app.controls.coords(), app.controls.forced_features());
        assert!(!features.is_empty());

        let id = app.detail_id.unwrap();
        app.index.tag(&[id], "favourite").unwrap();
        app.reload().unwrap();

        assert_eq!(app.controls.coords(), coords, "the axes survived a reload");
        assert_eq!(
            app.controls.forced_features(),
            features,
            "the toggles survived a reload"
        );
        assert_eq!(app.focus, Focus::Controls, "and so did the focus");
    }

    /// The pane is drawn from `Controls::rows`, so a rendering check is really a check
    /// that every control reaches the screen with its tag on it.
    #[test]
    fn every_control_is_drawn_with_its_tag() {
        let mut app = app();
        select_family(&mut app, "Bricolage");
        let face = app.detail.clone().unwrap();
        let drawn: Vec<String> = app
            .controls
            .rows()
            .map(|row| match row {
                controls::Row::Axis(a) => a.tag.clone(),
                controls::Row::Feature(f) => f.tag.clone(),
            })
            .collect();
        assert_eq!(drawn.len(), app.controls.len());
        for tag in &drawn {
            assert_eq!(tag.chars().count(), 4, "{tag} is not an OpenType tag");
        }
        // Axes come first, and the variable ones are exactly the face's own.
        let axes: Vec<&str> = face
            .variable
            .as_ref()
            .unwrap()
            .axes
            .iter()
            .filter(|a| !a.hidden)
            .map(|a| a.tag.as_str())
            .collect();
        assert_eq!(&drawn[..axes.len()], &axes[..]);
    }

    #[test]
    fn the_glyph_map_opens_on_the_face_on_show_and_closes_again() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        assert!(app.glyphs.is_none());
        app.open_glyphs();
        let map = app.glyphs.as_ref().expect("Amiri maps codepoints");
        assert!(map.covered() > 0);
        assert!(app.status.contains("fontina glyphs"), "{}", app.status);

        // Every key it owns is taken from the panes underneath.
        assert!(app.handle_glyph_key(KeyCode::Char('j')).unwrap());
        assert!(app.handle_glyph_key(KeyCode::Char('l')).unwrap());
        // A key it does not own is swallowed rather than passed to the panes beneath:
        // activating a font from behind a full-screen map would leave the map describing
        // a face that is no longer on show.
        let id = app.detail_id;
        assert!(app.handle_glyph_key(KeyCode::Char('a')).unwrap());
        assert!(app.handle_glyph_key(KeyCode::Enter).unwrap());
        assert_eq!(app.detail_id, id, "nothing underneath moved");
        assert!(app.glyphs.is_some(), "and the map is still open");

        assert!(app.handle_glyph_key(KeyCode::Esc).unwrap());
        assert!(app.glyphs.is_none(), "Esc closes the map");
    }

    /// A pane that grew since the last keypress must not start past the end of the
    /// block and draw nothing.
    #[test]
    fn a_resize_pulls_the_scroll_back_into_range() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        app.open_glyphs();
        let map = app.glyphs.as_mut().unwrap();
        // Scroll to the bottom of a narrow pane, then lay the same block out wide.
        map.scroll_by(i32::MAX / 2, 4);
        let narrow = map.scroll_row();
        assert!(narrow > 0);
        map.clamp_scroll(40);
        let covered = map.selected().unwrap().codepoints.len();
        assert!(
            map.scroll_row() * 40 < covered,
            "a widened pane still starts inside the block"
        );
    }

    /// Flatten a pane's lines to plain text, the way a reader sees them.
    fn text_of(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_pane_gives_the_verdict_and_the_reason_for_it() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        let mut face = app.detail.clone().unwrap();
        let shown = text_of(&license_lines(&face));
        assert!(shown.contains("OFL-1.1"), "{shown}");
        // Not `contains("free")`: the label "freedom" contains it, so that would hold
        // even if the verdict were missing or said the opposite.
        assert!(
            shown.lines().any(|l| l.trim_end() == "freedom   free"),
            "the verdict is on its own line: {shown}"
        );
        assert!(
            !shown.contains("grants the freedom"),
            "\"free\" explains itself; the pane is shared with the preview: {shown}"
        );

        // A nonfree licence says so, and says why.
        face.license.spdx = Some("LicenseRef-Proprietary".into());
        let shown = text_of(&license_lines(&face));
        assert!(shown.contains("nonfree"), "{shown}");
        assert!(shown.contains("withholds"), "{shown}");

        // A font with nothing embedded is not silently called free.
        face.license.spdx = None;
        let shown = text_of(&license_lines(&face));
        assert!(shown.contains("none embedded"), "{shown}");
        assert!(shown.contains("unstated"), "{shown}");
        assert!(shown.contains("no permission"), "{shown}");
    }

    /// The pane sizes itself from wrapped rows, so a long value cannot push what comes
    /// after it off the bottom. Measured with the same arithmetic the layout uses.
    /// The paragraph wraps with `trim: false`, which keeps every space, so a gap costs
    /// columns. Counting words alone under-charged by the width of each one — and two
    /// lines in this pane are mostly gap: `kv` pads its label to ten columns and the
    /// licence reason is indented by ten.
    #[test]
    fn an_indent_costs_the_columns_it_occupies() {
        // Ten columns of indent plus twenty of text does not fit in twenty-five.
        let indented = Line::from(format!("{:<10}{}", "", "a b c d e f g h i j"));
        assert!(
            wrapped_rows(&indented, 25) > 1,
            "an indented line that overflows must be charged for the indent"
        );
        // The same words with no indent do fit.
        assert_eq!(wrapped_rows(&Line::from("a b c d e f g h i j"), 25), 1);

        // A padded label is charged its padding, not its text length.
        let padded = kv("file", "x".repeat(18));
        assert!(
            wrapped_rows(&padded, 25) > 1,
            "kv pads the label to ten columns, so this is 28 columns, not 22"
        );
    }

    #[test]
    fn a_long_value_does_not_cost_the_lines_below_it() {
        let long = kv("file", "/a/very/long/path/".repeat(6));
        let short = kv("style", "Regular".into());
        let rows = wrapped_rows;

        assert!(rows(&long, 30) > 1, "a long value wraps");
        assert_eq!(rows(&short, 30), 1);
        let counted: u16 = [&long, &short].iter().map(|l| rows(l, 30)).sum();
        assert!(
            counted > 2,
            "sizing by line count would have lost {} row(s)",
            counted - 2
        );
        assert_eq!(
            rows(&Line::from(""), 30),
            1,
            "an empty line still takes a row"
        );
    }

    #[test]
    fn restricted_embedding_is_shown_as_reported_not_enforced() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        let mut face = app.detail.clone().unwrap();
        // Installable is the ordinary case and says nothing.
        assert!(!text_of(&license_lines(&face)).contains("embedding"));

        let os2 = face.os2.as_mut().unwrap();
        os2.fs_type = 0x0002;
        os2.embedding = fontina_core::model::EmbeddingRights::from_fs_type(0x0002);
        let shown = text_of(&license_lines(&face));
        assert!(shown.contains("RestrictedLicense"), "{shown}");
        assert!(
            shown.contains("not enforced"),
            "the pane must not imply fontina obeys these bits: {shown}"
        );
    }

    #[test]
    fn the_freedom_facet_filters_the_listing() {
        let mut app = app();
        // Every fixture is OFL, so the whole library is free and the facet says so.
        let free = app
            .facets
            .freedom
            .iter()
            .find(|c| c.value == "free")
            .expect("a freedom facet");
        assert_eq!(
            free.count, app.facets.faces,
            "every fixture is OFL, so the whole library is free"
        );
        assert_eq!(
            app.facets.freedom.len(),
            1,
            "and nothing else is represented"
        );

        app.selected.insert(Facet::Freedom, "free".into());
        app.reload().unwrap();
        let with = app.list_len();
        assert!(with > 0, "the free fonts are still listed");
        assert_eq!(app.filter().freedom, Some(fontina_core::Freedom::Free));

        // And a state nothing is in empties the listing rather than being ignored.
        app.selected.insert(Facet::Freedom, "nonfree".into());
        app.reload().unwrap();
        assert_eq!(app.list_len(), 0, "no fixture is nonfree");
        assert_eq!(app.filter().freedom, Some(fontina_core::Freedom::Nonfree));
    }

    #[test]
    fn the_freedom_facet_reaches_the_command_line() {
        let mut app = app();
        app.selected.insert(Facet::Freedom, "free".into());
        app.reload().unwrap();
        assert!(
            app.command_line().contains("--freedom free"),
            "{}",
            app.command_line()
        );
    }

    #[test]
    fn a_waterfall_covers_one_face_and_a_comparison_the_whole_family() {
        let mut app = app();
        select_family(&mut app, "Bricolage");

        app.open_sheet(sheet::Kind::Waterfall).unwrap();
        let s = app.sheet.as_ref().expect("a face was selected");
        assert_eq!(s.kind(), sheet::Kind::Waterfall);
        let names: std::collections::BTreeSet<&str> = s
            .rows()
            .iter()
            .map(|r| r.face.names.family.as_str())
            .collect();
        assert_eq!(names.len(), 1, "a waterfall is one face at many sizes");
        assert!(app.status.contains("fontina specimen"), "{}", app.status);

        // Every key is swallowed while it is open, so nothing underneath can move.
        let before = app.detail_id;
        app.handle_sheet_key(KeyCode::Char('a')).unwrap();
        assert_eq!(app.detail_id, before);
        assert!(app.sheet.is_some());
        app.handle_sheet_key(KeyCode::Esc).unwrap();
        assert!(app.sheet.is_none());

        app.open_sheet(sheet::Kind::Compare).unwrap();
        let s = app.sheet.as_ref().unwrap();
        assert_eq!(s.kind(), sheet::Kind::Compare);
        assert_eq!(
            s.rows().len(),
            app.current_face_ids().len(),
            "a comparison covers everything the selection stands for"
        );
    }

    /// The face listing inside a family is exactly the view a reader presses `C` from,
    /// and `current_face_ids` narrows to the selected face there — which would compare a
    /// face with itself.
    #[test]
    fn comparing_inside_an_open_family_covers_the_family() {
        let mut app = app();
        select_family(&mut app, "Inter");
        app.open_family().unwrap();
        assert!(app.open_family.is_some(), "the family is open");
        let listed = app.faces.len();
        assert!(listed > 1, "this family has siblings to compare");
        assert_eq!(app.current_face_ids().len(), 1, "the selection is one face");

        app.open_sheet(sheet::Kind::Compare).unwrap();
        assert_eq!(
            app.sheet.as_ref().unwrap().rows().len(),
            listed,
            "C compares what the listing shows"
        );
    }

    #[test]
    fn only_a_comparison_answers_the_size_keys() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        app.open_sheet(sheet::Kind::Waterfall).unwrap();
        let sizes: Vec<f32> = app
            .sheet
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .map(|r| r.size)
            .collect();
        app.handle_sheet_key(KeyCode::Char('+')).unwrap();
        let after: Vec<f32> = app
            .sheet
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .map(|r| r.size)
            .collect();
        assert_eq!(sizes, after, "a waterfall's ladder is fixed");

        app.sheet = None;
        app.open_sheet(sheet::Kind::Compare).unwrap();
        let before = app.sheet.as_ref().unwrap().size();
        app.handle_sheet_key(KeyCode::Char('+')).unwrap();
        assert!(app.sheet.as_ref().unwrap().size() > before);
    }

    /// One rendering per row, held between frames: scrolling a waterfall must not
    /// re-render nine bitmaps on every keypress.
    #[test]
    fn the_preview_cache_holds_a_rendering_for_every_row() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        let face = app.detail.clone().unwrap();
        let opts = |size: f32| RenderOptions {
            text: "Ag".into(),
            size,
            padding: 1,
            max_width: Some(60),
            ..Default::default()
        };
        for size in fontina_core::typography::WATERFALL_SIZES {
            app.preview.lines(&face, &opts(*size), 40);
        }
        assert_eq!(
            app.preview.len(),
            fontina_core::typography::WATERFALL_SIZES.len(),
            "every size kept its rendering"
        );
        // And asking again returns them without rendering: the count does not grow.
        for size in fontina_core::typography::WATERFALL_SIZES {
            app.preview.lines(&face, &opts(*size), 40);
        }
        assert_eq!(
            app.preview.len(),
            fontina_core::typography::WATERFALL_SIZES.len()
        );
    }

    #[test]
    fn searching_the_map_reports_what_it_found_or_did_not() {
        let mut app = app();
        select_family(&mut app, "Amiri");
        app.open_glyphs();

        app.start_input(InputKind::Glyph, String::new());
        for c in "U+0041".chars() {
            app.handle_input_key(KeyCode::Char(c)).unwrap();
        }
        app.handle_input_key(KeyCode::Enter).unwrap();
        assert!(app.status.starts_with("U+0041 in "), "{}", app.status);
        assert_eq!(app.glyphs.as_ref().unwrap().found(), Some(0x41));

        app.start_input(InputKind::Glyph, String::new());
        for c in "Tibetan".chars() {
            app.handle_input_key(KeyCode::Char(c)).unwrap();
        }
        app.handle_input_key(KeyCode::Enter).unwrap();
        assert!(app.status.contains("nothing covered"), "{}", app.status);
    }

    #[test]
    fn the_render_options_carry_what_the_reader_set() {
        let mut app = app();
        select_family(&mut app, "Bricolage");
        app.focus = Focus::Controls;
        app.controls.adjust(-3);
        let opts = app.render_options("Ag".into(), 80);
        assert_eq!(opts.variations, app.controls.variations());
        assert!(!opts.variations.is_empty());
        assert_eq!(opts.features, app.controls.forced_features());
        // The cache key is the options, so a moved axis is a different key.
        let before = opts.clone();
        app.controls.adjust(-1);
        assert_ne!(app.render_options("Ag".into(), 80), before);
    }

    // ----- frames -----
    //
    // ratatui's `TestBackend` draws into a buffer instead of a terminal, so the frames
    // a reader meets can be looked at. Two things in a frame are not the browser's
    // doing and would make a snapshot say more about this machine than about the code:
    // the absolute path of the fixtures, which the details pane prints in full, and the
    // shaped preview, whose pixels belong to the rasteriser and would move under a
    // skrifa release. Both are pinned for the snapshots, and the preview has its own
    // test below.

    /// The browser drawn into an in-memory terminal, as the text a reader would see.
    fn frame(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The same frame, made to depend on nothing but the browser: the sample text is a
    /// single space, so the preview is blank rather than a picture of whichever
    /// rasteriser is installed, and the file of the face on show is named relative to
    /// the crate rather than by the absolute path it was scanned under. The relative
    /// name still opens — the preview reads the file — because cargo runs a test with
    /// the package root as its working directory.
    fn stable_frame(app: &mut App, width: u16, height: u16) -> String {
        app.preview_text = Some(" ".into());
        if let Some(face) = app.detail.as_mut() {
            let name = Path::new(&face.file.path)
                .file_name()
                .expect("a face is a file")
                .to_string_lossy()
                .into_owned();
            face.file.path = format!("../../fixtures/{name}");
        }
        frame(app, width, height)
    }

    /// The status line on its own, which is the row that tells a reader what the rest
    /// of the screen is.
    fn status_line(app: &App, width: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 1)).unwrap();
        terminal.draw(|f| app.draw_status(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Where a substring starts, counted in columns rather than bytes: the frame is
    /// full of box drawing, and one of those is three bytes wide and one column.
    fn column_of(row: &str, needle: &str) -> Option<usize> {
        row.find(needle).map(|b| row[..b].chars().count())
    }

    #[test]
    fn the_browser_opens_on_the_family_list() {
        let mut app = app();
        let drawn = stable_frame(&mut app, 120, 36);
        assert!(drawn.contains("5 families"), "{drawn}");
        assert!(
            drawn.lines().next_back().unwrap().starts_with(" / search"),
            "the key line is the last row on the screen"
        );
        insta::assert_snapshot!(drawn);
    }

    /// The view the browser existed five milestones without: every family on show,
    /// each one setting its own name in its own face.
    #[test]
    fn the_specimen_sheet_sets_every_family_in_its_own_face() {
        let mut app = app();
        app.open_sheet(sheet::Kind::Specimen).unwrap();

        let sheet = app.sheet.as_ref().expect("P opens a sheet");
        assert_eq!(sheet.kind(), sheet::Kind::Specimen);
        assert_eq!(
            sheet.rows().len(),
            app.families.len(),
            "one row per family on show, not one per face"
        );

        // The words in each row are that row's own family name. This is the whole
        // feature: a comparison sheet would set every row in the same pangram.
        for row in sheet.rows() {
            assert_eq!(
                sheet.text_for(row, None),
                row.face.names.family,
                "a specimen row is set in the words of its own name"
            );
        }

        let drawn = stable_frame(&mut app, 120, 36);
        assert!(drawn.contains("specimen"), "{drawn}");
        insta::assert_snapshot!(drawn);
    }

    /// The snapshot above cannot prove this one. `stable_frame` sets the sample text to
    /// a single space on purpose, so that a snapshot never depends on a rasteriser —
    /// which means every row in it is blank by design, and a specimen sheet that drew
    /// nothing at all would produce exactly the same file.
    ///
    /// So this asserts the thing the feature is: that the family's name, set in the
    /// family's own face, puts ink on the screen.
    #[test]
    fn a_specimen_row_actually_draws_the_name() {
        let mut app = app();
        app.open_sheet(sheet::Kind::Specimen).unwrap();
        let sheet = app.sheet.as_ref().unwrap();
        let row = sheet
            .rows()
            .iter()
            .find(|r| r.face.names.family == "Inter")
            .expect("Inter is among the fixtures");

        let words = sheet.text_for(row, None);
        assert_eq!(words, "Inter");
        let opts = sheet.options(row, words, 100);

        let mut cache = preview::Cache::default();
        let lines = cache.lines(&row.face, &opts, (row.size.ceil() as u32 * 2).max(2));
        let ink: usize = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.chars().filter(|c| *c == '▀').count())
            .sum();
        assert!(
            ink > 0,
            "a specimen row must draw the name; got {} lines and no ink",
            lines.len()
        );
    }

    /// The sample text applies to a comparison and a waterfall, and must not to this.
    /// A specimen whose rows all say the same thing is a comparison, so `e` then `P`
    /// would have quietly deleted the whole feature.
    ///
    /// The first version of this test asserted the opposite and passed: it checked that
    /// `text_for` honours a chosen string, and its comment claimed the browser declined
    /// to pass one. The browser passed it. The words live on the row now, so the
    /// override is impossible rather than merely unintended.
    #[test]
    fn a_sample_text_cannot_turn_a_specimen_into_a_comparison() {
        let mut app = app();
        app.preview_text = Some("Hamburgefonstiv".into());
        app.open_sheet(sheet::Kind::Specimen).unwrap();
        let sheet = app.sheet.as_ref().unwrap();
        for row in sheet.rows() {
            assert_eq!(
                sheet.text_for(row, Some("Hamburgefonstiv")),
                row.face.names.family,
                "a specimen row keeps its own name even when a sample text is set"
            );
        }
    }

    /// `reload` clears the family list while a family is open, so collecting
    /// representatives there returned nothing and `P` reported "no families on show"
    /// against a full index. `Compare` has an arm for exactly this; this one did not.
    #[test]
    fn the_specimen_key_works_inside_an_open_family() {
        let mut app = app();
        select_family(&mut app, "Inter");
        app.open_family().unwrap();
        app.open_sheet(sheet::Kind::Specimen).unwrap();

        let sheet = app.sheet.as_ref().expect("P works inside a family");
        assert_eq!(sheet.rows().len(), app.faces.len());
        // Within one family every row shares a family name, so the words carry the
        // style too, or every row would set the same word.
        for row in sheet.rows() {
            let words = sheet.text_for(row, None);
            assert!(words.starts_with("Inter"), "{words}");
            assert!(
                words.len() > "Inter".len(),
                "the style distinguishes the row"
            );
        }
    }

    /// The title and the help both offered `+/-`, and `resize` refused anything that was
    /// not a comparison, so the promise was silent and false.
    #[test]
    fn a_specimen_resizes_like_a_comparison() {
        let mut app = app();
        app.open_sheet(sheet::Kind::Specimen).unwrap();
        let sheet = app.sheet.as_mut().unwrap();
        let before = sheet.size();
        assert!(sheet.resize(4.0), "a specimen resizes");
        assert!(sheet.size() > before);
    }

    #[test]
    fn opening_a_family_lists_its_faces_and_says_so_in_the_command() {
        let mut app = app();
        select_family(&mut app, "Inter");
        app.open_family().unwrap();
        assert_eq!(app.open_family.as_deref(), Some("Inter"));
        insta::assert_snapshot!(stable_frame(&mut app, 120, 36));
    }

    #[test]
    fn the_help_overlay_sits_over_the_browser() {
        let mut app = app();
        app.help = true;
        let drawn = stable_frame(&mut app, 120, 36);
        assert!(
            drawn.contains("any key to close"),
            "the overlay does not say how to leave it"
        );
        insta::assert_snapshot!(drawn);
    }

    #[test]
    fn the_status_line_says_what_the_screen_is() {
        let mut app = app();
        let mut rows = vec![status_line(&app, 100)];
        assert_eq!(rows[0].trim(), "$ fontina families");

        // A filter is a flag, and the line is a command that can be pasted.
        app.selected.insert(Facet::Variable, "variable".into());
        app.selected.insert(Facet::Vendor, "ATLR".into());
        app.reload().unwrap();
        rows.push(status_line(&app, 100));

        // While something is being typed, the line is the prompt instead.
        app.start_input(InputKind::Search, "grot".into());
        rows.push(status_line(&app, 100));

        // And after an action, what the action did, until the next reload.
        app.input = None;
        app.status = "tagged 1 face as favourite".into();
        rows.push(status_line(&app, 100));

        insta::assert_snapshot!(rows.join("\n"));
    }

    #[test]
    fn the_preview_is_shaped_ink_and_it_stays_inside_the_details_pane() {
        let mut app = app();
        let drawn = frame(&mut app, 120, 40);
        let rows: Vec<&str> = drawn.lines().collect();
        let pane = column_of(rows[0], "┌ Details").expect("the details pane is titled");
        assert!(
            drawn.contains('▀'),
            "the details pane drew no preview at all:\n{drawn}"
        );
        for (y, row) in rows.iter().enumerate() {
            if let Some(x) = column_of(row, "▀") {
                assert!(
                    x > pane,
                    "row {y} has preview ink at column {x}, outside the details pane at {pane}"
                );
            }
        }
        // And it is the sample text that puts it there, which is what lets the frames
        // above be snapshotted without a rasteriser in them.
        app.preview_text = Some(" ".into());
        assert!(
            !frame(&mut app, 120, 40).contains('▀'),
            "a blank sample text still drew ink"
        );
    }

    /// A defect this change reports rather than fixes.
    ///
    /// The preview is rendered at a fixed size and then clipped to the rows the pane
    /// has, from the top. A face with many features gives its controls the room, and
    /// what is left of the details pane is a few rows — into which the preview puts the
    /// first few pixel rows of the rendering. Those rows are the font's ascent, which
    /// is empty, so a reader on a 36-row terminal sees a blank space where the type
    /// should be. Fitting the rendering to the rows it has, or clipping around the
    /// baseline rather than the top, would fix it; when it is fixed this is the test
    /// that should change.
    #[test]
    fn a_details_pane_squeezed_by_controls_still_draws_the_preview() {
        // The rendering is clipped to the ink rather than to its top row. The top of a
        // rendering is the font's empty ascent — Source Serif at 28 px is 41 pixels tall
        // with nothing above row 9 — so clipping from row zero showed a pane of blank on
        // any terminal short enough that its feature controls crowded the preview.
        let mut app = app();
        select_family(&mut app, "Source Serif");
        assert!(
            app.controls.len() > 10,
            "the face was picked because its features crowd the pane"
        );
        for height in [36, 44] {
            assert!(
                frame(&mut app, 120, height).contains('▀'),
                "no preview drawn on a {height}-row terminal"
            );
        }
    }

    #[test]
    fn a_standard_eighty_column_terminal_still_shows_every_pane() {
        let mut app = app();
        let drawn = stable_frame(&mut app, 80, 24);
        assert_eq!(drawn.lines().count(), 24, "one row per terminal line");
        for (n, row) in drawn.lines().enumerate() {
            assert!(
                row.chars().count() <= 80,
                "row {n} is {} columns wide on an 80-column terminal",
                row.chars().count()
            );
        }
        assert!(
            drawn.contains("$ fontina families"),
            "the status line still says what the screen is"
        );
        assert!(
            drawn.contains(".ttf") || drawn.contains(".otf"),
            "the file name keeps its extension: the text block is sized by wrapped rows"
        );
        insta::assert_snapshot!(drawn);
    }
}
