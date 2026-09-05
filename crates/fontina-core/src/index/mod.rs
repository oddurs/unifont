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

//! SQLite index. One file, WAL mode, FTS5 for names. The full `FaceMetadata` JSON is
//! stored per face so `info` round-trips without re-parsing the font.
//!
//! - this module: open, scan bookkeeping, listing and filtering, duplicates, stats
//! - [`library`]: tags, collections (with JSON export/import), sources, activation state,
//!   conflicts
//! - [`facets`]: facet counts and family grouping over a filter

mod facets;
mod library;
mod schema;

pub use facets::{
    FacetCount, Facets, Family, weight_bucket, weight_name, width_bucket, width_name,
};
pub use library::{
    ActivationRecord, ActivationState, BUNDLE_FILE, BUNDLE_FONTS, BundleReport, CollectionExport,
    CollectionFace, CollectionInfo, Conflict, ImportReport, Source, SourceKind, TagInfo,
    TagSyncChange, TagSyncReport, TagSyncSkip,
};

use crate::FileInfo;
use crate::error::Result;
use crate::freedom::{self, Freedom};
use crate::model::FaceMetadata;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub struct Index {
    conn: Connection,
}

/// Compact per-face row used by listings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FaceSummary {
    pub id: i64,
    pub path: String,
    pub index: u32,
    pub family: String,
    pub subfamily: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postscript_name: Option<String>,
    pub weight: f32,
    pub width: f32,
    /// The weights this face can be set to, when a `wght` axis lets it be set at all.
    ///
    /// Absent for a face that is only the one weight `weight` names, so the shape of a
    /// static face's JSON is exactly what it was, and presence is itself the answer to
    /// "does this reach further than it says".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight_range: Option<[f32; 2]>,
    /// The same, in percent, over `wdth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_range: Option<[f32; 2]>,
    pub italic: bool,
    pub variable: bool,
    pub color: bool,
    /// What `post.isFixedPitch` says. Reported, never second-guessed: a font whose
    /// advance widths contradict its own flag is a health check, not a filter that
    /// quietly disagrees with the file.
    #[serde(default)]
    pub monospace: bool,
    pub glyph_count: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Whether `license` grants the four freedoms. Derived on read, never stored.
    #[serde(default)]
    pub freedom: Freedom,
    pub scripts: Vec<String>,
    pub container: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub designer: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Activation state recorded by fontina, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<ActivationState>,
}

#[derive(Debug, Clone, Default)]
pub struct FaceFilter {
    /// Full-text query over family, subfamily, PostScript name and designer.
    pub query: Option<String>,
    /// Exact (case-insensitive) family match.
    pub family: Option<String>,
    pub variable: Option<bool>,
    pub color: Option<bool>,
    pub italic: Option<bool>,
    /// ISO 15924 script codes the face must cover, e.g. `Arab`. Every one of them: two
    /// scripts mean a face that has both, not either.
    pub scripts: Vec<String>,
    /// How many codepoints of each script in `scripts` the face must cover. `None` is
    /// one, which is "covers it at all".
    pub script_min: Option<u32>,
    /// A language the face claims, of either kind: an OpenType language system tag
    /// (`TRK`, `VIT`) or a BCP 47 tag on a name record (`tr`, `vi`). They are different
    /// namespaces and the tag says which is meant.
    pub lang: Option<String>,
    /// Restrict `lang` to one kind of claim.
    pub lang_source: Option<LanguageSource>,
    /// `Some(true)` for faces the font itself calls monospaced, `Some(false)` for the
    /// rest.
    pub monospace: Option<bool>,
    /// SPDX identifier prefix match, e.g. `OFL`.
    pub license: Option<String>,
    /// Whether the license grants the four freedoms.
    pub freedom: Option<Freedom>,
    pub weight: Option<(u16, u16)>,
    /// Width range in percent, e.g. `(75, 100)`.
    pub width: Option<(u16, u16)>,
    /// Exact (case-insensitive) `OS/2` vendor id.
    pub vendor: Option<String>,
    /// Faces carrying this tag.
    pub tag: Option<String>,
    /// Faces in this collection (by name).
    pub collection: Option<String>,
    /// `Some(true)`: only faces with an activation record; `Some(false)`: only without.
    pub active: Option<bool>,
    /// Only faces in exactly this activation state.
    pub activation: Option<ActivationState>,
    /// Container as in `FaceSummary::container`, e.g. `woff2`.
    pub container: Option<String>,
    pub path_prefix: Option<String>,
    /// Restrict to these face ids.
    pub ids: Option<Vec<i64>>,
    pub limit: Option<usize>,
}

