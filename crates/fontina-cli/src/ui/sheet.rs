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

//! The same words, rendered more than once, down a scrolling sheet.
//!
//! A waterfall is one face at every size in the ladder; a comparison is several faces at
//! one size. They differ only in how the rows are filled, so they are one mode: the
//! scrolling, the labels and the drawing are shared, and a fix to either is a fix to
//! both.

use fontina_core::model::FaceMetadata;
use fontina_core::render::RenderOptions;
use fontina_core::typography;
use ratatui::text::Line;

/// The SPDX identifier, or a word saying there is not one. Shown beside a family name
/// in the specimen sheet because "may I use this" is the question that follows "do I
/// like this", and answering it here saves opening the face to find out.
/// What a specimen row sets: the family, or the face within an open family, and never
/// the empty string.
///
/// `parse::names` defaults a missing family to `""`, so a font carrying neither a
/// typographic nor a legacy family name renders a blank row under a blank label — the
/// same failure the waterfall arm guards against, where a screen of nothing cannot be
/// told from a rendering that failed.
fn naming(face: &FaceMetadata, by_face: bool) -> String {
    let family = face.names.family.trim();
    let base = if family.is_empty() {
        face.names
            .full_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(face.names.postscript_name.as_deref())
            .unwrap_or("(unnamed)")
    } else {
        family
    };
    let style = face.names.subfamily.trim();
    if by_face && !style.is_empty() {
        format!("{base} {style}")
    } else {
        base.to_string()
    }
}

