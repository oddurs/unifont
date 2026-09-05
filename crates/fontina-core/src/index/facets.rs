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

//! Facet counts and family grouping over a filter. Both are computed from one lean row
//! scan; with 50k faces that is a few milliseconds, well inside the search budget.

use super::{FaceFilter, FaceSummary, Index};
use crate::error::Result;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FacetCount {
    pub value: String,
    pub count: i64,
}

/// Counts of faces per facet value, for the faces matching a filter.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Facets {
    pub faces: i64,
    pub families: i64,
    /// CSS weight buckets, `100`..`900`.
    pub weight: Vec<FacetCount>,
    /// Width buckets as percentages, `50`..`200`.
    pub width: Vec<FacetCount>,
    /// `upright` or `italic`.
    pub style: Vec<FacetCount>,
    pub variable: i64,
    pub color: i64,
    pub container: Vec<FacetCount>,
    /// ISO 15924 script codes.
    pub script: Vec<FacetCount>,
    /// Languages the matched faces claim, most-claimed first.
    ///
    /// The value is the tag alone. Both kinds of claim are offered on the one list and
    /// the tag is what tells them apart — `TRK` is an OpenType language system, `tr` a
    /// BCP 47 name record — so a face claiming a language both ways appears twice, under
    /// two tags. Ask `Index::languages` for the source of a particular claim.
    pub language: Vec<FacetCount>,
    /// `monospace` or `proportional`, from `post.isFixedPitch`.
    pub spacing: Vec<FacetCount>,
    pub license: Vec<FacetCount>,
    /// `free`, `nonfree`, `unknown` or `unstated`, derived from the license.
    pub freedom: Vec<FacetCount>,
    pub vendor: Vec<FacetCount>,
    pub tag: Vec<FacetCount>,
    pub collection: Vec<FacetCount>,
    /// `session`, `user`, `installed`, or `none`.
    pub activation: Vec<FacetCount>,
    /// Registered source directories the faces live under.
    pub source: Vec<FacetCount>,
}

/// Faces grouped by typographic family name.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Family {
    pub name: String,
    pub faces: usize,
    pub ids: Vec<i64>,
    /// The face to show for the family: upright, closest to weight 400 and width 100.
    pub representative: i64,
    pub variable: bool,
    pub color: bool,
    pub italic: bool,
    /// Lowest and highest weight in the family.
    pub weights: [f32; 2],
    pub widths: [f32; 2],
    pub scripts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub designer: Option<String>,
    pub containers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Faces with an activation record.
    pub active: usize,
    #[serde(skip)]
    rep_score: f32,
}

/// Nearest CSS weight bucket, 100..=900.
pub fn weight_bucket(weight: f32) -> u16 {
    ((weight / 100.0).round() as u16 * 100).clamp(100, 900)
}

/// Name of a CSS weight bucket.
pub fn weight_name(bucket: u16) -> &'static str {
    match bucket {
        100 => "Thin",
        200 => "ExtraLight",
        300 => "Light",
        400 => "Regular",
        500 => "Medium",
        600 => "SemiBold",
        700 => "Bold",
        800 => "ExtraBold",
        _ => "Black",
    }
}

const WIDTH_BUCKETS: &[(f32, &str)] = &[
    (50.0, "UltraCondensed"),
    (62.5, "ExtraCondensed"),
    (75.0, "Condensed"),
    (87.5, "SemiCondensed"),
    (100.0, "Normal"),
    (112.5, "SemiExpanded"),
    (125.0, "Expanded"),
    (150.0, "ExtraExpanded"),
    (200.0, "UltraExpanded"),
];

/// Nearest `usWidthClass` percentage.
pub fn width_bucket(width: f32) -> f32 {
    WIDTH_BUCKETS
        .iter()
        .min_by(|a, b| (a.0 - width).abs().total_cmp(&(b.0 - width).abs()))
        .map(|b| b.0)
        .unwrap_or(100.0)
}

pub fn width_name(bucket: f32) -> &'static str {
    WIDTH_BUCKETS
        .iter()
        .find(|b| (b.0 - bucket).abs() < 0.01)
        .map(|b| b.1)
        .unwrap_or("Normal")
}

/// Every CSS weight bucket the range `lo..=hi` touches, inclusive.
///
/// A static face has `lo == hi` and gets exactly the one bucket it always got.
pub fn weight_buckets_in(lo: f32, hi: f32) -> Vec<u16> {
    let (first, last) = (weight_bucket(lo), weight_bucket(hi.max(lo)));
    (first..=last).step_by(100).collect()
}

/// Every `usWidthClass` bucket the range `lo..=hi` touches, inclusive.
pub fn width_buckets_in(lo: f32, hi: f32) -> Vec<f32> {
    let (first, last) = (width_bucket(lo), width_bucket(hi.max(lo)));
    WIDTH_BUCKETS
        .iter()
        .map(|b| b.0)
        .filter(|b| *b >= first && *b <= last)
        .collect()
}