/// One candidate from [`Index::related`], with the numbers that say what the overlap
/// means.
///
/// The score is printed rather than thresholded away, and the metrics stand beside it,
/// because "covers the same characters" is not the same as "is the same design". High
/// overlap with identical metrics is a variant of one typeface; high overlap with
/// different metrics is two fonts that happen to serve the same languages. The reader
/// draws that line, the way `freedom` reports a verdict and its reason rather than
/// filtering silently.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Related {
    pub face: FaceSummary,
    /// Jaccard similarity of the two codepoint sets: `|A ∩ B| / |A ∪ B|`, 0.0 to 1.0.
    pub overlap: f64,
    /// Codepoints both cover.
    pub shared: u32,
    /// Codepoints either covers.
    pub union: u32,
    /// True when units per em, ascender, descender and fixed pitch all agree with the
    /// target — the four numbers that decide whether identical coverage means identical
    /// design.
    pub metrics_agree: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DuplicateGroup {
    pub reason: String,
    pub key: String,
    pub faces: Vec<FaceSummary>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Stats {
    pub files: i64,
    pub faces: i64,
    pub families: i64,
    pub variable_faces: i64,
    pub color_faces: i64,
    pub failed_files: i64,
    pub tags: i64,
    pub collections: i64,
    pub sources: i64,
    pub activations: i64,
    pub db_path: String,
}

/// The `WHERE` clauses and their bound values for a filter.
struct Where {
    clauses: Vec<String>,
    args: Vec<Box<dyn rusqlite::ToSql>>,
}

impl Where {
    fn sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        }
    }
    fn params(&self) -> impl Iterator<Item = &dyn rusqlite::ToSql> {
        self.args.iter().map(|a| a.as_ref())
    }
}