fn licence_of(face: &FaceMetadata) -> String {
    face.license
        .spdx
        .clone()
        .unwrap_or_else(|| "no licence stated".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Waterfall,
    Compare,
    /// One row per family, each set in the face that family names.
    Specimen,
}

/// One rendering in the sheet: which face, how big, how it is set, and what to call it.
///
/// The metadata is held rather than the id. A sheet is drawn on every frame, and reading
/// a face back from the index per row per frame is a query and a full JSON parse each
/// time — the mistake #36 fixed for the details pane.
pub struct Row {
    pub face: FaceMetadata,
    pub label: String,
    pub size: f32,
    /// Axis positions and forced features, captured when the sheet was opened. The sheet
    /// is modal, so nothing can change them while it is up.
    pub variations: Vec<(String, f32)>,
    pub features: Vec<(String, bool)>,
    /// The words this row sets, when they belong to the row rather than to the sheet.
    ///
    /// A waterfall and a comparison share one string, and the reader may replace it
    /// with `e`. A specimen cannot: its whole claim is that each row is set in its own
    /// name, and a sample text applied across it turns it back into the comparison it
    /// exists not to be. Holding the words on the row is what makes that impossible
    /// rather than merely intended.
    pub words: Option<String>,
}

/// A sheet laid out for one pane width and one sample text.
struct Built {
    width: u16,
    text: Option<String>,
    lines: Vec<Line<'static>>,
}

pub struct Sheet {
    kind: Kind,
    rows: Vec<Row>,
    /// First terminal line on show. The rows have wildly different heights — a 96 px
    /// row is nearly fifty terminal lines and a 10 px row is five — so scrolling counts
    /// lines rather than rows, or a single keypress would jump a screenful.
    scroll: usize,
    /// The rendered sheet, kept until the pane width or the sample text changes.
    /// Rebuilding is nine rasterisations for a waterfall and one per face for a
    /// comparison, which is fine once and ruinous on every frame.
    built: Option<Built>,
}

impl Sheet {
    /// One face at every size in the ladder, set the way the reader left it.
    ///
    /// The controls carry through here and nowhere else: an `opsz` axis walked down a
    /// size ladder is most of what a waterfall is for, and a waterfall is one face, so
    /// the settings mean something for every row.
    pub fn waterfall(
        face: FaceMetadata,
        variations: Vec<(String, f32)>,
        features: Vec<(String, bool)>,
    ) -> Self {
        Sheet {
            kind: Kind::Waterfall,
            rows: typography::WATERFALL_SIZES
                .iter()
                .map(|&size| Row {
                    face: face.clone(),
                    label: format!("{size:.0} px"),
                    size,
                    variations: variations.clone(),
                    features: features.clone(),
                    words: None,
                })
                .collect(),
            scroll: 0,
            built: None,
        }
    }

    /// One row per family, each row setting that family's own name in its own face.
    ///
    /// This is the view the browser was missing. Every other list in the tool names a
    /// typeface in the terminal's face, which tells a reader everything except the one
    /// thing they opened a font manager to find out. Here the name of the font is drawn
    /// by the font.
    ///
    /// It is a sheet rather than a column because of arithmetic rather than taste. A
    /// half-block cell is one pixel wide and two tall, so a list pane twenty-four
    /// columns across is a twenty-four pixel canvas, and no type is legible in that. The
    /// full frame is a hundred and forty, which is a readable line.
    pub fn specimen(faces: Vec<FaceMetadata>, size: f32, by_face: bool) -> Self {
        Sheet {
            kind: Kind::Specimen,
            rows: faces
                .into_iter()
                .map(|face| Row {
                    label: format!("{}  {}", naming(&face, by_face), licence_of(&face)),
                    words: Some(naming(&face, by_face)),
                    face,
                    size,
                    variations: Vec::new(),
                    features: Vec::new(),
                })
                .collect(),
            scroll: 0,
            built: None,
        }
    }

    /// Several faces at one size, in the order the listing had them.
    ///
    /// Deliberately unset: the controls describe the one face the details pane was
    /// showing, and applying its `wght` to every other face in the family would be
    /// meaningless where the axis exists and a lie where it does not.
    pub fn compare(faces: Vec<FaceMetadata>, size: f32) -> Self {
        Sheet {
            kind: Kind::Compare,
            rows: faces
                .into_iter()
                .map(|face| Row {
                    label: format!("{} {}", face.names.family, face.names.subfamily),
                    face,
                    size,
                    variations: Vec::new(),
                    features: Vec::new(),
                    words: None,
                })
                .collect(),
            scroll: 0,
            built: None,
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn scroll_row(&self) -> usize {
        self.scroll
    }

    /// Terminal lines the built sheet occupies, or none until it has been laid out.
    pub fn lines(&self) -> usize {
        self.built.as_ref().map(|b| b.lines.len()).unwrap_or(0)
    }

    /// Whether the sheet already holds a rendering for this width and sample text.
    pub fn is_built_for(&self, width: u16, text: Option<&str>) -> bool {
        self.built
            .as_ref()
            .is_some_and(|b| b.width == width && b.text.as_deref() == text)
    }

    /// Keep a laid-out sheet. Anything that changes what it should look like — a resize,
    /// new sample text, a different size — drops it and it is built again.
    pub fn set_built(&mut self, width: u16, text: Option<String>, lines: Vec<Line<'static>>) {
        self.built = Some(Built { width, text, lines });
    }

    /// The window of lines to draw, given how many rows the pane can show.
    pub fn window(&self, visible: usize) -> Vec<Line<'static>> {
        let Some(built) = &self.built else {
            return Vec::new();
        };
        built
            .lines
            .iter()
            .skip(self.scroll)
            .take(visible)
            .cloned()
            .collect()
    }

    /// Scroll by whole terminal lines, never past the last screenful.
    pub fn scroll_by(&mut self, delta: i32, visible: usize) {
        let last = self.lines().saturating_sub(visible.max(1));
        self.scroll = (self.scroll as i32 + delta).clamp(0, last as i32) as usize;
    }

    /// The words a row is set in.
    ///
    /// A comparison holds the string constant and varies the face — that is what makes
    /// it a comparison — so every row gets the same text, taken from the first face when
    /// the reader has not chosen any. A waterfall is one face, so it can fall back to
    /// that face's own embedded sample string.
    pub fn text_for(&self, row: &Row, chosen: Option<&str>) -> String {
        // A row that owns its words keeps them. This is what stops `e` turning a
        // specimen into a comparison of one string.
        if let Some(words) = &row.words {
            return words.clone();
        }
        if let Some(text) = chosen {
            return text.to_string();
        }
        match self.kind {
            Kind::Compare => self
                .rows
                .first()
                .map(|first| typography::preview_text(&first.face).to_string())
                .unwrap_or_default(),
            // A waterfall is one face, so it may use that face's own sample string —
            // but only if there is something in it. A `name` table is free to hold a
            // string of spaces, and a waterfall set in spaces is a screen of nothing
            // with no way to tell it from a rendering that failed. Until now the only
            // thing standing between a reader and that screen was `parse::english`
            // dropping a name that trims empty, which is a coupling between two crates
            // that nothing declared.
            // Unreachable in practice: every specimen row carries its own words and
            // returned above. Kept total rather than clever.
            Kind::Specimen => naming(&row.face, false),
            Kind::Waterfall => row
                .face
                .names
                .sample_text
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| typography::preview_text(&row.face).to_string()),
        }
    }

    /// How each row should be rendered, at the pane width being drawn.
    pub fn options(&self, row: &Row, text: String, width: u32) -> RenderOptions {
        RenderOptions {
            text,
            size: row.size,
            variations: row.variations.clone(),
            features: row.features.clone(),
            padding: 1,
            max_width: Some(width),
        }
    }

    /// Change the size every row is rendered at. Only a comparison has one size to
    /// change; a waterfall's sizes are the point of it.
    pub fn resize(&mut self, delta: f32) -> bool {
        // A specimen has one uniform size, exactly like a comparison, so it resizes for
        // the same reason. The title and the help both promised this before the code
        // allowed it.
        if !matches!(self.kind, Kind::Compare | Kind::Specimen) {
            return false;
        }
        let mut changed = false;
        for row in &mut self.rows {
            let next = (row.size + delta).clamp(8.0, 160.0);
            changed |= next != row.size;
            row.size = next;
        }
        if changed {
            self.scroll = 0;
            self.built = None;
        }
        changed
    }

    /// The size a comparison is set to, for its title.
    pub fn size(&self) -> f32 {
        self.rows.first().map(|r| r.size).unwrap_or(0.0)
    }

    pub fn title(&self) -> String {
        match self.kind {
            Kind::Waterfall => format!("waterfall — {} sizes", self.rows.len()),
            Kind::Compare => format!(
                "compare — {} face(s) at {:.0} px, +/- to resize",
                self.rows.len(),
                self.size()
            ),
            Kind::Specimen => format!(
                "specimen — {} famil{} at {:.0} px, +/- to resize",
                self.rows.len(),
                if self.rows.len() == 1 { "y" } else { "ies" },
                self.size()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn face(name: &str) -> FaceMetadata {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        fontina_core::load_file(&path).unwrap().1.remove(0)
    }

    fn waterfall() -> Sheet {
        Sheet::waterfall(face("Amiri-Regular.ttf"), Vec::new(), Vec::new())
    }

    #[test]
    fn a_waterfall_is_one_face_at_every_size_in_the_ladder() {
        let s = waterfall();
        assert_eq!(s.kind(), Kind::Waterfall);
        assert_eq!(s.rows().len(), typography::WATERFALL_SIZES.len());
        let names: std::collections::BTreeSet<&str> = s
            .rows()
            .iter()
            .map(|r| r.face.names.family.as_str())
            .collect();
        assert_eq!(names.len(), 1, "one face throughout");
        let sizes: Vec<f32> = s.rows().iter().map(|r| r.size).collect();
        assert_eq!(sizes, typography::WATERFALL_SIZES);
        assert_eq!(s.rows()[0].label, "10 px");
    }

    #[test]
    fn a_waterfall_carries_the_settings_it_was_opened_with() {
        let variations = vec![("wght".to_string(), 700.0)];
        let features = vec![("smcp".to_string(), true)];
        let s = Sheet::waterfall(
            face("Amiri-Regular.ttf"),
            variations.clone(),
            features.clone(),
        );
        assert!(
            s.rows()
                .iter()
                .all(|r| r.variations == variations && r.features == features),
            "every size shows the face as the reader set it"
        );
        let opts = s.options(&s.rows()[0], "Ag".into(), 80);
        assert_eq!(opts.variations, variations);
        assert_eq!(opts.features, features);
    }

    #[test]
    fn a_comparison_is_every_face_at_one_size_and_carries_no_settings() {
        let faces = vec![
            face("Amiri-Regular.ttf"),
            face("SourceSerif4-Regular.otf"),
            face("BricolageGrotesque[opsz,wdth,wght].ttf"),
        ];
        let s = Sheet::compare(faces, 32.0);
        assert_eq!(s.kind(), Kind::Compare);
        assert_eq!(s.rows().len(), 3);
        assert!(
            s.rows().iter().all(|r| r.size == 32.0),
            "one size throughout"
        );
        assert!(
            s.rows()
                .iter()
                .all(|r| r.variations.is_empty() && r.features.is_empty()),
            "one face's axes mean nothing on another's row"
        );
        assert!(s.rows()[0].label.starts_with("Amiri"));
    }

    #[test]
    fn only_a_comparison_can_be_resized() {
        let mut w = waterfall();
        assert!(!w.resize(8.0), "a waterfall's sizes are the point of it");
        assert_eq!(w.rows()[0].size, typography::WATERFALL_SIZES[0]);

        let mut c = Sheet::compare(vec![face("Amiri-Regular.ttf")], 32.0);
        assert!(c.resize(8.0));
        assert_eq!(c.size(), 40.0);
        for _ in 0..100 {
            c.resize(8.0);
        }
        assert_eq!(c.size(), 160.0);
        assert!(!c.resize(8.0), "already at the largest size");
        for _ in 0..100 {
            c.resize(-8.0);
        }
        assert_eq!(c.size(), 8.0);
        assert!(!c.resize(-8.0), "already at the smallest size");
    }

    #[test]
    fn scrolling_stops_at_the_last_screenful() {
        let mut s = waterfall();
        s.set_built(80, None, vec![Line::from("x"); 200]);
        assert_eq!(s.lines(), 200);
        s.scroll_by(-10, 40);
        assert_eq!(s.scroll_row(), 0);
        s.scroll_by(10_000, 40);
        assert_eq!(s.scroll_row(), 160, "the last screenful, not past the end");
        assert_eq!(s.window(40).len(), 40);

        // A pane taller than the sheet cannot scroll at all.
        s.set_built(80, None, vec![Line::from("x"); 20]);
        s.scroll_by(10_000, 40);
        assert_eq!(s.scroll_row(), 0);
        assert_eq!(s.window(40).len(), 20, "the window never invents lines");
    }

    #[test]
    fn a_comparison_sets_every_row_in_the_same_words() {
        let faces = vec![
            face("Amiri-Regular.ttf"),
            face("SourceSerif4-Regular.otf"),
            face("BricolageGrotesque[opsz,wdth,wght].ttf"),
        ];
        let c = Sheet::compare(faces, 32.0);
        let texts: std::collections::BTreeSet<String> =
            c.rows().iter().map(|r| c.text_for(r, None)).collect();
        assert_eq!(
            texts.len(),
            1,
            "varying the words as well as the face compares nothing"
        );
        assert!(c.rows().iter().all(|r| c.text_for(r, Some("Ag")) == "Ag"));
    }

    #[test]
    fn a_waterfall_may_use_the_faces_own_sample_string() {
        let mut f = face("Amiri-Regular.ttf");
        f.names.sample_text = Some("A specimen".into());
        let w = Sheet::waterfall(f, Vec::new(), Vec::new());
        assert_eq!(w.text_for(&w.rows()[0], None), "A specimen");
        assert_eq!(w.text_for(&w.rows()[0], Some("Ag")), "Ag");
    }

    /// Anything that changes what the sheet should look like drops the rendering, and a
    /// sheet that is already laid out for this width and text is not rendered again.
    #[test]
    fn a_built_sheet_is_reused_until_something_changes() {
        let mut c = Sheet::compare(vec![face("Amiri-Regular.ttf")], 32.0);
        assert!(!c.is_built_for(80, None), "nothing rendered yet");
        c.set_built(80, None, vec![Line::from("x"); 50]);
        assert!(c.is_built_for(80, None));
        assert!(!c.is_built_for(100, None), "a resized pane is a new layout");
        assert!(
            !c.is_built_for(80, Some("Ag")),
            "new words are a new layout"
        );

        c.scroll_by(10, 10);
        assert!(c.scroll_row() > 0);
        c.resize(8.0);
        assert!(
            !c.is_built_for(80, None),
            "a resized sheet must be rendered again"
        );
        assert_eq!(c.scroll_row(), 0, "and starts from the top");
        assert_eq!(c.lines(), 0);
    }

    // ----- scrolling -----
    //
    // A sheet is a column of terminal lines taller than the pane, in a terminal whose
    // height the reader changes at will. The pane can be one line tall, or shorter than
    // a single row of the sheet, or taller than the whole of it.

    /// The deltas `ui::mod` sends for j, k, Home and End. PageDown and PageUp send
    /// `visible - 1`, which depends on the pane and is applied where it is tested.
    const SCROLL_KEYS: [i32; 4] = [1, -1, i32::MIN / 2, i32::MAX / 2];

    /// Pane heights in terminal lines: none, one, two, a usual pane, a tall one.
    const VISIBLE: [usize; 6] = [0, 1, 2, 3, 40, 4096];

    /// A waterfall already laid out to `lines` terminal lines. What is on them does not
    /// matter to the scrolling; how many there are is the whole of it.
    fn sheet_of(lines: usize) -> Sheet {
        let mut s = waterfall();
        s.set_built(80, None, vec![Line::from("x"); lines]);
        s
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The window is what the pane draws, so the two invariants are the same one: the
    /// scroll never passes the last screenful, and the window never runs off the end.
    #[test]
    fn no_pane_height_scrolls_the_sheet_past_its_last_screenful() {
        for lines in [0usize, 1, 2, 19, 20, 21, 200, 1000] {
            for visible in VISIBLE {
                let mut s = sheet_of(lines);
                for key in SCROLL_KEYS {
                    s.scroll_by(key, visible);
                    assert!(
                        s.scroll_row() <= lines.saturating_sub(visible.max(1)),
                        "{lines} lines in a {visible}-line pane scrolled to {}",
                        s.scroll_row()
                    );
                    assert_eq!(
                        s.window(visible).len(),
                        visible.min(lines - s.scroll_row()),
                        "the window neither invents lines nor hides them"
                    );
                }
                // End leaves the last line of the sheet on the last line of the pane.
                s.scroll_by(i32::MAX / 2, visible);
                if visible > 0 && visible <= lines {
                    assert_eq!(s.scroll_row() + visible, lines);
                }
            }
        }
    }

    /// Paging down until it stops and then up the same number of times lands back at
    /// the top: the step is the same in both directions, and the clamp at each end
    /// swallows the overshoot rather than losing a screenful to it.
    #[test]
    fn paging_to_the_end_and_back_lands_where_it_started() {
        for lines in [0usize, 1, 2, 19, 20, 21, 200, 1000] {
            for visible in [2usize, 3, 40, 4096] {
                let mut s = sheet_of(lines);
                let step = visible as i32 - 1;
                let mut pages = 0;
                loop {
                    let before = s.scroll_row();
                    s.scroll_by(step, visible);
                    if s.scroll_row() == before {
                        break;
                    }
                    pages += 1;
                    assert!(pages < 100_000, "paging never reached the end");
                }
                assert_eq!(s.scroll_row(), lines.saturating_sub(visible));
                for _ in 0..pages {
                    s.scroll_by(-step, visible);
                }
                assert_eq!(
                    s.scroll_row(),
                    0,
                    "{lines} lines paged down {pages} time(s) in a {visible}-line pane \
                     and back up as many"
                );
            }
        }
        // Home and End are the same round trip in one keypress each.
        let mut s = sheet_of(200);
        s.scroll_by(i32::MAX / 2, 40);
        assert_eq!(s.scroll_row(), 160);
        s.scroll_by(i32::MIN / 2, 40);
        assert_eq!(s.scroll_row(), 0);
        s.scroll_by(i32::MAX / 2, 40);
        assert_eq!(s.scroll_row(), 160);
    }

    /// A delta of zero moves nothing, which is this type's answer and the right one.
    ///
    /// It used to be reachable: `ui::mod` sent `visible - 1` for a page, so in a pane
    /// one line tall PageDown, PageUp and Space were dead keys. The caller now sends at
    /// least one line; `a_page_in_a_one_line_pane_still_moves` in `ui::mod` holds that.
    #[test]
    fn a_delta_of_zero_scrolls_nowhere() {
        let mut s = sheet_of(200);
        s.scroll_by(0, 1);
        assert_eq!(
            s.scroll_row(),
            0,
            "a page of no lines moves by none of them"
        );
        // The other keys still work there.
        s.scroll_by(1, 1);
        assert_eq!(s.scroll_row(), 1);
        s.scroll_by(i32::MAX / 2, 1);
        assert_eq!(s.scroll_row(), 199);
        assert_eq!(s.window(1).len(), 1);
    }

    #[test]
    fn a_sheet_that_has_not_been_laid_out_draws_nothing_and_scrolls_nowhere() {
        let mut s = waterfall();
        assert_eq!(s.lines(), 0);
        assert!(!s.is_built_for(80, None));
        assert!(s.window(40).is_empty());
        for visible in VISIBLE {
            for key in SCROLL_KEYS {
                s.scroll_by(key, visible);
                assert_eq!(s.scroll_row(), 0);
            }
        }
    }

    /// `set_built` keeps a rendering; it does not touch the scroll. A sheet laid out
    /// again shorter than the one the reader had scrolled through therefore starts past
    /// its own end and draws blank, which is why `draw_sheet` clamps on every frame
    /// after building. Take that line out and this is the test that says what breaks.
    #[test]
    fn a_sheet_laid_out_shorter_needs_the_clamp_the_drawing_does() {
        let mut s = sheet_of(200);
        s.scroll_by(i32::MAX / 2, 20);
        assert_eq!(s.scroll_row(), 180);
        s.set_built(40, None, vec![Line::from("x"); 30]);
        assert_eq!(
            s.scroll_row(),
            180,
            "laying out again does not move the scroll"
        );
        assert!(
            s.window(20).is_empty(),
            "and until it is clamped, it is blank"
        );
        s.scroll_by(0, 20);
        assert_eq!(s.scroll_row(), 10, "the last screenful of the new layout");
        assert_eq!(s.window(20).len(), 20);
    }

    #[test]
    fn the_window_starts_at_the_scroll_and_stops_at_the_last_line() {
        let mut s = waterfall();
        s.set_built(
            80,
            None,
            (0..200).map(|i| Line::from(i.to_string())).collect(),
        );
        s.scroll_by(37, 20);
        let window = s.window(20);
        assert_eq!(window.len(), 20);
        assert_eq!(text_of(&window[0]), "37");
        assert_eq!(text_of(&window[19]), "56");
        s.scroll_by(i32::MAX / 2, 20);
        let window = s.window(20);
        assert_eq!(text_of(&window[19]), "199", "the last line is reachable");
        // A pane taller than the sheet is pulled back to the top and shows all of it.
        s.scroll_by(0, 4096);
        assert_eq!(s.scroll_row(), 0);
        assert_eq!(s.window(4096).len(), 200);
    }

    // ----- sheets at the extremes -----

    #[test]
    fn a_comparison_holds_from_one_face_to_a_hundred() {
        let f = face("Amiri-Regular.ttf");
        for n in [1usize, 2, 5, 100] {
            let s = Sheet::compare(vec![f.clone(); n], 32.0);
            assert_eq!(s.rows().len(), n);
            assert!(s.title().contains(&format!("{n} face(s)")), "{}", s.title());
            assert!(s.rows().iter().all(|r| r.size == 32.0));
            let texts: std::collections::BTreeSet<String> =
                s.rows().iter().map(|r| s.text_for(r, None)).collect();
            assert_eq!(texts.len(), 1, "one comparison, one set of words");
        }
    }

    /// `ui::mod` refuses to open one of these, so it is the type's own floor rather
    /// than a screen a reader can reach — but the arithmetic still has to hold.
    #[test]
    fn a_comparison_of_no_faces_is_empty_rather_than_broken() {
        let mut s = Sheet::compare(Vec::new(), 32.0);
        assert!(s.rows().is_empty());
        assert_eq!(s.size(), 0.0, "no row, no size to report");
        assert_eq!(s.title(), "compare — 0 face(s) at 0 px, +/- to resize");
        assert!(!s.resize(8.0), "there is nothing to resize");
        assert_eq!(s.lines(), 0);
        assert!(s.window(40).is_empty());
        for key in SCROLL_KEYS {
            s.scroll_by(key, 40);
            assert_eq!(s.scroll_row(), 0);
        }
    }

    /// Whatever the reader types is what every row is set in, and the rendering is cut
    /// to the pane rather than to the words.
    #[test]
    fn the_sample_text_may_be_empty_a_space_or_wider_than_any_terminal() {
        let f = face("Amiri-Regular.ttf");
        let w = Sheet::waterfall(f.clone(), Vec::new(), Vec::new());
        let c = Sheet::compare(vec![f, face("SourceSerif4-Regular.otf")], 32.0);
        let wide = "Hamburgefonstiv ".repeat(60);
        for chosen in ["", " ", wide.as_str()] {
            for (sheet, row) in [(&w, &w.rows()[0]), (&c, &c.rows()[1])] {
                assert_eq!(sheet.text_for(row, Some(chosen)), chosen);
                let opts = sheet.options(row, chosen.to_string(), 60);
                assert_eq!(opts.max_width, Some(60), "cut to the pane, not the words");
                assert_eq!(opts.padding, 1);
                assert_eq!(opts.size, row.size);
                let bitmap = fontina_core::render::render_face(&row.face, &opts).unwrap();
                assert!(
                    bitmap.width <= 60,
                    "{} pixels of type in a 60-column pane",
                    bitmap.width
                );
            }
        }
    }

    /// The fallback differs by kind, and that is the point of the kinds: a waterfall is
    /// one face, so it can use that face's own embedded sample string; a comparison
    /// holds the words constant across faces, so it cannot.
    #[test]
    fn only_a_waterfall_falls_back_to_the_faces_own_sample_string() {
        let mut f = face("Amiri-Regular.ttf");
        f.names.sample_text = Some("A specimen".into());
        let w = Sheet::waterfall(f.clone(), Vec::new(), Vec::new());
        assert!(w.rows().iter().all(|r| w.text_for(r, None) == "A specimen"));

        let c = Sheet::compare(vec![f.clone(), face("SourceSerif4-Regular.otf")], 32.0);
        let shared = typography::preview_text(&f);
        assert!(c.rows().iter().all(|r| c.text_for(r, None) == shared));
        assert_ne!(
            c.text_for(&c.rows()[0], None),
            "A specimen",
            "one face's sample string is not what the other face is compared against"
        );
    }

    /// A blank embedded sample string is not what a waterfall is set in.
    ///
    /// `text_for` used to hand the face's sample string back as it stood, blank
    /// included, and the only thing between a reader and a screen of nothing was
    /// `parse::english` dropping a name that trims empty — a coupling between two
    /// crates that nothing declared. The sheet now decides for itself, and the second
    /// half of this test still holds the parser to its side of it.
    #[test]
    fn a_blank_embedded_sample_string_is_not_what_a_waterfall_is_set_in() {
        let mut f = face("Amiri-Regular.ttf");
        f.names.sample_text = Some("   ".into());
        let w = Sheet::waterfall(f.clone(), Vec::new(), Vec::new());
        assert_eq!(
            w.text_for(&w.rows()[0], None),
            typography::preview_text(&f),
            "a sample string of spaces falls back to real words"
        );
        // What the reader typed still wins, whatever it is.
        assert_eq!(w.text_for(&w.rows()[0], Some("  ")), "  ");

        for name in [
            "Amiri-Regular.ttf",
            "SourceSerif4-Regular.otf",
            "BricolageGrotesque[opsz,wdth,wght].ttf",
            "Nabla[EDPT,EHLT].ttf",
            "inter-latin-400-normal.woff",
        ] {
            let parsed = face(name);
            assert!(
                parsed
                    .names
                    .sample_text
                    .as_deref()
                    .is_none_or(|s| !s.trim().is_empty()),
                "{name} came out of the parser with a blank sample string"
            );
        }
    }

    #[test]
    fn the_title_says_which_sheet_it_is_and_what_it_is_set_at() {
        assert_eq!(
            waterfall().title(),
            format!("waterfall — {} sizes", typography::WATERFALL_SIZES.len())
        );
        let mut c = Sheet::compare(vec![face("Amiri-Regular.ttf")], 32.0);
        assert_eq!(c.title(), "compare — 1 face(s) at 32 px, +/- to resize");
        assert!(c.resize(-4.0));
        assert_eq!(c.title(), "compare — 1 face(s) at 28 px, +/- to resize");
    }

    #[test]
    fn a_resize_moves_every_row_together_and_stops_together() {
        let mut c = Sheet::compare(
            vec![
                face("Amiri-Regular.ttf"),
                face("SourceSerif4-Regular.otf"),
                face("BricolageGrotesque[opsz,wdth,wght].ttf"),
            ],
            158.0,
        );
        assert!(
            c.resize(8.0),
            "a step that only part of the way is still a step"
        );
        assert!(
            c.rows().iter().all(|r| r.size == 160.0),
            "clamped, and clamped together"
        );
        assert!(!c.resize(8.0));
        for _ in 0..40 {
            c.resize(-8.0);
        }
        assert!(c.rows().iter().all(|r| r.size == 8.0));
        assert_eq!(
            c.size(),
            8.0,
            "and the title reports what the rows are set at"
        );
    }

    // ----- frames -----
    //
    // The sheet is a mode rather than a pane: it covers the browser and has to stay
    // inside the area it was handed. `ui::mod` draws it, so these go through a frame.

    /// The browser over the fixture fonts, with a blank sample text: what the preview
    /// draws has its own tests in `ui::mod`, and a rasteriser in these frames would
    /// make them say more about this machine than about the sheet.
    fn app() -> crate::ui::App {
        let mut index = fontina_core::Index::open_in_memory().unwrap();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        fontina_core::scan::scan(&mut index, &[fixtures], &Default::default()).unwrap();
        let mut app = crate::ui::App::new(index).unwrap();
        app.preview_text = Some(" ".into());
        app
    }

    /// The browser drawn into an in-memory terminal, one string per terminal row.
    fn frame(app: &mut crate::ui::App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    // Two columns leave nothing at all once the border has taken them; nine leave seven,
    // one short of the width `draw_sheet` insists on; four rows leave the pane no room
    // for a line of type once the border has had two of them.
    const SIZES: [(u16, u16); 16] = [
        (1, 1),
        (1, 2),
        (2, 1),
        (2, 20),
        (1, 40),
        (40, 1),
        (3, 3),
        (9, 20),
        (10, 20),
        (20, 4),
        (20, 5),
        (80, 24),
        (120, 40),
        (200, 60),
        (400, 12),
        (400, 120),
    ];

    /// Every terminal size the sheet can be asked to draw in: one column, one row, no
    /// usable area at all once the border has taken its two columns, and a wall of
    /// them. Nothing may panic, and the sheet must stay inside the pane it was given —
    /// the key line at the foot of the screen belongs to the browser underneath, and is
    /// the same row either way.
    #[test]
    fn a_sheet_draws_at_any_terminal_size() {
        for kind in [Kind::Waterfall, Kind::Compare] {
            for (w, h) in SIZES {
                let mut app = app();
                let closed = frame(&mut app, w, h);
                app.open_sheet(kind).unwrap();
                assert!(app.sheet.is_some(), "a face was selected");
                let open = frame(&mut app, w, h);
                assert_eq!(open.len(), h as usize, "one row per line on {w}x{h}");
                for row in &open {
                    assert!(
                        row.chars().count() <= w as usize,
                        "a row is wider than the {w}-column terminal it is drawn in"
                    );
                }
                if h >= 5 {
                    assert_eq!(
                        open.last(),
                        closed.last(),
                        "the sheet drew over the key line on a {w}x{h} terminal"
                    );
                }
                // And what the frame left behind is a scroll the next keypress can use.
                let sheet = app.sheet.as_ref().unwrap();
                assert!(
                    sheet.scroll_row() <= sheet.lines().saturating_sub(1),
                    "a {w}x{h} frame left the scroll past the end of the sheet"
                );
            }
        }
    }

    /// Every cell outside the pane the sheet was handed belongs to something else. The
    /// panes underneath are drawn first and the sheet goes over them, so a cell it
    /// writes outside its own area is a pane it has silently eaten.
    #[test]
    fn a_sheet_writes_nothing_outside_the_pane_it_is_given() {
        use ratatui::layout::{Position, Rect};
        let mut app = app();
        app.open_sheet(Kind::Waterfall).unwrap();
        for pane in [
            Rect::new(3, 2, 60, 20),
            Rect::new(0, 0, 1, 1),
            Rect::new(10, 10, 2, 2),
            Rect::new(5, 1, 40, 1),
            Rect::new(0, 0, 80, 30),
            Rect::new(79, 29, 1, 1),
        ] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
            terminal.draw(|f| app.draw_sheet(f, pane)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            for y in 0..30 {
                for x in 0..80 {
                    if pane.contains(Position::new(x, y)) {
                        continue;
                    }
                    let cell = &buffer[(x, y)];
                    assert!(
                        cell.symbol() == " "
                            && cell.fg == ratatui::style::Color::Reset
                            && cell.bg == ratatui::style::Color::Reset,
                        "the sheet wrote {:?} at ({x}, {y}), outside {pane:?}",
                        cell.symbol()
                    );
                }
            }
        }
    }

    /// A comparison of every fixture at once, in panes that cannot hold one row of it.
    #[test]
    fn a_comparison_of_every_fixture_draws_in_a_pane_that_cannot_hold_a_row() {
        let mut app = app();
        let faces = [
            "Amiri-Regular.ttf",
            "SourceSerif4-Regular.otf",
            "BricolageGrotesque[opsz,wdth,wght].ttf",
            "Nabla[EDPT,EHLT].ttf",
            "inter-latin-400-normal.woff",
        ]
        .map(face)
        .to_vec();
        app.sheet = Some(Sheet::compare(faces, 96.0));
        for (w, h) in SIZES {
            let drawn = frame(&mut app, w, h);
            assert_eq!(drawn.len(), h as usize);
            for row in &drawn {
                assert!(row.chars().count() <= w as usize);
            }
        }
        // A row of type at 96 px is taller than any of those panes, so the sheet is
        // longer than the tallest of them and the reader has to scroll it.
        let sheet = app.sheet.as_ref().unwrap();
        assert!(sheet.lines() > 120, "{} lines", sheet.lines());
    }

    /// Words far wider than the pane are cut to the pane, not wrapped and not spilled.
    #[test]
    fn words_wider_than_the_terminal_stay_inside_the_pane() {
        let mut app = app();
        app.preview_text = Some("Hamburgefonstiv ".repeat(40));
        app.open_sheet(Kind::Compare).unwrap();
        for w in [20u16, 40, 120] {
            for (n, row) in frame(&mut app, w, 30).iter().enumerate() {
                assert!(
                    row.chars().count() <= w as usize,
                    "row {n} overflows a {w}-column terminal"
                );
            }
        }
    }

    /// Laying a sheet out is nine rasterisations for a waterfall, so it happens once
    /// per pane width and sample text and not once per frame — and not at all in a pane
    /// with no room to draw in.
    #[test]
    fn a_pane_with_no_room_lays_out_nothing_and_a_resized_one_lays_out_again() {
        let mut app = app();
        app.open_sheet(Kind::Waterfall).unwrap();
        frame(&mut app, 9, 30);
        assert_eq!(
            app.sheet.as_ref().unwrap().lines(),
            0,
            "seven usable columns is below the floor draw_sheet keeps"
        );
        frame(&mut app, 120, 4);
        assert_eq!(
            app.sheet.as_ref().unwrap().lines(),
            0,
            "and so is no height"
        );

        frame(&mut app, 120, 40);
        let lines = app.sheet.as_ref().unwrap().lines();
        assert!(lines > 0, "given room, it laid out");
        assert!(app.sheet.as_ref().unwrap().is_built_for(118, Some(" ")));
        frame(&mut app, 120, 40);
        assert_eq!(
            app.sheet.as_ref().unwrap().lines(),
            lines,
            "the same pane and the same words are the same layout"
        );
        frame(&mut app, 60, 40);
        assert!(
            app.sheet.as_ref().unwrap().is_built_for(58, Some(" ")),
            "a narrower pane is a new layout"
        );
    }

    /// A waterfall runs to a few hundred lines, so the title says where in it the
    /// reader is. It is built from the layout the *previous* frame left behind, which
    /// is why the first frame of a sheet has no position on it at all.
    #[test]
    fn the_title_says_where_in_the_sheet_the_reader_is() {
        let mut app = app();
        app.open_sheet(Kind::Waterfall).unwrap();
        let first = frame(&mut app, 120, 40).join("\n");
        assert!(
            !first.contains("[1/"),
            "the sheet had not been laid out when this title was written: {}",
            first.lines().next().unwrap_or_default()
        );
        let total = app.sheet.as_ref().unwrap().lines();
        assert!(total > 40, "{total} lines");
        let second = frame(&mut app, 120, 40).join("\n");
        assert!(second.contains(&format!("[1/{total}]")), "{second}");

        let visible = app.sheet_visible;
        app.sheet.as_mut().unwrap().scroll_by(5, visible);
        let third = frame(&mut app, 120, 40).join("\n");
        assert!(third.contains(&format!("[6/{total}]")), "{third}");
    }
}