fn counts(map: BTreeMap<String, i64>) -> Vec<FacetCount> {
    map.into_iter()
        .map(|(value, count)| FacetCount { value, count })
        .collect()
}

fn counts_by_count(map: BTreeMap<String, i64>) -> Vec<FacetCount> {
    let mut v = counts(map);
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.value.cmp(&b.value)));
    v
}

fn fmt_width(w: f32) -> String {
    if w.fract() == 0.0 {
        format!("{}", w as i64)
    } else {
        format!("{w}")
    }
}

impl Index {
    pub fn facets(&self, filter: &FaceFilter) -> Result<Facets> {
        let w = Self::where_for(filter);
        let sql = format!(
            "SELECT f.family, f.weight_min, f.weight_max, f.width_min, f.width_max,
                    f.italic, f.is_variable, f.is_color, f.is_fixed_pitch, fi.container,
                    f.license_spdx, f.vendor, a.scope, fi.path
             FROM faces f JOIN files fi ON fi.id = f.file_id LEFT JOIN activations a ON a.face_id = f.id{}",
            w.sql()
        );
        let sources = self.sources()?;
        let mut out = Facets::default();
        let mut families = std::collections::HashSet::new();
        let (
            mut weight,
            mut width,
            mut style,
            mut container,
            mut spacing,
            mut license,
            mut freedom,
            mut vendor,
            mut activation,
            mut source,
        ) = (
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(w.params()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f32>(1)?,
                r.get::<_, f32>(2)?,
                r.get::<_, f32>(3)?,
                r.get::<_, f32>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, bool>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, String>(13)?,
            ))
        })?;
        for row in rows {
            let (
                family,
                wt_lo,
                wt_hi,
                wd_lo,
                wd_hi,
                italic,
                variable,
                color,
                monospace,
                cont,
                lic,
                ven,
                act,
                path,
            ) = row?;
            out.faces += 1;
            families.insert(family.to_lowercase());
            // A face that spans 200 to 800 belongs under every bucket in between, the
            // same way a face covering four scripts is counted under each of them.
            // Counting it only under its default instance made the facet disagree with
            // the filter beside it, which now finds it at all of them.
            for b in weight_buckets_in(wt_lo, wt_hi) {
                *weight.entry(b.to_string()).or_default() += 1;
            }
            for b in width_buckets_in(wd_lo, wd_hi) {
                *width.entry(fmt_width(b)).or_default() += 1;
            }
            *style
                .entry(if italic { "italic" } else { "upright" }.to_string())
                .or_default() += 1;
            if variable {
                out.variable += 1;
            }
            if color {
                out.color += 1;
            }
            *container.entry(cont).or_default() += 1;
            *spacing
                .entry(
                    if monospace {
                        "monospace"
                    } else {
                        "proportional"
                    }
                    .to_string(),
                )
                .or_default() += 1;
            *freedom
                .entry(crate::freedom::classify(lic.as_deref()).to_string())
                .or_default() += 1;
            *license
                .entry(lic.unwrap_or_else(|| "none".into()))
                .or_default() += 1;
            if let Some(v) = ven.filter(|v| !v.trim().is_empty()) {
                *vendor.entry(v.trim().to_string()).or_default() += 1;
            }
            *activation
                .entry(act.unwrap_or_else(|| "none".into()))
                .or_default() += 1;
            for s in &sources {
                if path.starts_with(&s.path) {
                    *source.entry(s.path.clone()).or_default() += 1;
                }
            }
        }
        out.families = families.len() as i64;
        // Weight and width sort numerically, not lexically.
        let mut weight = counts(weight);
        weight.sort_by_key(|c| c.value.parse::<u16>().unwrap_or(0));
        let mut width = counts(width);
        width.sort_by(|a, b| {
            a.value
                .parse::<f32>()
                .unwrap_or(0.0)
                .total_cmp(&b.value.parse::<f32>().unwrap_or(0.0))
        });
        out.weight = weight;
        out.width = width;
        out.style = counts(style);
        out.container = counts_by_count(container);
        out.spacing = counts_by_count(spacing);
        out.license = counts_by_count(license);
        out.freedom = counts_by_count(freedom);
        out.vendor = counts_by_count(vendor);
        out.activation = counts(activation);
        out.source = counts(source);

        let inner = format!(
            "SELECT f.id FROM faces f JOIN files fi ON fi.id = f.file_id LEFT JOIN activations a ON a.face_id = f.id{}",
            w.sql()
        );
        // Ordered by how much of each script is there, not by how many faces mention it.
        // Zyyy and Zinh — common punctuation and inherited marks — are in almost every
        // font and are never what it is for; a handful of Latin codepoints in a Japanese
        // face is not a Latin font. Depth says which is which, and `face_scripts` has
        // counted it since M4 §12 item 3. The count stays "faces", which is what the
        // facet means and what clicking it returns.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT fs.script, COUNT(*), SUM(fs.codepoints) FROM face_scripts fs
             WHERE fs.face_id IN ({inner})
             GROUP BY fs.script ORDER BY SUM(fs.codepoints) DESC, fs.script"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(w.params()), |r| {
            Ok(FacetCount {
                value: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        out.script = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        // Ordered by how many faces claim each language, which is the useful order for a
        // list somebody is scanning to narrow a library down. Both kinds of claim are
        // offered, told apart by the tag namespace rather than merged.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT fl.tag, COUNT(DISTINCT fl.face_id) FROM face_languages fl
             WHERE fl.face_id IN ({inner})
             GROUP BY fl.tag ORDER BY COUNT(DISTINCT fl.face_id) DESC, fl.tag"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(w.params()), |r| {
            Ok(FacetCount {
                value: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        out.language = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT t.name, COUNT(*) FROM face_tags ft JOIN tags t ON t.id = ft.tag_id WHERE ft.face_id IN ({inner}) GROUP BY t.id ORDER BY t.name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(w.params()), |r| {
            Ok(FacetCount {
                value: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        out.tag = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT c.name, COUNT(*) FROM collection_faces cf JOIN collections c ON c.id = cf.collection_id WHERE cf.face_id IN ({inner}) GROUP BY c.id ORDER BY c.name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(w.params()), |r| {
            Ok(FacetCount {
                value: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        out.collection = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Faces matching the filter, grouped by family. `filter.limit` caps families.
    pub fn families(&self, filter: &FaceFilter) -> Result<Vec<Family>> {
        let faces = self.list(&FaceFilter {
            limit: None,
            ..filter.clone()
        })?;
        let mut out: Vec<Family> = Vec::new();
        for f in faces {
            match out.last_mut() {
                Some(fam) if fam.name.eq_ignore_ascii_case(&f.family) => fam.push(&f),
                _ => out.push(Family::new(&f)),
            }
        }
        for fam in &mut out {
            fam.finish();
        }
        if let Some(n) = filter.limit {
            out.truncate(n);
        }
        Ok(out)
    }
}

impl Family {
    fn new(f: &FaceSummary) -> Family {
        let mut fam = Family {
            name: f.family.clone(),
            faces: 0,
            ids: Vec::new(),
            representative: f.id,
            variable: false,
            color: false,
            italic: false,
            weights: [
                f.weight_range.map_or(f.weight, |r| r[0]),
                f.weight_range.map_or(f.weight, |r| r[1]),
            ],
            widths: [
                f.width_range.map_or(f.width, |r| r[0]),
                f.width_range.map_or(f.width, |r| r[1]),
            ],
            scripts: f.scripts.clone(),
            license: f.license.clone(),
            vendor: f.vendor.clone(),
            designer: f.designer.clone(),
            containers: Vec::new(),
            tags: Vec::new(),
            active: 0,
            rep_score: f32::MAX,
        };
        fam.push(f);
        fam
    }

    /// Distance from "the regular face"; lower is more representative.
    fn score(f: &FaceSummary) -> f32 {
        (f.weight - 400.0).abs() + (f.width - 100.0).abs() + if f.italic { 1000.0 } else { 0.0 }
    }

    fn push(&mut self, f: &FaceSummary) {
        self.faces += 1;
        self.ids.push(f.id);
        self.variable |= f.variable;
        self.color |= f.color;
        self.italic |= f.italic;
        let (wt_lo, wt_hi) = f
            .weight_range
            .map_or((f.weight, f.weight), |r| (r[0], r[1]));
        let (wd_lo, wd_hi) = f.width_range.map_or((f.width, f.width), |r| (r[0], r[1]));
        self.weights[0] = self.weights[0].min(wt_lo);
        self.weights[1] = self.weights[1].max(wt_hi);
        self.widths[0] = self.widths[0].min(wd_lo);
        self.widths[1] = self.widths[1].max(wd_hi);
        if self.license.is_none() {
            self.license = f.license.clone();
        }
        if !self.containers.contains(&f.container) {
            self.containers.push(f.container.clone());
        }
        for t in &f.tags {
            if !self.tags.contains(t) {
                self.tags.push(t.clone());
            }
        }
        if f.activation.is_some() {
            self.active += 1;
        }
        if Self::score(f) < self.rep_score {
            self.representative = f.id;
            self.rep_score = Self::score(f);
            self.scripts = f.scripts.clone();
        }
    }

    fn finish(&mut self) {
        self.tags.sort();
        self.containers.sort();
    }
}