/// How long a write waits for another fontina process before giving up. Generous on
/// purpose: the thing it waits for is usually one scan transaction committing, and
/// failing is worse for a user than a pause.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Put the file in WAL mode, waiting for another connection that is doing the same.
///
/// Changing the journal mode needs a moment with no other connection in the file, and
/// SQLite answers `SQLITE_BUSY` for it immediately rather than consulting the busy
/// timeout, so two fontinas opening the same fresh index collide here before they reach
/// a single query. The one that loses only has to wait: whoever won is setting the very
/// mode it wanted. If it never takes — an index on a filesystem that cannot do WAL —
/// the rollback journal still works, so this is not a reason to refuse to open.
fn ensure_wal(conn: &Connection) {
    for _ in 0..50 {
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap_or_default();
        if mode.eq_ignore_ascii_case("wal") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

impl Index {
    /// Default location: the platform data directory for `fontina`.
    pub fn default_path() -> PathBuf {
        directories::ProjectDirs::from("", "", "fontina")
            .map(|d| d.data_dir().join("index.db"))
            .unwrap_or_else(|| PathBuf::from("fontina-index.db"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::Error::Io(parent.to_path_buf(), e))?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        // First, before anything that takes a lock. Several fontina processes share one
        // index by design: `watch` runs as a user service while you tag something in `ui`
        // and activate something else from the shell. Without a busy timeout the second
        // writer fails instantly with "database is locked" instead of waiting the moment
        // or two the first one needs — and switching to WAL is itself a write, so setting
        // the timeout after it left two processes opening the same fresh index racing on
        // the very first pragma.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        ensure_wal(&conn);
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&mut conn)?;
        Ok(Index { conn })
    }

    pub fn path(&self) -> String {
        self.conn.path().unwrap_or(":memory:").to_string()
    }

    /// Begin a write transaction.
    ///
    /// `BEGIN IMMEDIATE`, not the deferred default. A deferred transaction takes a read
    /// lock and asks for the write lock later, and SQLite refuses that upgrade
    /// immediately when another connection holds the write lock: it cannot wait without
    /// risking deadlock, so the busy timeout does not apply and the caller sees
    /// "database is locked" however long it was willing to wait. Taking the write lock up
    /// front is what makes the timeout mean anything.
    pub fn begin(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    pub fn file_is_unchanged(&self, path: &str, size: u64, mtime: i64) -> Result<bool> {
        let row: Option<(i64, i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT size, mtime, error FROM files WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(matches!(row, Some((s, m, None)) if s == size as i64 && m == mtime))
    }

    /// Replace a file and its faces. Tags, collection memberships and activation state
    /// of the previous faces carry over by (path, face index).
    pub(crate) fn upsert_file_tx(
        tx: &Transaction,
        file: &FileInfo,
        faces: &[FaceMetadata],
    ) -> Result<()> {
        let carried = library::carry_over_take(tx, &file.path)?;
        tx.execute("DELETE FROM files WHERE path = ?1", params![file.path])?;
        tx.execute(
            "INSERT INTO files (path, size, mtime, blake3, container, face_count, scanned_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch(), NULL)",
            params![file.path, file.size as i64, file.mtime, file.blake3, file.container.as_str(), faces.len() as i64],
        )?;
        let file_id = tx.last_insert_rowid();
        let mut stmt = tx.prepare_cached(
            "INSERT INTO faces (file_id, face_index, postscript_name, family, subfamily, full_name,
                weight, width, italic, is_variable, is_color, glyph_count, license_spdx, vendor,
                version, designer, identity_hash, scripts, metadata,
                weight_min, weight_max, width_min, width_max, is_fixed_pitch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                ?20, ?21, ?22, ?23, ?24)",
        )?;
        for face in faces {
            let (weight_span, width_span) = (face.weight_span(), face.width_span());
            let scripts = format!(
                ",{},",
                face.coverage
                    .scripts
                    .iter()
                    .map(|s| s.script.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let face_id = stmt.insert(params![
                file_id,
                face.index,
                face.names.postscript_name,
                face.names.family,
                face.names.subfamily,
                face.names.full_name,
                face.style.weight,
                face.style.width,
                face.is_italic(),
                face.is_variable(),
                face.is_color(),
                face.glyph_count,
                face.license.spdx,
                face.os2.as_ref().map(|o| o.vendor_id.clone()),
                face.names.version,
                face.names.designer,
                face.identity_hash,
                scripts,
                serde_json::to_string(face)?,
                weight_span.0,
                weight_span.1,
                width_span.0,
                width_span.1,
                face.metrics.is_fixed_pitch,
            ])?;
            insert_ranges(tx, face_id, &face.coverage.ranges)?;
            insert_scripts(tx, face_id, &face.coverage.scripts)?;
            insert_languages(tx, face_id, face)?;
            library::carry_over_apply(tx, face_id, face.index, &carried)?;
        }
        Ok(())
    }

    /// Record that a file could not be parsed.
    ///
    /// A file that parsed before keeps its row, and so keeps its faces and everything
    /// hanging off them: tags, collection memberships and activation state. Deleting the
    /// row would cascade all of that away (`PRAGMA foreign_keys` is on), and a failure is
    /// usually transient: a font being rewritten in place, a truncated download, a
    /// half-copied file caught by the watcher. Curation the user built by hand must not
    /// depend on a parse succeeding on every pass. What the file last parsed as stays
    /// visible, flagged by `files.error` and reported in `stats`, until it parses again.
    pub(crate) fn record_failure_tx(tx: &Transaction, path: &str, error: &str) -> Result<()> {
        let updated = tx.execute(
            "UPDATE files SET error = ?2, scanned_at = unixepoch() WHERE path = ?1",
            params![path, error],
        )?;
        if updated == 0 {
            tx.execute(
                "INSERT INTO files (path, size, mtime, blake3, container, face_count, scanned_at, error)
                 VALUES (?1, 0, 0, '', '', 0, unixepoch(), ?2)",
                params![path, error],
            )?;
        }
        Ok(())
    }

    /// Remove files under `root` that no longer exist on disk. Returns the count removed.
    ///
    /// "Gone" means the path's own metadata reports `NotFound`. An unreadable file, or one
    /// under a directory we may not traverse, is kept: `Path::exists` cannot tell those
    /// apart from a deleted file, and pruning cascades the user's tags, collections and
    /// activation records away with the row.
    ///
    /// Two directory-level guards, because losing a whole library is not a recoverable
    /// mistake. Nothing is pruned when `root` itself is unreadable, and nothing is pruned
    /// when *every* indexed file under it has vanished at once: an unmounted share is
    /// indistinguishable from a deleted one, and the empty mount point is the common case.
    /// [`Index::remove_under`] is the explicit way to forget a directory on purpose.
    pub fn prune_missing(&mut self, root: &str) -> Result<usize> {
        match std::fs::metadata(Path::new(root)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Ok(0),
        }
        let paths: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT path FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'")?;
            let rows =
                stmt.query_map(params![root, like_prefix(root)], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let missing: Vec<&String> = paths.iter().filter(|p| is_gone(p)).collect();
        if missing.len() == paths.len() && paths.len() > 1 {
            return Ok(0);
        }
        let tx = self.begin()?;
        for p in &missing {
            tx.execute("DELETE FROM files WHERE path = ?1", params![p])?;
        }
        tx.commit()?;
        Ok(missing.len())
    }

    /// Forget one file (and its faces). Returns whether it was indexed.
    pub fn remove_file(&mut self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?
            > 0)
    }

    /// Remove every file under `root` from the index, present on disk or not.
    pub fn remove_under(&mut self, root: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            params![root, like_prefix(root)],
        )?)
    }

    fn row_to_summary(r: &rusqlite::Row) -> rusqlite::Result<FaceSummary> {
        /// A span, or `None` where the two ends are the same number and there is nothing
        /// to say.
        fn span(r: &rusqlite::Row, lo: &str, hi: &str) -> rusqlite::Result<Option<[f32; 2]>> {
            let (lo, hi): (f32, f32) = (r.get(lo)?, r.get(hi)?);
            Ok((lo < hi).then_some([lo, hi]))
        }

        let scripts: String = r.get("scripts")?;
        let tags: Option<String> = r.get("tags")?;
        let activation: Option<String> = r.get("activation")?;
        let license: Option<String> = r.get("license_spdx")?;
        Ok(FaceSummary {
            id: r.get("id")?,
            path: r.get("path")?,
            index: r.get::<_, i64>("face_index")? as u32,
            family: r.get("family")?,
            subfamily: r.get("subfamily")?,
            postscript_name: r.get("postscript_name")?,
            weight: r.get("weight")?,
            width: r.get("width")?,
            weight_range: span(r, "weight_min", "weight_max")?,
            width_range: span(r, "width_min", "width_max")?,
            italic: r.get("italic")?,
            variable: r.get("is_variable")?,
            color: r.get("is_color")?,
            monospace: r.get("is_fixed_pitch")?,
            glyph_count: r.get::<_, i64>("glyph_count")? as u16,
            freedom: crate::freedom::classify(license.as_deref()),
            license,
            scripts: scripts
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            container: r.get("container")?,
            vendor: r.get("vendor")?,
            designer: r.get("designer")?,
            tags: tags
                .map(|t| t.split('\u{1f}').map(String::from).collect())
                .unwrap_or_default(),
            activation: activation.and_then(|a| a.parse().ok()),
        })
    }

    const SUMMARY_SELECT: &'static str = "SELECT f.id, fi.path, f.face_index, f.family, f.subfamily, f.postscript_name,
        f.weight, f.width, f.weight_min, f.weight_max, f.width_min, f.width_max,
        f.italic, f.is_variable, f.is_color, f.is_fixed_pitch, f.glyph_count, f.license_spdx, f.scripts, fi.container,
        f.vendor, f.designer,
        (SELECT group_concat(name, char(31)) FROM (SELECT t.name FROM face_tags ft JOIN tags t ON t.id = ft.tag_id
            WHERE ft.face_id = f.id ORDER BY t.name COLLATE NOCASE)) AS tags,
        a.scope AS activation
        FROM faces f JOIN files fi ON fi.id = f.file_id LEFT JOIN activations a ON a.face_id = f.id";

    const SUMMARY_ORDER: &'static str =
        " ORDER BY f.family COLLATE NOCASE, f.weight, f.italic, f.width, fi.path, f.face_index";

    fn where_for(filter: &FaceFilter) -> Where {
        let mut w = Where {
            clauses: Vec::new(),
            args: Vec::new(),
        };
        if let Some(q) = filter
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            w.clauses
                .push("f.id IN (SELECT rowid FROM faces_fts WHERE faces_fts MATCH ?)".into());
            w.args.push(Box::new(fts_query(q)));
        }
        if let Some(fam) = &filter.family {
            w.clauses.push("f.family = ? COLLATE NOCASE".into());
            w.args.push(Box::new(fam.clone()));
        }
        if let Some(v) = filter.variable {
            w.clauses.push("f.is_variable = ?".into());
            w.args.push(Box::new(v));
        }
        if let Some(v) = filter.color {
            w.clauses.push("f.is_color = ?".into());
            w.args.push(Box::new(v));
        }
        if let Some(v) = filter.italic {
            w.clauses.push("f.italic = ?".into());
            w.args.push(Box::new(v));
        }
        // Every script asked for, each its own clause, so two of them mean both — which
        // the `LIKE` over the joined string could never express. `script_min` is the
        // depth: a font with three Arabic codepoints should stop ranking beside one with
        // three thousand.
        if let Some(v) = filter.monospace {
            w.clauses.push("f.is_fixed_pitch = ?".into());
            w.args.push(Box::new(v));
        }
        if let Some(l) = &filter.lang {
            let mut clause = String::from(
                "f.id IN (SELECT fl.face_id FROM face_languages fl
                          WHERE fl.tag = ? COLLATE NOCASE",
            );
            if filter.lang_source.is_some() {
                clause.push_str(" AND fl.source = ?");
            }
            clause.push(')');
            w.clauses.push(clause);
            w.args.push(Box::new(l.clone()));
            if let Some(src) = filter.lang_source {
                w.args.push(Box::new(src.as_str().to_string()));
            }
        }
        for s in &filter.scripts {
            w.clauses.push(
                "f.id IN (SELECT fs.face_id FROM face_scripts fs
                          WHERE fs.script = ? COLLATE NOCASE AND fs.codepoints >= ?)"
                    .into(),
            );
            w.args.push(Box::new(s.clone()));
            w.args.push(Box::new(filter.script_min.unwrap_or(1) as i64));
        }
        if let Some(l) = &filter.license {
            w.clauses.push("f.license_spdx LIKE ?".into());
            w.args.push(Box::new(format!("{}%", l)));
        }
        if let Some(f) = filter.freedom {
            w.clauses.push(freedom_clause(f));
        }
        // An overlap, not a containment: a face matches when the range it spans and the
        // range asked for share any point at all. For a static face the two ends are
        // equal and this is the old `BETWEEN`.
        if let Some((lo, hi)) = filter.weight {
            w.clauses
                .push("f.weight_min <= ? AND f.weight_max >= ?".into());
            w.args.push(Box::new(hi));
            w.args.push(Box::new(lo));
        }
        if let Some((lo, hi)) = filter.width {
            w.clauses
                .push("f.width_min <= ? AND f.width_max >= ?".into());
            w.args.push(Box::new(hi));
            w.args.push(Box::new(lo));
        }
        if let Some(v) = &filter.vendor {
            w.clauses.push("f.vendor = ? COLLATE NOCASE".into());
            w.args.push(Box::new(v.clone()));
        }
        if let Some(t) = &filter.tag {
            w.clauses.push(
                "f.id IN (SELECT ft.face_id FROM face_tags ft JOIN tags t ON t.id = ft.tag_id WHERE t.name = ? COLLATE NOCASE)"
                    .into(),
            );
            w.args.push(Box::new(t.clone()));
        }
        if let Some(c) = &filter.collection {
            w.clauses.push(
                "f.id IN (SELECT cf.face_id FROM collection_faces cf JOIN collections c ON c.id = cf.collection_id WHERE c.name = ? COLLATE NOCASE)"
                    .into(),
            );
            w.args.push(Box::new(c.clone()));
        }
        if let Some(active) = filter.active {
            w.clauses.push(if active {
                "a.face_id IS NOT NULL".into()
            } else {
                "a.face_id IS NULL".into()
            });
        }
        if let Some(state) = filter.activation {
            w.clauses.push("a.scope = ?".into());
            w.args.push(Box::new(state.as_str()));
        }
        if let Some(c) = &filter.container {
            w.clauses.push("fi.container = ?".into());
            w.args.push(Box::new(c.to_ascii_lowercase()));
        }
        if let Some(p) = &filter.path_prefix {
            w.clauses.push("fi.path LIKE ? ESCAPE '\\'".into());
            w.args.push(Box::new(like_prefix(p)));
        }
        if let Some(ids) = &filter.ids {
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            w.clauses.push(format!("f.id IN ({list})"));
        }
        w
    }

    fn query_summaries(&self, sql: &str, w: &Where) -> Result<Vec<FaceSummary>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(w.params()), Self::row_to_summary)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list(&self, filter: &FaceFilter) -> Result<Vec<FaceSummary>> {
        let w = Self::where_for(filter);
        let mut sql = format!("{}{}{}", Self::SUMMARY_SELECT, w.sql(), Self::SUMMARY_ORDER);
        if let Some(n) = filter.limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        self.query_summaries(&sql, &w)
    }

    /// Summaries for specific ids, in the usual listing order.
    pub fn summaries(&self, ids: &[i64]) -> Result<Vec<FaceSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.list(&FaceFilter {
            ids: Some(ids.to_vec()),
            ..Default::default()
        })
    }

    /// Face ids stored for a file path, in face order.
    pub fn ids_for_path(&self, path: &str) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.id FROM faces f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1 ORDER BY f.face_index",
        )?;
        let rows = stmt.query_map(params![path], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Ids of every face in the same file as `face_id` (itself included).
    pub fn file_faces(&self, face_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM faces WHERE file_id = (SELECT file_id FROM faces WHERE id = ?1) ORDER BY face_index",
        )?;
        let rows = stmt.query_map(params![face_id], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Faces whose cmap covers every character in `text` (whitespace and controls
    /// ignored). At most 500 distinct characters.
    pub fn covering(&self, text: &str, filter: &FaceFilter) -> Result<Vec<FaceSummary>> {
        let mut cps: Vec<u32> = text
            .chars()
            .filter(|c| !c.is_whitespace() && !c.is_control())
            .map(|c| c as u32)
            .collect();
        cps.sort_unstable();
        cps.dedup();
        if cps.is_empty() {
            return Ok(Vec::new());
        }
        if cps.len() > 500 {
            return Err(crate::Error::Other(
                "text has more than 500 distinct characters".into(),
            ));
        }
        let mut w = Self::where_for(filter);
        for cp in &cps {
            w.clauses.push(
                "EXISTS (SELECT 1 FROM face_ranges r WHERE r.face_id = f.id AND r.lo <= ? AND r.hi >= ?)"
                    .into(),
            );
            w.args.push(Box::new(*cp as i64));
            w.args.push(Box::new(*cp as i64));
        }
        let mut sql = format!("{}{}{}", Self::SUMMARY_SELECT, w.sql(), Self::SUMMARY_ORDER);
        if let Some(n) = filter.limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        self.query_summaries(&sql, &w)
    }

    pub fn get_face(&self, id: i64) -> Result<Option<FaceMetadata>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT metadata FROM faces WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    pub fn faces_for_path(&self, path: &str) -> Result<Vec<FaceMetadata>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.metadata FROM faces f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1 ORDER BY f.face_index",
        )?;
        let rows = stmt.query_map(params![path], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for j in rows {
            out.push(serde_json::from_str(&j?)?);
        }
        Ok(out)
    }

    /// Faces whose character coverage overlaps `face_id` by at least `min`.
    ///
    /// The declared family is often the wrong unit for "these belong together", and there
    /// is no standard that says what the right one is. §12 rules out storing a computed
    /// superfamily: deciding what belongs together needs a rule, the only rule available
    /// is a naming convention, those conventions belong to other projects and change
    /// without telling anyone, and a stored grouping that is wrong is worse than none
    /// because everything downstream inherits the mistake.
    ///
    /// A question is the right shape instead. Asked of one face it is answerable from
    /// evidence already in the index, it costs nothing when nobody asks, and when it is
    /// wrong it is wrong once rather than permanently.
    ///
    /// Jaccard over the codepoint sets, computed straight from `face_ranges`, which
    /// already holds them as sorted ranges indexed by face — so an intersection is a
    /// linear merge and the whole query is one pass over the library.
    ///
    /// This is why it is not `dupes` and does not become a flag on it. `dupes` can sweep
    /// the whole library because exact identity is hash equality: group by
    /// `identity_hash`, one pass. Similarity has no such trick — it is pairwise, and a
    /// sweep is quadratic over a library that may hold tens of thousands of faces. Same
    /// axis, different cost, so a different shape: `dupes` sweeps, this answers about a
    /// target.
    pub fn related(&self, face_id: i64, min: f64) -> Result<Vec<Related>> {
        if self.get_summary(face_id)?.is_none() {
            return Err(crate::Error::Other(format!("no face with id {face_id}")));
        }
        let mine = self.ranges_of(face_id)?;
        if mine.is_empty() {
            return Ok(Vec::new());
        }
        let target_metrics = self.metrics_key(face_id)?;

        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT face_id, lo, hi FROM face_ranges WHERE face_id != ?1 ORDER BY face_id, lo",
        )?;
        let rows = stmt.query_map(params![face_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as u32,
                r.get::<_, i64>(2)? as u32,
            ))
        })?;
        let mut current: Option<(i64, Vec<[u32; 2]>)> = None;
        let finish = |id: i64, ranges: &[[u32; 2]], out: &mut Vec<(i64, u32, u32)>| {
            let (shared, union) = overlap(&mine, ranges);
            if union > 0 {
                out.push((id, shared, union));
            }
        };
        let mut scored: Vec<(i64, u32, u32)> = Vec::new();
        for row in rows {
            let (id, lo, hi) = row?;
            match &mut current {
                Some((cur, ranges)) if *cur == id => ranges.push([lo, hi]),
                Some((cur, ranges)) => {
                    finish(*cur, ranges, &mut scored);
                    current = Some((id, vec![[lo, hi]]));
                }
                None => current = Some((id, vec![[lo, hi]])),
            }
        }
        if let Some((cur, ranges)) = &current {
            finish(*cur, ranges, &mut scored);
        }

        for (id, shared, union) in scored {
            let score = f64::from(shared) / f64::from(union);
            if score < min {
                continue;
            }
            let Some(face) = self.get_summary(id)? else {
                continue;
            };
            out.push(Related {
                // Two unknowns are not an agreement, so `None == None` must not count.
                metrics_agree: match (self.metrics_key(id)?, target_metrics) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                },
                face,
                overlap: score,
                shared,
                union,
            });
        }
        // Most alike first, and by id after that so the order does not wander between
        // runs over a library where several faces tie.
        out.sort_by(|a, b| {
            b.overlap
                .total_cmp(&a.overlap)
                .then(a.face.id.cmp(&b.face.id))
        });
        Ok(out)
    }

    /// The four numbers that decide whether identical coverage means identical design,
    /// or `None` for a row whose stored metadata this build cannot read.
    ///
    /// `None` rather than an error: every M4 backfill tolerates exactly this row, and
    /// `list` keeps working on an index that holds one. Propagating here would let a
    /// single unreadable face — very likely not even the one being asked about — fail the
    /// whole of `related`, and so all of `fontina variants`. An unknown key compares
    /// equal to nothing, so such a candidate is reported with `metrics_agree: false`,
    /// which is the honest answer: fontina does not know that they agree.
    fn metrics_key(&self, face_id: i64) -> Result<Option<(u16, i16, i16, bool)>> {
        let json: String = self.conn.query_row(
            "SELECT metadata FROM faces WHERE id = ?1",
            params![face_id],
            |r| r.get(0),
        )?;
        let Ok(face) = serde_json::from_str::<FaceMetadata>(&json) else {
            return Ok(None);
        };
        Ok(Some((
            face.metrics.units_per_em,
            face.metrics.ascender,
            face.metrics.descender,
            face.metrics.is_fixed_pitch,
        )))
    }

    fn ranges_of(&self, face_id: i64) -> Result<Vec<[u32; 2]>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT lo, hi FROM face_ranges WHERE face_id = ?1 ORDER BY lo")?;
        let rows = stmt.query_map(params![face_id], |r| {
            Ok([r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32])
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn get_summary(&self, face_id: i64) -> Result<Option<FaceSummary>> {
        Ok(self.summaries(&[face_id])?.into_iter().next())
    }

    /// Faces that share an identity hash (same outlines and names across containers) or a
    /// PostScript name (installing both would conflict).
    pub fn duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        let mut groups = Vec::new();
        for (reason, column) in [
            ("identical outlines and names", "identity_hash"),
            ("same PostScript name", "postscript_name"),
        ] {
            let sql = format!(
                "{} WHERE f.{column} IN (SELECT {column} FROM faces WHERE {column} IS NOT NULL AND {column} != '' GROUP BY {column} HAVING COUNT(*) > 1)
                 ORDER BY f.{column}, fi.path, f.face_index",
                Self::SUMMARY_SELECT.replace("SELECT f.id,", &format!("SELECT f.{column} AS grp, f.id,"))
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>("grp")?, Self::row_to_summary(r)?))
            })?;
            let mut current: Option<DuplicateGroup> = None;
            for row in rows {
                let (key, face) = row?;
                match current.as_mut() {
                    Some(g) if g.key == key => g.faces.push(face),
                    _ => {
                        if let Some(g) = current.take() {
                            groups.push(g);
                        }
                        current = Some(DuplicateGroup {
                            reason: reason.into(),
                            key,
                            faces: vec![face],
                        });
                    }
                }
            }
            if let Some(g) = current.take() {
                groups.push(g);
            }
        }
        // A PostScript-name group that is exactly an identity group adds nothing.
        let identity: Vec<Vec<i64>> = groups
            .iter()
            .filter(|g| g.reason.starts_with("identical"))
            .map(|g| g.faces.iter().map(|f| f.id).collect())
            .collect();
        groups.retain(|g| {
            if g.reason.starts_with("same") {
                let ids: Vec<i64> = g.faces.iter().map(|f| f.id).collect();
                !identity.contains(&ids)
            } else {
                true
            }
        });
        Ok(groups)
    }

    pub fn stats(&self) -> Result<Stats> {
        let q = |sql: &str| -> rusqlite::Result<i64> { self.conn.query_row(sql, [], |r| r.get(0)) };
        Ok(Stats {
            files: q("SELECT COUNT(*) FROM files WHERE error IS NULL")?,
            faces: q("SELECT COUNT(*) FROM faces")?,
            families: q("SELECT COUNT(DISTINCT family COLLATE NOCASE) FROM faces")?,
            variable_faces: q("SELECT COUNT(*) FROM faces WHERE is_variable")?,
            color_faces: q("SELECT COUNT(*) FROM faces WHERE is_color")?,
            failed_files: q("SELECT COUNT(*) FROM files WHERE error IS NOT NULL")?,
            tags: q("SELECT COUNT(*) FROM tags")?,
            collections: q("SELECT COUNT(*) FROM collections")?,
            sources: q("SELECT COUNT(*) FROM sources")?,
            activations: q("SELECT COUNT(*) FROM activations")?,
            db_path: self.path(),
        })
    }

    pub fn failures(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, error FROM files WHERE error IS NOT NULL ORDER BY path")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Insert the script coverage of a face, one row per script with its depth.
impl Index {
    /// The scripts a face covers and how many codepoints of each, deepest first.
    ///
    /// The same numbers `Coverage.scripts` has always held, now answerable per script
    /// without reading the whole metadata document back.
    pub fn script_coverage(&self, face_id: i64) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT script, codepoints FROM face_scripts WHERE face_id = ?1
             ORDER BY codepoints DESC, script",
        )?;
        let rows = stmt.query_map(params![face_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Which of the two things a font can say about a language it is.
///
/// They are different claims and must not be collapsed. A language system tag under an
/// OpenType script says the shaping engine has rules for that language — Turkish `i`,
/// Serbian italics. A BCP 47 tag on a name record only says the font names *itself* in
/// that language, which says nothing about whether it can set a word of it. A filter
/// that merged them would over-report in one direction and under-report in the other,
/// and the reader would have no way to tell which had happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LanguageSource {
    /// An OpenType language system tag declared under a script in GSUB or GPOS.
    Opentype,
    /// A BCP 47 tag on a `name` record.
    Name,
}

impl LanguageSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LanguageSource::Opentype => "opentype",
            LanguageSource::Name => "name",
        }
    }
}

impl std::str::FromStr for LanguageSource {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        Ok(match s {
            "opentype" => LanguageSource::Opentype,
            "name" => LanguageSource::Name,
            _ => return Err(()),
        })
    }
}

impl Index {
    /// The languages a face claims, and which claim each one is. Sorted by tag.
    pub fn languages(&self, face_id: i64) -> Result<Vec<(String, LanguageSource)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT tag, source FROM face_languages WHERE face_id = ?1 ORDER BY tag, source",
        )?;
        let rows = stmt.query_map(params![face_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?
                    .parse()
                    .unwrap_or(LanguageSource::Name),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Insert both kinds of language claim a face makes.
pub(crate) fn insert_languages(tx: &Transaction, face_id: i64, face: &FaceMetadata) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR IGNORE INTO face_languages (face_id, tag, source) VALUES (?1, ?2, ?3)",
    )?;
    for script in &face.features.scripts {
        for lang in &script.languages {
            // OpenType language system tags are four bytes, space-padded: `TRK `, `AZE `.
            // The padding is the format's, not the language's.
            let tag = lang.trim();
            if !tag.is_empty() {
                stmt.execute(params![face_id, tag, LanguageSource::Opentype.as_str()])?;
            }
        }
    }
    for record in &face.name_records {
        if let Some(tag) = record
            .language
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            stmt.execute(params![face_id, tag, LanguageSource::Name.as_str()])?;
        }
    }
    Ok(())
}

pub(crate) fn insert_scripts(
    tx: &Transaction,
    face_id: i64,
    scripts: &[crate::model::ScriptCoverage],
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR REPLACE INTO face_scripts (face_id, script, codepoints) VALUES (?1, ?2, ?3)",
    )?;
    for s in scripts {
        stmt.execute(params![face_id, s.script, s.codepoints])?;
    }
    Ok(())
}

/// Sizes of the intersection and the union of two sorted, merged range lists.
///
/// A linear merge: both sides are already sorted and non-overlapping, which is what
/// `Coverage.ranges` guarantees and what `face_ranges` stores.
fn overlap(a: &[[u32; 2]], b: &[[u32; 2]]) -> (u32, u32) {
    let count = |r: &[[u32; 2]]| -> u32 { r.iter().map(|[lo, hi]| hi - lo + 1).sum() };
    let (mut i, mut j, mut shared) = (0usize, 0usize, 0u32);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        let lo = x[0].max(y[0]);
        let hi = x[1].min(y[1]);
        if lo <= hi {
            shared += hi - lo + 1;
        }
        if x[1] < y[1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    // |A ∪ B| = |A| + |B| - |A ∩ B|, which costs nothing once the intersection is known.
    (shared, count(a) + count(b) - shared)
}

pub(crate) fn insert_ranges(tx: &Transaction, face_id: i64, ranges: &[[u32; 2]]) -> Result<()> {
    let mut stmt =
        tx.prepare_cached("INSERT INTO face_ranges (face_id, lo, hi) VALUES (?1, ?2, ?3)")?;
    for [lo, hi] in ranges {
        stmt.execute(params![face_id, *lo as i64, *hi as i64])?;
    }
    Ok(())
}

/// `LIKE` pattern (with `ESCAPE '\\'`) matching anything below `root`, wildcards and
/// backslashes escaped. The separator is appended before escaping so a Windows `\`
/// does not swallow the `%`.
/// The `WHERE` fragment selecting faces whose license falls in `want`.
///
/// The freedom of a face is derived from its SPDX identifier rather than stored, so the
/// clause is built from `freedom::FREE` and `freedom::NONFREE` on every query and cannot
/// go stale when those tables change. `license_spdx` only ever holds a single identifier,
/// since `license::spdx_from_names` is what writes it; SPDX expressions reach
/// `freedom::classify` through externally supplied metadata, not through the index.
fn freedom_clause(want: Freedom) -> String {
    let unstated = "(f.license_spdx IS NULL OR trim(f.license_spdx) = '')";
    let free = freedom::sql_in("f.license_spdx", freedom::FREE);
    let nonfree = freedom::sql_in("f.license_spdx", freedom::NONFREE);
    match want {
        Freedom::Unstated => unstated.to_string(),
        Freedom::Free => format!("(NOT {unstated} AND {free})"),
        Freedom::Nonfree => format!("(NOT {unstated} AND {nonfree})"),
        Freedom::Unknown => format!("(NOT {unstated} AND NOT {free} AND NOT {nonfree})"),
    }
}

/// Whether a path is really gone, as opposed to merely unreadable. A dangling symlink
/// counts as gone: `metadata` follows the link, and the font it named is what matters.
fn is_gone(path: &str) -> bool {
    matches!(std::fs::metadata(Path::new(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

fn like_prefix(root: &str) -> String {
    let mut prefix = root.to_string();
    if !(prefix.ends_with(std::path::MAIN_SEPARATOR) || prefix.ends_with('/')) {
        prefix.push(std::path::MAIN_SEPARATOR);
    }
    let escaped = prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("{escaped}%")
}

/// Turn free text into an FTS5 prefix query: each term quoted and suffixed with `*`.
fn fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}
