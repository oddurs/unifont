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

mod config;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use fontina_core::{
    ActivationState, FaceFilter, FaceSummary, Freedom, Index, LanguageSource, ScanOptions,
    SourceKind, TagSyncChange, TagSyncReport, TagSyncSkip,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::PathBuf;

// See the note in Cargo.toml. musl's own allocator makes a parallel scan four times
// slower, and the scan is the thing a user waits for.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// The GNU Coding Standards ask `--version` to say who holds the copyright, under what
// licence the program is distributed, and that it comes with no warranty, so that a
// person who has only the binary can still find out what their rights are. `-V` keeps
// the short form for scripts that only want the number.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\n",
    "Copyright (C) 2026 Oddur Sigurdsson\n",
    "License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>.\n",
    "This is free software: you are free to change and redistribute it.\n",
    "There is NO WARRANTY, to the extent permitted by law.",
);

/// fontina: a lightweight, standards-based font manager.
#[derive(Parser)]
#[command(
    name = "fontina",
    version,
    long_version = LONG_VERSION,
    about,
    propagate_version = true
)]
struct Cli {
    /// Path to the index database (default: the platform data directory).
    #[arg(long, global = true, env = "FONTINA_DB")]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index fonts under one or more directories (or files).
    Scan {
        /// Directories or files to scan.
        paths: Vec<PathBuf>,
        /// Also scan the operating system's font directories.
        #[arg(long)]
        system: bool,
        /// Re-parse files even when size and mtime are unchanged.
        #[arg(long)]
        force: bool,
        /// Follow symlinks while walking.
        #[arg(long)]
        follow_symlinks: bool,
        /// Drop index entries under the scanned roots whose files no longer exist.
        #[arg(long)]
        prune: bool,
        #[arg(long)]
        json: bool,
    },
    /// List indexed faces, optionally filtered.
    List(ListArgs),
    /// List families (faces grouped by typographic family name), optionally filtered.
    Families(ListArgs),
    /// Count faces per weight, width, style, script, license, vendor, tag, collection,
    /// activation state and source, for the faces matching the filters.
    Facets(ListArgs),
    /// Tag faces. A tag is a free-form label; a face can carry many.
    #[command(subcommand)]
    Tag(TagCmd),
    /// Collections: ordered, named sets of faces that export to and import from JSON.
    #[command(subcommand)]
    Collection(CollectionCmd),
    /// Sources: the directories the index was built from; `watch` follows the watched ones.
    #[command(subcommand)]
    Source(SourceCmd),
    /// Make faces visible to other applications, in place. Persistent for the user unless
    /// `--session`. Exit code 2 when a conflict blocks it (see `conflicts`).
    Activate {
        /// Face ids, `family:<name>`, or indexed file paths (`path#index` for one face
        /// of a collection).
        #[arg(required = true)]
        targets: Vec<String>,
        /// Until logout or reboot instead of persistently.
        #[arg(long)]
        session: bool,
        /// Deactivate or uninstall conflicting faces that fontina manages first.
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        json: bool,
    },
    /// Undo `activate`.
    Deactivate {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Copy faces into the per-user font directory. Exit code 2 on an unresolved conflict.
    Install {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove the per-user copies made by `install`.
    Uninstall {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Faces that would clash with these once active: same PostScript name or same
    /// family and style, already active or living in an OS font directory.
    Conflicts {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Everything fontina has activated or installed.
    Activations {
        #[arg(long)]
        json: bool,
    },
    /// Re-apply recorded activations, for a login agent or after a reboot.
    Restore {
        #[arg(long)]
        json: bool,
    },
    /// The optional login agent that runs `restore` when you log in. Off until you
    /// install it, per-user, and removable with one command.
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Follow the watched sources (and any extra directories) and keep the index
    /// current until interrupted. One line per batch of changes; `--json` for one
    /// JSON object per line.
    Watch {
        /// Extra directories to follow for this run.
        paths: Vec<PathBuf>,
        /// Quiet period in milliseconds before a batch is applied.
        #[arg(long, default_value_t = 500)]
        debounce_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// Show everything known about a face, by index id or by file path (parses the file when not indexed).
    Info {
        /// Face id from `list`, or a path to a font file.
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Report faces that are duplicates across containers or share a PostScript name.
    Dupes {
        #[arg(long)]
        json: bool,
    },
    /// Faces whose character coverage overlaps a target's, most alike first.
    ///
    /// The declared family is often the wrong unit for "these belong together", and
    /// nothing here stores a guess about what the right one is: this asks about one face
    /// and answers from the coverage already indexed. The score and the metrics are
    /// printed together, because covering the same characters is not the same as being
    /// the same design — you draw that line, not fontina.
    ///
    /// `dupes` sweeps the whole library for exact identity, which is one pass over a
    /// hash. Similarity is pairwise, so it answers about a target instead.
    Variants {
        /// A face id, `family:<name>`, or an indexed file path.
        target: String,
        /// Only candidates overlapping at least this much, 0.0 to 1.0.
        #[arg(long, default_value = "0.5")]
        min: f64,
        #[arg(long)]
        json: bool,
    },
    /// Emit `@font-face` rules for faces by id, or for every face in a file.
    Css {
        /// Face ids or font file paths.
        targets: Vec<String>,
        /// Use this URL prefix instead of file:// paths (e.g. `/fonts/`).
        #[arg(long)]
        url_prefix: Option<String>,
    },
    /// Index statistics and recent failures.
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Print the operating system's font directories.
    Dirs {
        #[arg(long)]
        json: bool,
    },
    /// Run health checks (fontbakery-lite) on faces. Exit code 1 if any check errors.
    Check {
        /// Face ids or font file paths.
        targets: Vec<String>,
        /// Also fail on warnings.
        #[arg(long)]
        strict: bool,
        /// Hide findings below this level: info, warn or error.
        #[arg(long, default_value = "info")]
        min: String,
        #[arg(long)]
        json: bool,
    },
    /// Find indexed faces whose character map covers every character of a text.
    Covers {
        text: String,
        /// Only variable fonts.
        #[arg(long)]
        variable: bool,
        #[arg(long)]
        under: Option<String>,
        #[arg(long, short = 'n')]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Show a face's character coverage by Unicode block.
    Glyphs {
        target: String,
        /// Print the characters of one block (case-insensitive substring of the block name).
        #[arg(long)]
        block: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// License and embedding report: SPDX identifier, embedding rights, reserved font names.
    License {
        /// Face ids or font file paths; every indexed face when omitted.
        targets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Write a self-contained HTML specimen: waterfall, script samples, axis sliders,
    /// feature toggles, glyph map, and side-by-side comparison for several faces.
    Specimen {
        /// Face ids or font file paths.
        targets: Vec<String>,
        /// Output file; `-` for stdout.
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,
        /// Sample text.
        #[arg(long)]
        text: Option<String>,
        /// Reference font files by path instead of embedding them (smaller, but only
        /// works when served over HTTP or in browsers that allow file:// font loads).
        #[arg(long)]
        link: bool,
        #[arg(long)]
        title: Option<String>,
    },
    /// Show faces as real, shaped glyphs in the terminal (kitty, iTerm2 or sixel
    /// images, or half-block text anywhere), or write a PNG.
    Preview(PreviewArgs),
    /// Browse the index: facets, families, faces, details and previews, keyboard first.
    Ui,
    /// Print shell completions: bash, zsh, fish, elvish or powershell.
    Completions { shell: clap_complete::Shell },
    /// Print the man page, or write one page per command into a directory.
    Man {
        /// Write `fontina.1`, `fontina-scan.1`, ... here instead of printing `fontina.1`.
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    /// Show the settings in force, and where each one came from.
    Config {
        /// Print the path of the configuration file and nothing else.
        #[arg(long)]
        path: bool,
        /// Print a commented file holding every setting, to save and edit.
        #[arg(long)]
        example: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print a JSON Schema: `face` (default), `collection`, or `cli-output`.
    Schema {
        #[arg(default_value = "face")]
        which: String,
    },
}

#[derive(Args)]
struct PreviewArgs {
    /// Face ids, `family:<name>`, or font file paths.
    #[arg(required = true)]
    targets: Vec<String>,
    /// Sample text; `\n` for a new line. Defaults to a pangram, or the face's own
    /// sample text when it has one.
    #[arg(long, short = 't')]
    text: Option<String>,
    /// Font size in pixels [default: 48, or preview.size in the config file].
    #[arg(long, short = 's')]
    size: Option<f32>,
    /// Variable axis setting, e.g. `wght=700`; repeatable.
    #[arg(long = "axis", short = 'a', value_parser = parse_axis)]
    axes: Vec<(String, f32)>,
    /// OpenType feature to turn on (`smcp`) or off (`liga=0`); repeatable.
    #[arg(long = "feature", short = 'f', value_parser = parse_feature)]
    features: Vec<(String, bool)>,
    /// Output protocol: auto, kitty, iterm, sixel, blocks, or png (needs --output)
    /// [default: auto, or preview.protocol in the config file].
    #[arg(long, short = 'p')]
    protocol: Option<String>,
    /// Write a PNG here instead of drawing in the terminal (one face only).
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
    /// Ink colour, `#rrggbb`.
    #[arg(long)]
    fg: Option<String>,
    /// Background colour for sixel and blocks, `#rrggbb`.
    #[arg(long)]
    bg: Option<String>,
    /// Clip to this many pixels wide.
    #[arg(long)]
    max_width: Option<u32>,
}

fn parse_axis(s: &str) -> std::result::Result<(String, f32), String> {
    let (tag, value) = s
        .split_once('=')
        .ok_or_else(|| format!("expected tag=value, got {s:?}"))?;
    let v: f32 = value
        .trim()
        .parse()
        .map_err(|_| format!("bad axis value {value:?}"))?;
    Ok((tag.trim().to_string(), v))
}

fn parse_feature(s: &str) -> std::result::Result<(String, bool), String> {
    let (tag, on) = match s.split_once('=') {
        Some((t, v)) => (t, !matches!(v.trim(), "0" | "off" | "false")),
        None => (s, true),
    };
    let tag = tag.trim();
    if tag.len() != 4 {
        return Err(format!("{tag:?} is not a four-character feature tag"));
    }
    Ok((tag.to_string(), on))
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Write the agent for this system. Prints where it went and, where the system
    /// needs one, the command that starts it now rather than at the next login.
    Install {
        #[arg(long)]
        json: bool,
    },
    /// Remove it.
    Uninstall {
        #[arg(long)]
        json: bool,
    },
    /// Whether one is installed, and what it would contain if it were not.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TagCmd {
    /// All tags with their face counts.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a tag to faces (created if new).
    Add {
        tag: String,
        /// Face ids, `family:<name>`, or indexed file paths (`path#index` for one face
        /// of a collection).
        #[arg(required = true)]
        targets: Vec<String>,
    },
    /// Remove a tag from faces.
    Remove {
        tag: String,
        #[arg(required = true)]
        targets: Vec<String>,
    },
    Rename {
        old: String,
        new: String,
    },
    /// Delete a tag from every face.
    Delete {
        tag: String,
    },
    /// Copy tags between fontina's index and the files themselves, in one direction.
    ///
    /// A tag in the index is fast and searchable and invisible to everything else. A tag
    /// on the file is one your file manager shows and a backup carries. This moves them
    /// across: macOS Finder tags, `user.xdg.tags` on GNU/Linux, nothing on Windows.
    ///
    /// The direction is required, and the side you name wins: the other is made to match
    /// it, tags removed as well as added. Two tag sets with no common ancestor cannot
    /// tell a deletion from an addition, so guessing would lose tags quietly. Use
    /// `--dry-run` first.
    Sync {
        /// The index is right: write its tags onto the files.
        #[arg(long)]
        to_files: bool,
        /// The files are right: read their tags into the index.
        #[arg(long, conflicts_with = "to_files")]
        from_files: bool,
        /// Face ids, `family:<name>`, or indexed file paths (`path#index` for one face
        /// of a collection). Everything, by default.
        targets: Vec<String>,
        /// Say what would change, and change nothing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CollectionCmd {
    /// All collections with their face counts.
    List {
        #[arg(long)]
        json: bool,
    },
    Create {
        name: String,
    },
    Delete {
        name: String,
    },
    Rename {
        old: String,
        new: String,
    },
    /// Append faces to a collection (created if missing).
    Add {
        name: String,
        /// Face ids, `family:<name>`, or indexed file paths (`path#index` for one face
        /// of a collection).
        #[arg(required = true)]
        targets: Vec<String>,
    },
    Remove {
        name: String,
        #[arg(required = true)]
        targets: Vec<String>,
    },
    /// The faces of a collection, in order.
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Write a collection as JSON (`schemas/collection.json`).
    Export {
        name: String,
        /// Output file; `-` for stdout.
        #[arg(default_value = "-")]
        output: PathBuf,
        /// Write a bundle in this directory instead: the JSON beside a copy of every
        /// font, with relative paths, so the collection can be handed to somebody else.
        #[arg(long, value_name = "DIR", conflicts_with = "output")]
        bundle: Option<PathBuf>,
        /// Print what the bundle write did, rather than a line of prose. Without
        /// `--bundle` the export is JSON already.
        #[arg(long, requires = "bundle")]
        json: bool,
    },
    /// Read a collection into this index, matching faces by identity hash, PostScript
    /// name, then path.
    Import {
        /// Input file, a bundle directory, or `-` for stdin.
        #[arg(default_value = "-")]
        input: PathBuf,
        /// Import under this name instead of the one in the file.
        #[arg(long)]
        name: Option<String>,
        /// Do not apply the tags stored in the file.
        #[arg(long)]
        no_tags: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    /// Register a directory and scan it now.
    Add {
        path: PathBuf,
        /// Register without following it in `watch`.
        #[arg(long)]
        no_watch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Forget a directory; with `--purge`, drop its faces from the index too.
    Remove {
        path: PathBuf,
        #[arg(long)]
        purge: bool,
    },
    /// Turn watching on (default) or off for a source.
    Watch {
        path: PathBuf,
        #[arg(long)]
        off: bool,
    },
}

#[derive(Args)]
struct ListArgs {
    /// Full-text query over family, style, PostScript name and designer.
    query: Option<String>,
    #[command(flatten)]
    filter: FilterArgs,
    #[arg(long, short = 'n')]
    limit: Option<usize>,
    #[arg(long)]
    json: bool,
}

impl ListArgs {
    fn to_filter(&self) -> FaceFilter {
        FaceFilter {
            query: self.query.clone(),
            limit: self.limit,
            ..self.filter.to_filter()
        }
    }
}

/// Filters shared by `list`, `families`, `facets` and `covers`.
#[derive(Args, Clone, Default)]
struct FilterArgs {
    /// Exact family name.
    #[arg(long)]
    family: Option<String>,
    /// Only variable fonts (or `--variable=false` for static only).
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    variable: Option<bool>,
    /// Only color fonts.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    color: Option<bool>,
    /// Only italic/oblique faces.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    italic: Option<bool>,
    /// Faces covering this script (ISO 15924, e.g. Arab, Cyrl, Hani). Repeatable, and
    /// every one of them must be covered: `--script Cyrl --script Grek` is a face that
    /// has both, not either.
    #[arg(long)]
    script: Vec<String>,
    /// How many codepoints of each `--script` the face must have. A font with three
    /// Arabic codepoints is not an Arabic font.
    #[arg(long, value_name = "N", requires = "script")]
    script_min: Option<u32>,
    /// Faces claiming this language. Two namespaces, and the tag says which: an OpenType
    /// language system tag (TRK, VIT, BGR) means the shaping engine has rules for it; a
    /// BCP 47 tag on a name record (tr, vi, bg) means only that the font names itself
    /// in it.
    #[arg(long, value_name = "TAG")]
    lang: Option<String>,
    /// Which kind of claim `--lang` means: `opentype` or `name`.
    #[arg(long, value_name = "KIND", requires = "lang", value_parser = parse_lang_source)]
    lang_source: Option<LanguageSource>,
    /// Only faces the font itself calls monospaced (`post.isFixedPitch`).
    #[arg(long, conflicts_with = "proportional")]
    mono: bool,
    /// Only faces it does not.
    #[arg(long)]
    proportional: bool,
    /// SPDX license prefix, e.g. OFL, Apache, LicenseRef-Proprietary.
    #[arg(long)]
    license: Option<String>,
    /// Only fonts whose license grants the four freedoms. Short for `--freedom free`.
    #[arg(long, conflicts_with_all = ["nonfree", "freedom"])]
    free: bool,
    /// Only fonts whose license withholds one of them. Short for `--freedom nonfree`.
    #[arg(long, conflicts_with = "freedom")]
    nonfree: bool,
    /// free, nonfree, unknown (a license nobody has ruled free) or unstated (none at all).
    #[arg(long, value_name = "STATE", value_parser = parse_freedom)]
    freedom: Option<Freedom>,
    /// Weight range, e.g. 600-900.
    #[arg(long, value_parser = parse_range)]
    weight: Option<(u16, u16)>,
    /// Width range in percent, e.g. 50-87.
    #[arg(long, value_parser = parse_range)]
    width: Option<(u16, u16)>,
    /// `OS/2` vendor id, e.g. GOOG, ADBE.
    #[arg(long)]
    vendor: Option<String>,
    /// Faces carrying this tag.
    #[arg(long)]
    tag: Option<String>,
    /// Faces in this collection.
    #[arg(long)]
    collection: Option<String>,
    /// Only faces activated or installed through fontina (`--active=false` for the rest).
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    active: Option<bool>,
    /// Only faces in this activation state: session, user or installed.
    #[arg(long, value_parser = parse_state)]
    activation: Option<ActivationState>,
    /// Container: ttf, otf, ttc, woff or woff2.
    #[arg(long)]
    container: Option<String>,
    /// Only faces whose path starts with this prefix.
    #[arg(long)]
    under: Option<String>,
}

impl FilterArgs {
    /// `--free` and `--nonfree` are shorthands for the corresponding `--freedom`; clap
    /// has already rejected any combination of the three.
    fn freedom(&self) -> Option<Freedom> {
        self.freedom
            .or(self.free.then_some(Freedom::Free))
            .or(self.nonfree.then_some(Freedom::Nonfree))
    }

    fn to_filter(&self) -> FaceFilter {
        FaceFilter {
            family: self.family.clone(),
            variable: self.variable,
            color: self.color,
            italic: self.italic,
            scripts: self.script.clone(),
            script_min: self.script_min,
            lang: self.lang.clone(),
            lang_source: self.lang_source,
            monospace: match (self.mono, self.proportional) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            },
            license: self.license.clone(),
            freedom: self.freedom(),
            weight: self.weight,
            width: self.width,
            vendor: self.vendor.clone(),
            tag: self.tag.clone(),
            collection: self.collection.clone(),
            active: self.active,
            activation: self.activation,
            container: self.container.clone(),
            path_prefix: self.under.clone(),
            ..Default::default()
        }
    }
}

fn parse_freedom(s: &str) -> std::result::Result<Freedom, String> {
    s.parse()
}

/// A source that will not parse used to be silently dropped, and a dropped source
/// *widens* the filter to "either kind of claim" — so `--lang-source Opentype` answered
/// the opposite question and said nothing about it.
fn parse_lang_source(s: &str) -> std::result::Result<LanguageSource, String> {
    s.parse()
        .map_err(|_| format!("unknown source {s:?}; use opentype or name"))
}

fn parse_state(s: &str) -> std::result::Result<ActivationState, String> {
    s.parse()
        .map_err(|_| format!("unknown state {s:?}; use session, user or installed"))
}

fn parse_range(s: &str) -> std::result::Result<(u16, u16), String> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let lo: u16 = a.trim().parse().map_err(|_| format!("bad weight: {a}"))?;
    let hi: u16 = b.trim().parse().map_err(|_| format!("bad weight: {b}"))?;
    Ok((lo.min(hi), lo.max(hi)))
}

/// Die on a closed pipe, the way every other Unix program does.
///
/// Rust sets `SIGPIPE` to ignore before `main`, so writing to a pipe whose reader has
/// gone returns `EPIPE`, and `println!` turns that into a panic: `fontina list | head`
/// printed "failed printing to stdout: Broken pipe" and exited 101. Restoring the
/// default disposition makes the process end quietly on signal 13, which is what a shell
/// and every tool in a pipeline expect.
#[cfg(unix)]
fn die_on_broken_pipe() {
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    // SAFETY: restoring a signal's default disposition, before any thread is spawned.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn die_on_broken_pipe() {}

fn main() {
    die_on_broken_pipe();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn open_index(cli: &Cli) -> Result<Index> {
    // `--db` (which clap also fills from FONTINA_DB), then the config file, then the
    // platform data directory.
    let path = match &cli.db {
        Some(p) => p.clone(),
        None => match config::load()?.config.index.db {
            Some(p) => config::expand(&p),
            None => Index::default_path(),
        },
    };
    Index::open(&path).with_context(|| format!("opening index at {}", path.display()))
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Scan {
            paths,
            system,
            force,
            follow_symlinks,
            prune,
            json,
        } => {
            let cfg = config::load()?.config.scan;
            // A bare `scan` uses the sources in the config file, if it names any. Passing
            // paths overrides them: the file is a default, never an addition, so what a
            // command touches is always what its arguments say.
            let configured: Vec<PathBuf> = cfg
                .sources
                .unwrap_or_default()
                .iter()
                .map(|s| config::expand(s))
                .collect();
            let paths: &Vec<PathBuf> = if paths.is_empty() && !configured.is_empty() {
                &configured
            } else {
                paths
            };
            let system = *system || (cfg.system.unwrap_or(false) && paths.is_empty());
            let system_roots: Vec<PathBuf> = if system {
                fontina_platform::system_font_dirs()
                    .into_iter()
                    .map(|d| d.path)
                    .filter(|p| p.exists())
                    .collect()
            } else {
                Vec::new()
            };
            if paths.is_empty() && system_roots.is_empty() {
                bail!(
                    "nothing to scan: pass directories, or --system, or set scan.sources in {}",
                    config::path().display()
                );
            }
            let mut index = open_index(&cli)?;
            let opts = ScanOptions {
                force: *force,
                follow_symlinks: *follow_symlinks,
                prune: *prune,
                kind: None,
            };
            let started = std::time::Instant::now();
            let mut report = fontina_core::ScanReport::default();
            if !paths.is_empty() {
                report = fontina_core::scan::scan(&mut index, paths, &opts)?;
            }
            if !system_roots.is_empty() {
                let sys = fontina_core::scan::scan(
                    &mut index,
                    &system_roots,
                    &ScanOptions {
                        kind: Some(SourceKind::System),
                        ..opts.clone()
                    },
                )?;
                report.candidates += sys.candidates;
                report.parsed += sys.parsed;
                report.faces += sys.faces;
                report.unchanged += sys.unchanged;
                report.removed += sys.removed;
                report.failed.extend(sys.failed);
            }
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "scanned {} candidates in {:.2}s: {} parsed ({} faces), {} unchanged, {} removed, {} failed",
                    report.candidates,
                    started.elapsed().as_secs_f64(),
                    report.parsed,
                    report.faces,
                    report.unchanged,
                    report.removed,
                    report.failed.len()
                );
                for f in &report.failed {
                    eprintln!("  ! {}: {}", f.path, f.error);
                }
            }
        }
        Command::List(args) => {
            let index = open_index(&cli)?;
            let faces = index.list(&args.to_filter())?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&faces)?);
            } else {
                print_table(&faces);
            }
        }
        Command::Families(args) => {
            let index = open_index(&cli)?;
            let families = index.families(&args.to_filter())?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&families)?);
            } else {
                print_families(&families);
            }
        }
        Command::Facets(args) => {
            let index = open_index(&cli)?;
            let facets = index.facets(&args.to_filter())?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&facets)?);
            } else {
                print_facets(&facets);
            }
        }
        Command::Tag(cmd) => run_tag(&cli, cmd)?,
        Command::Collection(cmd) => run_collection(&cli, cmd)?,
        Command::Source(cmd) => run_source(&cli, cmd)?,
        Command::Activate {
            targets,
            session,
            replace,
            json,
        } => {
            let state = if *session {
                ActivationState::Session
            } else {
                ActivationState::User
            };
            run_activate(&cli, targets, state, *replace, *json)?
        }
        Command::Install {
            targets,
            replace,
            json,
        } => run_activate(&cli, targets, ActivationState::Installed, *replace, *json)?,
        Command::Deactivate { targets, json } => run_deactivate(&cli, targets, false, *json)?,
        Command::Uninstall { targets, json } => run_deactivate(&cli, targets, true, *json)?,
        Command::Conflicts { targets, json } => {
            let index = open_index(&cli)?;
            let ids = resolve_all_ids(&index, targets)?;
            let conflicts = collect_conflicts(&index, &ids)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&conflicts)?);
            } else if conflicts.is_empty() {
                println!("no conflicts");
                note_the_blind_spot(&index);
            } else {
                print_conflicts(&conflicts);
                std::process::exit(2);
            }
        }
        Command::Activations { json } => {
            let index = open_index(&cli)?;
            let records = index.activations()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&records)?);
            } else if records.is_empty() {
                println!("nothing activated or installed through fontina");
            } else {
                for r in &records {
                    println!(
                        "{:<10} [{}] {} {}  {}",
                        r.state.as_str(),
                        r.face.id,
                        r.face.family,
                        r.face.subfamily,
                        r.installed_path.as_deref().unwrap_or(&r.face.path)
                    );
                }
                println!("{} face(s)", records.len());
            }
        }
        Command::Restore { json } => run_restore(&cli, *json)?,
        Command::Agent(cmd) => run_agent(&cli, cmd)?,
        Command::Watch {
            paths,
            debounce_ms,
            json,
        } => {
            let mut index = open_index(&cli)?;
            let mut roots: Vec<PathBuf> = index
                .sources()?
                .into_iter()
                .filter(|s| s.watch && std::path::Path::new(&s.path).is_dir())
                .map(|s| PathBuf::from(s.path))
                .collect();
            roots.extend(paths.iter().cloned());
            if roots.is_empty() {
                bail!(
                    "nothing to watch: add a source (`fontina source add <dir>`) or pass directories"
                );
            }
            if !*json {
                for r in &roots {
                    eprintln!("watching {}", r.display());
                }
            }
            fontina_core::watch::watch(
                &mut index,
                &roots,
                &fontina_core::watch::WatchOptions {
                    debounce: std::time::Duration::from_millis(*debounce_ms),
                    ..Default::default()
                },
                |ev| {
                    if *json {
                        println!("{}", serde_json::to_string(ev).unwrap_or_default());
                    } else {
                        println!(
                            "{} path(s): {} parsed ({} faces), {} unchanged, {} removed, {} failed",
                            ev.paths.len(),
                            ev.report.parsed,
                            ev.report.faces,
                            ev.report.unchanged,
                            ev.report.removed,
                            ev.report.failed.len()
                        );
                        for f in &ev.report.failed {
                            eprintln!("  ! {}: {}", f.path, f.error);
                        }
                    }
                    true
                },
            )?;
        }
        Command::Info { target, json } => {
            let faces = resolve_faces(&cli, target)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&faces)?);
            } else {
                for f in &faces {
                    print_info(f);
                }
            }
        }
        Command::Variants { target, min, json } => {
            if !(0.0..=1.0).contains(min) {
                bail!("--min is a similarity between 0.0 and 1.0, not {min}");
            }
            let index = open_index(&cli)?;
            let ids = resolve_ids(&index, target)?;
            let Some(&id) = ids.first() else {
                bail!("{target} matches no indexed face");
            };
            if ids.len() > 1 {
                eprintln!(
                    "{target} matches {} faces; asking about the first ({id})",
                    ids.len()
                );
            }
            let related = index.related(id, *min)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&related)?);
            } else if related.is_empty() {
                println!("nothing overlaps it by {min} or more");
            } else {
                print_variants(&index, id, &related)?;
            }
        }
        Command::Dupes { json } => {
            let index = open_index(&cli)?;
            let groups = index.duplicates()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&groups)?);
            } else if groups.is_empty() {
                println!("no duplicates");
            } else {
                for g in &groups {
                    println!(
                        "{} ({}):",
                        g.reason,
                        g.key.chars().take(16).collect::<String>()
                    );
                    for f in &g.faces {
                        println!(
                            "  [{}] {} {}  {}#{}",
                            f.id, f.family, f.subfamily, f.path, f.index
                        );
                    }
                }
            }
        }
        Command::Css {
            targets,
            url_prefix,
        } => {
            if targets.is_empty() {
                bail!("pass face ids or font file paths");
            }
            for t in &expand_targets(targets)? {
                for face in resolve_faces(&cli, t)? {
                    let url = url_prefix.as_ref().map(|p| {
                        let name = std::path::Path::new(&face.file.path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        format!("{}{}", p, name)
                    });
                    print!(
                        "{}",
                        fontina_core::css::font_face_rule(&face, url.as_deref())
                    );
                }
            }
        }
        Command::Stats { json } => {
            let index = open_index(&cli)?;
            let stats = index.stats()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!("index:     {}", stats.db_path);
                println!("files:     {}", stats.files);
                println!("faces:     {}", stats.faces);
                println!("families:  {}", stats.families);
                println!("variable:  {}", stats.variable_faces);
                println!("color:     {}", stats.color_faces);
                println!("failed:    {}", stats.failed_files);
                println!("tags:      {}", stats.tags);
                println!("collections: {}", stats.collections);
                println!("sources:   {}", stats.sources);
                println!("active:    {}", stats.activations);
                for (p, e) in index.failures()?.iter().take(20) {
                    println!("  ! {p}: {e}");
                }
            }
        }
        Command::Dirs { json } => {
            let dirs = fontina_platform::system_font_dirs();
            if *json {
                println!("{}", serde_json::to_string_pretty(&dirs)?);
            } else {
                for d in dirs {
                    println!(
                        "{} {}{}",
                        pad(&d.path.display().to_string(), 60),
                        d.description,
                        if d.user_writable {
                            " (install target)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        Command::Check {
            targets,
            strict,
            min,
            json,
        } => {
            if targets.is_empty() {
                bail!("pass face ids or font file paths");
            }
            let min_sev = match min.as_str() {
                "info" => fontina_core::Severity::Info,
                "warn" | "warning" => fontina_core::Severity::Warn,
                "error" => fontina_core::Severity::Error,
                other => bail!("unknown level {other:?}; use info, warn or error"),
            };
            let mut reports = Vec::new();
            for t in &expand_targets(targets)? {
                for face in resolve_faces(&cli, t)? {
                    let mut r = fontina_core::check_face(&face);
                    r.findings.retain(|f| f.severity >= min_sev);
                    reports.push(r);
                }
            }
            let failed = reports.iter().filter(|r| !r.passed(*strict)).count();
            if *json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                for r in &reports {
                    let status = if r.passed(*strict) { "PASS" } else { "FAIL" };
                    println!(
                        "{status}  {} {}  ({}#{})  {} error(s), {} warning(s)",
                        r.family, r.subfamily, r.path, r.index, r.errors, r.warnings
                    );
                    for f in &r.findings {
                        let tag = match f.severity {
                            fontina_core::Severity::Error => "ERROR",
                            fontina_core::Severity::Warn => "WARN ",
                            fontina_core::Severity::Info => "info ",
                        };
                        println!("  {tag} {:<22} {}", f.id, f.message);
                    }
                }
                println!("{} face(s) checked, {} failed", reports.len(), failed);
            }
            if failed > 0 {
                std::process::exit(1);
            }
        }
        Command::Covers {
            text,
            variable,
            under,
            limit,
            json,
        } => {
            let index = open_index(&cli)?;
            let filter = FaceFilter {
                variable: if *variable { Some(true) } else { None },
                path_prefix: under.clone(),
                limit: *limit,
                ..Default::default()
            };
            let faces = index.covering(text, &filter)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&faces)?);
            } else {
                let n = text
                    .chars()
                    .filter(|c| !c.is_whitespace() && !c.is_control())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                println!("{} distinct character(s)", n);
                print_table(&faces);
            }
        }
        Command::Glyphs {
            target,
            block,
            json,
        } => {
            let faces = resolve_faces(&cli, target)?;
            for face in &faces {
                let blocks = fontina_core::unicode::glyph_map(&face.coverage.ranges);
                if let Some(name) = block {
                    let needle = name.to_ascii_lowercase();
                    let hits: Vec<_> = blocks
                        .iter()
                        .filter(|b| b.block.to_ascii_lowercase().contains(&needle))
                        .collect();
                    if hits.is_empty() {
                        bail!("no covered block matches {name:?}");
                    }
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&hits)?);
                    } else {
                        for b in hits {
                            println!(
                                "{} (U+{:04X}–U+{:04X}): {} of {}",
                                b.block,
                                b.start,
                                b.end,
                                b.codepoints.len(),
                                b.block_size
                            );
                            let chars: Vec<char> = b
                                .codepoints
                                .iter()
                                .map(|&c| fontina_core::unicode::cell_for(c).glyph)
                                .collect();
                            for chunk in chars.chunks(64) {
                                println!("  {}", chunk.iter().collect::<String>());
                            }
                        }
                    }
                } else if *json {
                    println!("{}", serde_json::to_string_pretty(&blocks)?);
                } else {
                    println!(
                        "{} {}: {} codepoints in {} blocks",
                        face.names.family,
                        face.names.subfamily,
                        face.coverage.codepoints,
                        blocks.len()
                    );
                    for b in &blocks {
                        println!(
                            "  {:<44} U+{:04X}–U+{:04X}  {:>5} / {:<5} {:>3}%",
                            b.block,
                            b.start,
                            b.end,
                            b.codepoints.len(),
                            b.block_size,
                            b.codepoints.len() * 100 / b.block_size as usize
                        );
                    }
                }
            }
        }
        Command::License { targets, json } => {
            let faces: Vec<fontina_core::FaceMetadata> = if targets.is_empty() {
                let index = open_index(&cli)?;
                let mut out = Vec::new();
                for s in index.list(&FaceFilter::default())? {
                    if let Some(f) = index.get_face(s.id)? {
                        out.push(f);
                    }
                }
                out
            } else {
                let mut out = Vec::new();
                for t in &expand_targets(targets)? {
                    out.extend(resolve_faces(&cli, t)?);
                }
                out
            };
            let rows: Vec<LicenseRow> = faces
                .iter()
                .map(|f| {
                    let v = fontina_core::freedom::assess(f.license.spdx.as_deref());
                    LicenseRow {
                        family: &f.names.family,
                        subfamily: &f.names.subfamily,
                        path: &f.file.path,
                        spdx: f.license.spdx.as_deref(),
                        freedom: v.freedom,
                        reason: v.reason,
                        embedding: f.os2.as_ref().map(|o| &o.embedding),
                        reserved_font_names: &f.license.reserved_font_names,
                        url: f.license.url.as_deref(),
                        copyright: f.names.copyright.as_deref(),
                    }
                })
                .collect();
            if *json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                let mut by: std::collections::BTreeMap<String, Vec<&LicenseRow>> =
                    Default::default();
                for r in &rows {
                    by.entry(r.spdx.unwrap_or("(none embedded)").to_string())
                        .or_default()
                        .push(r);
                }
                for (spdx, rs) in &by {
                    let v = rs[0];
                    println!(
                        "{spdx}  [{}]  ({} face(s))\n  {}",
                        v.freedom,
                        rs.len(),
                        v.reason
                    );
                    for r in rs {
                        let emb = r
                            .embedding
                            .map(|e| format!("{:?}", e.level))
                            .unwrap_or_else(|| "-".into());
                        let rfn = if r.reserved_font_names.is_empty() {
                            String::new()
                        } else {
                            format!("  RFN: {}", r.reserved_font_names.join(", "))
                        };
                        println!("  {} {}  [{emb}]{rfn}  {}", r.family, r.subfamily, r.path);
                    }
                }
                let mut tally: Vec<String> = Vec::new();
                for f in Freedom::ALL {
                    let n = rows.iter().filter(|r| r.freedom == f).count();
                    if n > 0 {
                        tally.push(format!("{n} {f}"));
                    }
                }
                println!("\n{}", tally.join(", "));
            }
        }
        Command::Specimen {
            targets,
            output,
            text,
            link,
            title,
        } => {
            if targets.is_empty() {
                bail!("pass face ids or font file paths");
            }
            let mut faces = Vec::new();
            for t in &expand_targets(targets)? {
                faces.extend(resolve_faces(&cli, t)?);
            }
            if !*link {
                note_what_is_embedded(&faces);
            }
            let html = fontina_core::specimen::render(
                &faces,
                &fontina_core::specimen::SpecimenOptions {
                    text: text.clone(),
                    link: *link,
                    title: title.clone(),
                },
            )?;
            if output.as_os_str() == "-" {
                print!("{html}");
            } else {
                std::fs::write(output, &html)
                    .with_context(|| format!("writing {}", output.display()))?;
                eprintln!(
                    "wrote {} ({} faces, {} KB)",
                    output.display(),
                    faces.len(),
                    html.len() / 1024
                );
            }
        }
        Command::Preview(args) => run_preview(&cli, args)?,
        Command::Ui => {
            let path = cli.db.clone().unwrap_or_else(Index::default_path);
            ui::run(&path)?
        }
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "fontina", &mut std::io::stdout());
        }
        Command::Man { out_dir } => {
            let cmd = Cli::command();
            match out_dir {
                Some(dir) => {
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("creating {}", dir.display()))?;
                    clap_mangen::generate_to(cmd, dir)
                        .with_context(|| format!("writing man pages to {}", dir.display()))?;
                    eprintln!("wrote man pages to {}", dir.display());
                }
                None => {
                    let mut out = Vec::new();
                    clap_mangen::Man::new(cmd).render(&mut out)?;
                    std::io::stdout().write_all(&out)?;
                }
            }
        }
        Command::Config {
            path,
            example,
            json,
        } => {
            if *example {
                print!("{}", config::EXAMPLE);
                return Ok(());
            }
            let loaded = config::load()?;
            if *path {
                println!("{}", loaded.path.display());
                return Ok(());
            }
            let settings = loaded.config.settings(cli.db.as_deref());
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ConfigReport {
                        path: loaded.path.clone(),
                        found: loaded.found,
                        settings: &settings,
                    })?
                );
                return Ok(());
            }
            println!(
                "{}{}",
                loaded.path.display(),
                if loaded.found {
                    ""
                } else {
                    "  (no file yet; `fontina config --example` prints one to save there)"
                }
            );
            println!();
            for s in settings {
                println!("{:<18} {:<44} {}", s.key, s.value, s.source.label());
            }
        }
        Command::Schema { which } => {
            let schema = match which.as_str() {
                "face" => fontina_core::face_schema(),
                "collection" => fontina_core::collection_schema(),
                "cli-output" | "cli_output" | "cli" => cli_output_schema(),
                other => bail!("unknown schema {other:?}; use face, collection or cli-output"),
            };
            println!("{}", serde_json::to_string_pretty(&schema)?);
        }
    }
    Ok(())
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct LicenseRow<'a> {
    family: &'a str,
    subfamily: &'a str,
    path: &'a str,
    spdx: Option<&'a str>,
    freedom: Freedom,
    /// Why `freedom` is what it is, in one line.
    reason: &'static str,
    embedding: Option<&'a fontina_core::model::EmbeddingRights>,
    reserved_font_names: &'a [String],
    url: Option<&'a str>,
    copyright: Option<&'a str>,
}

/// The login agent: write it, remove it, or say where it is.
///
/// Everything here stays inside the user's own directories and none of it needs
/// elevation, so installing the agent cannot affect anyone else on the machine.
fn run_agent(cli: &Cli, cmd: &AgentCmd) -> Result<()> {
    use fontina_platform::agent;
    match cmd {
        AgentCmd::Install { json } => {
            // The binary as it was invoked. Deliberately not canonicalised: on Homebrew
            // and Nix the invoked path is a stable symlink and its target is a
            // version-specific store path that the next upgrade deletes, so resolving it
            // would produce an agent that fails at every login after an update.
            let exe = std::env::current_exe()
                .context("cannot find this executable, so no login agent can point at it")?;
            // The index has to travel with it. Without this an agent installed by
            // someone who keeps their index elsewhere restores from the default one,
            // finds nothing, and reports success.
            let mut args = vec!["restore".to_string()];
            if let Some(db) = &cli.db {
                args.push("--db".into());
                args.push(db.display().to_string());
            }
            let plan = agent::install(&exe, &args)?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&AgentInstalled {
                        installed: true,
                        path: plan.path.clone(),
                        kind: plan.kind,
                        activate_with: plan.activate_with.clone(),
                    })?
                );
            } else {
                println!("wrote the {} to {}", plan.kind, plan.path.display());
                match &plan.activate_with {
                    Some(c) => println!("it starts at your next login; to start it now:  {c}"),
                    None => println!("it runs at your next login"),
                }
            }
        }
        AgentCmd::Uninstall { json } => {
            let plan = agent::plan(std::path::Path::new("/fontina"), &[]);
            let removed = agent::uninstall()?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&AgentRemoved {
                        removed,
                        deactivate_with: plan.as_ref().and_then(|p| p.deactivate_with.clone()),
                    })?
                );
            } else if removed {
                println!("removed the login agent");
                // Deleting the file does not undo the enablement: systemd keeps a
                // symlink that then fails at every login, and launchd keeps the job
                // loaded until logout.
                if let Some(c) = plan.as_ref().and_then(|p| p.deactivate_with.as_ref()) {
                    println!("if you enabled it, also run:  {c}");
                }
            } else {
                println!("no login agent was installed");
            }
        }
        AgentCmd::Status { json } => {
            let status = agent::status();
            let plan = agent::plan(std::path::Path::new("/fontina"), &[]);
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&AgentStatus {
                        installed: status.as_ref().is_some_and(|s| s.installed),
                        enabled: status.as_ref().is_some_and(|s| s.enabled),
                        path: status.as_ref().map(|s| s.path.clone()),
                        kind: plan.as_ref().map(|p| p.kind),
                    })?
                );
            } else {
                match (&status, &plan) {
                    (Some(s), Some(p)) if s.installed && s.enabled => {
                        println!("installed: {} at {}", p.kind, s.path.display())
                    }
                    (Some(s), Some(p)) if s.installed => {
                        // The file exists and the system has not been told to run it,
                        // which is not the same as being installed.
                        println!(
                            "written but not enabled: {} at {}",
                            p.kind,
                            s.path.display()
                        );
                        if let Some(c) = &p.activate_with {
                            println!("enable it with:  {c}");
                        }
                    }
                    (_, Some(p)) => println!(
                        "not installed; `fontina agent install` would write the {} to {}",
                        p.kind,
                        p.path.display()
                    ),
                    _ => println!("no home directory, so no login agent is possible here"),
                }
            }
        }
    }
    Ok(())
}

/// A target is a face id when numeric and not an existing path; otherwise a file path,
/// served from the index when present and parsed directly when not.
fn resolve_faces(cli: &Cli, target: &str) -> Result<Vec<fontina_core::FaceMetadata>> {
    if let Some(family) = target.strip_prefix("family:") {
        let index = open_index(cli)?;
        let ids = resolve_ids(&index, target)?;
        if ids.is_empty() {
            bail!("no indexed family named {family:?}");
        }
        return ids
            .iter()
            .filter_map(|id| index.get_face(*id).transpose())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
    }
    if split_face_index(target).is_some() {
        // One face of a collection, named the way a listing prints it. `resolve_ids`
        // knows how to read that; this only has to turn the ids into faces.
        let index = open_index(cli)?;
        let ids = resolve_ids(&index, target)?;
        return ids
            .iter()
            .filter_map(|id| index.get_face(*id).transpose())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
    }
    let path = PathBuf::from(target);
    if !path.exists() {
        if let Ok(id) = target.parse::<i64>() {
            let index = open_index(cli)?;
            return match index.get_face(id)? {
                Some(f) => Ok(vec![f]),
                None => bail!("no face with id {id}"),
            };
        }
        bail!(
            "{}: no such file, and not a face id",
            // A target this long is a shell that did not split the arguments, and the
            // message is read in a terminal: printing six kilobytes of ids on one line
            // buries the sentence that says what went wrong.
            fontina_core::unicode::fit(target, 120)
        );
    }
    let canonical = std::fs::canonicalize(&path)?;
    if let Ok(index) = open_index(cli) {
        let cached = index.faces_for_path(&canonical.to_string_lossy())?;
        if !cached.is_empty() {
            return Ok(cached);
        }
    }
    let (_, faces) = fontina_core::load_file(&canonical)?;
    Ok(faces)
}

fn print_table(faces: &[FaceSummary]) {
    if faces.is_empty() {
        println!("no faces match");
        return;
    }
    let w_fam = faces
        .iter()
        .map(|f| fontina_core::unicode::columns(&f.family))
        .max()
        .unwrap_or(6)
        .clamp(6, 40);
    let w_sub = faces
        .iter()
        .map(|f| fontina_core::unicode::columns(&f.subfamily))
        .max()
        .unwrap_or(5)
        .clamp(5, 28);
    // A variable face reaches further than the one number it reports, and a table that
    // shows only the default instance is the same omission the filter used to make.
    let wght: Vec<String> = faces
        .iter()
        .map(|f| axis_cell(f.weight, f.weight_range))
        .collect();
    let wdth: Vec<String> = faces
        .iter()
        .map(|f| axis_cell(f.width, f.width_range))
        .collect();
    let w_wght = wght.iter().map(|c| c.len()).max().unwrap_or(4).max(4);
    let w_wdth = wdth.iter().map(|c| c.len()).max().unwrap_or(4).max(4);
    let any_tags = faces.iter().any(|f| !f.tags.is_empty());
    // One `println!` per row is one `write` syscall per row: Rust's stdout is line
    // buffered whether or not it is a terminal. Listing five thousand faces spent more
    // time in the kernel than in the query. Lock it once and buffer the whole table.
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let _ = writeln!(
        out,
        "{:>6}  {:<w_fam$}  {:<w_sub$}  {:>w_wght$}  {:>w_wdth$}  {:<6}  {:<12}  path{}",
        "id",
        "family",
        "style",
        "wght",
        "wdth",
        "flags",
        "license",
        if any_tags { "  [tags]" } else { "" }
    );
    for (i, f) in faces.iter().enumerate() {
        // The flags column is exactly six characters, so it goes straight into the row
        // rather than through a `format!` and an allocation for every face listed.
        let _ = writeln!(
            out,
            "{:>6}  {}  {}  {:>w_wght$}  {:>w_wdth$}  {}{}{}{}{}{}  {}  {}{}{}",
            f.id,
            cell(&f.family, w_fam),
            cell(&f.subfamily, w_sub),
            wght[i],
            wdth[i],
            if f.variable { "V" } else { "-" },
            if f.color { "C" } else { "-" },
            if f.italic { "I" } else { "-" },
            if f.monospace { "M" } else { "-" },
            match f.activation {
                Some(ActivationState::Session) => "s",
                Some(ActivationState::User) => "u",
                Some(ActivationState::Installed) => "i",
                None => "-",
            },
            freedom_flag(f.freedom),
            cell(f.license.as_deref().unwrap_or("-"), 12),
            f.path,
            if f.index > 0 || f.container == "ttc" {
                format!("#{}", f.index)
            } else {
                String::new()
            },
            if f.tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", f.tags.join(", "))
            }
        );
    }
    let _ = writeln!(out, "{} face(s)", faces.len());
    // BufWriter swallows a failed flush in its destructor, and a closed pipe is the
    // ordinary way this ends; `die_on_broken_pipe` has already made that a signal.
    let _ = out.flush();
}

/// The candidates, with the four numbers that say whether the overlap means anything.
fn print_variants(index: &Index, target: i64, related: &[fontina_core::Related]) -> Result<()> {
    let of = |id: i64| -> Result<(String, String)> {
        let face = index
            .summaries(&[id])?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("face {id} vanished mid-query"))?;
        Ok((
            format!("{} {}", face.family, face.subfamily),
            face.path.clone(),
        ))
    };
    let (name, _) = of(target)?;
    println!("faces overlapping {name} (#{target}):");
    let w = related
        .iter()
        .map(|r| {
            fontina_core::unicode::columns(&r.face.family)
                + fontina_core::unicode::columns(&r.face.subfamily)
                + 1
        })
        .max()
        .unwrap_or(20)
        .clamp(20, 44);
    println!(
        "  {:>6}  {:<w$}  {:>7}  {:>7}  {:<8}  path",
        "id", "face", "overlap", "shared", "metrics"
    );
    for r in related {
        println!(
            "  {:>6}  {}  {:>6.2}%  {:>7}  {:<8}  {}",
            r.face.id,
            cell(&format!("{} {}", r.face.family, r.face.subfamily), w),
            r.overlap * 100.0,
            r.shared,
            if r.metrics_agree { "same" } else { "differ" },
            r.face.path
        );
    }
    println!(
        "{} face(s). `same` metrics means units per em, ascender, descender and spacing \
         all agree; high overlap with `differ` is two fonts that serve the same \
         languages, not two cuts of one typeface.",
        related.len()
    );
    Ok(())
}

/// One axis of a face for the table: the number it is, or the range it can be set to.
fn axis_cell(value: f32, range: Option<[f32; 2]>) -> String {
    match range {
        Some([lo, hi]) => format!("{}-{}", lo.round() as i64, hi.round() as i64),
        None => format!("{}", value.round() as i64),
    }
}

/// The fifth character of the `flags` column: `F` free, `N` nonfree, `?` a license
/// nobody has ruled on, `-` no license stated.
fn freedom_flag(f: Freedom) -> &'static str {
    match f {
        Freedom::Free => "F",
        Freedom::Nonfree => "N",
        Freedom::Unknown => "?",
        Freedom::Unstated => "-",
    }
}

/// Shorten to `n` characters, borrowing when it already fits. Listing a large library
/// formats two of these per row, and almost every one of them fits.
/// `s` padded to `w` terminal columns, and never cut short.
///
/// For a column whose content is the answer rather than a label: a path, a name someone
/// typed. `fontina dirs` is how a script asks where an install goes — `scripts/acceptance`
/// does exactly that — so a path longer than the column is a wider row, not a shorter
/// path. The columns after it lose their alignment on that row and keep it on every
/// other, which is the right trade when the alternative is printing something untrue.
fn pad(s: &str, w: usize) -> String {
    let mut out = fontina_core::unicode::fit(s, w.max(fontina_core::unicode::columns(s)));
    for _ in fontina_core::unicode::columns(&out)..w {
        out.push(' ');
    }
    out
}

/// One table cell: `s` fitted to `w` terminal columns and padded to exactly `w`.
///
/// Rust's own `{:<w$}` pads to a character count, and a character is not a column. A
/// family name in Japanese takes two columns per character, a name with a combining mark
/// takes none for the mark, and either way every column to the right of it lands
/// somewhere different on that row than on the row above. Fonts are named in every
/// script there is, so this is the ordinary case for anyone whose fonts are not all
/// Latin, not an exotic one.
///
/// `fontina_core::unicode::fit` also stands a replacement character in for anything that
/// would move the cursor or reverse the line, which a `name` table is free to contain.
fn cell(s: &str, w: usize) -> String {
    let mut out = fontina_core::unicode::fit(s, w);
    for _ in fontina_core::unicode::columns(&out)..w {
        out.push(' ');
    }
    out
}

fn print_info(f: &fontina_core::FaceMetadata) {
    let n = &f.names;
    println!("{} {}", n.family, n.subfamily);
    println!(
        "  file:        {}{}",
        f.file.path,
        if f.file.face_count > 1 {
            format!(" (face {} of {})", f.index, f.file.face_count)
        } else {
            String::new()
        }
    );
    println!(
        "  container:   {}  {} bytes  blake3 {}",
        f.file.container.as_str(),
        f.file.size,
        &f.file.blake3[..16]
    );
    if let Some(p) = &n.postscript_name {
        println!("  postscript:  {p}");
    }
    if let Some(v) = &n.version {
        println!("  version:     {v}");
    }
    if let Some(d) = &n.designer {
        println!("  designer:    {d}");
    }
    if let Some(m) = &n.manufacturer {
        println!("  vendor:      {m}");
    }
    println!(
        "  css:         weight {}; stretch {}; style {}",
        f.style.css.weight, f.style.css.stretch, f.style.css.style
    );
    println!(
        "  metrics:     {} upm, asc {} desc {} gap {}, italic angle {}",
        f.metrics.units_per_em,
        f.metrics.ascender,
        f.metrics.descender,
        f.metrics.line_gap,
        f.metrics.italic_angle
    );
    println!(
        "  outlines:    {:?}{}",
        f.capabilities.outlines,
        if f.capabilities.hinting {
            ", hinted"
        } else {
            ""
        }
    );
    if !f.capabilities.color.is_empty() {
        println!("  color:       {:?}", f.capabilities.color);
    }
    println!(
        "  glyphs:      {}   codepoints: {}",
        f.glyph_count, f.coverage.codepoints
    );
    let scripts: Vec<String> = f
        .coverage
        .scripts
        .iter()
        .take(8)
        .map(|s| format!("{} {}", s.script, s.codepoints))
        .collect();
    println!("  scripts:     {}", scripts.join(", "));
    // Two different claims, kept apart: the shaping engine's rules, and the languages the
    // font names itself in. Merging them would say more than the file does.
    let shaping: Vec<&str> = f
        .features
        .scripts
        .iter()
        .flat_map(|s| s.languages.iter().map(|l| l.trim()))
        .filter(|l| !l.is_empty())
        .collect();
    if !shaping.is_empty() {
        let mut tags: Vec<&str> = shaping;
        tags.sort_unstable();
        tags.dedup();
        println!(
            "  shaping for: {}{}",
            tags.iter().take(12).copied().collect::<Vec<_>>().join(" "),
            if tags.len() > 12 {
                format!(" +{} more", tags.len() - 12)
            } else {
                String::new()
            }
        );
    }
    let mut named: Vec<&str> = f
        .name_records
        .iter()
        .filter_map(|r| r.language.as_deref())
        .collect();
    named.sort_unstable();
    named.dedup();
    if !named.is_empty() {
        println!("  named in:    {}", named.join(" "));
    }
    if let Some(v) = &f.variable {
        println!(
            "  axes:        {}",
            v.axes
                .iter()
                .map(|a| format!("{} {}..{} (default {})", a.tag, a.min, a.max, a.default))
                .collect::<Vec<_>>()
                .join("; ")
        );
        if !v.instances.is_empty() {
            println!(
                "  instances:   {}",
                v.instances
                    .iter()
                    .filter_map(|i| i.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if !f.features.gsub.is_empty() {
        println!("  gsub:        {}", f.features.gsub.join(" "));
    }
    if !f.features.gpos.is_empty() {
        println!("  gpos:        {}", f.features.gpos.join(" "));
    }
    if let Some(o) = &f.os2 {
        println!(
            "  embedding:   {:?}{}{}",
            o.embedding.level,
            if o.embedding.no_subsetting {
                ", no subsetting"
            } else {
                ""
            },
            if o.embedding.bitmap_only {
                ", bitmap only"
            } else {
                ""
            }
        );
    }
    println!(
        "  license:     {}{}",
        f.license.spdx.as_deref().unwrap_or("(none embedded)"),
        f.license
            .url
            .as_ref()
            .map(|u| format!("  {u}"))
            .unwrap_or_default()
    );
    println!();
}

/// A target that names one face of a file: `path#index`, the way a listing prints it.
///
/// A collection is several faces in one file, so the listing has to say which face a row
/// is, and the obvious thing to do with a line of output is paste it back. Without this
/// that gives "no such file, and not a face id" for a path the reader is looking at.
///
/// A file whose own name ends in `#1` wins: the split happens only when the whole target
/// is not a file and the part before the `#` is.
fn split_face_index(target: &str) -> Option<(&str, u32)> {
    if std::path::Path::new(target).exists() {
        return None;
    }
    let (path, index) = target.rsplit_once('#')?;
    let index = index.parse().ok()?;
    std::path::Path::new(path).exists().then_some((path, index))
}

/// Face ids for a target that must already be indexed: a numeric id, `family:<name>`, or
/// a file path.
fn resolve_ids(index: &Index, target: &str) -> Result<Vec<i64>> {
    if let Some(family) = target.strip_prefix("family:") {
        let faces = index.list(&FaceFilter {
            family: Some(family.to_string()),
            ..Default::default()
        })?;
        if faces.is_empty() {
            bail!("no indexed family named {family:?}");
        }
        return Ok(faces.into_iter().map(|f| f.id).collect());
    }
    if let Some((file, want)) = split_face_index(target) {
        let canonical = std::fs::canonicalize(file)?;
        let ids = index.ids_for_path(&canonical.to_string_lossy())?;
        if ids.is_empty() {
            bail!("{file} is not indexed; run `fontina scan` on it first");
        }
        let mine: Vec<i64> = index
            .summaries(&ids)?
            .into_iter()
            .filter(|s| s.index == want)
            .map(|s| s.id)
            .collect();
        if mine.is_empty() {
            bail!("{file} has no face {want}");
        }
        return Ok(mine);
    }
    let path = PathBuf::from(target);
    if path.exists() {
        let canonical = std::fs::canonicalize(&path)?;
        let ids = index.ids_for_path(&canonical.to_string_lossy())?;
        if ids.is_empty() {
            bail!("{target} is not indexed; run `fontina scan` on it first");
        }
        return Ok(ids);
    }
    if let Ok(id) = target.parse::<i64>() {
        if index.summaries(&[id])?.is_empty() {
            bail!("no face with id {id}");
        }
        return Ok(vec![id]);
    }
    bail!(
        "{}: no such file, and not a face id",
        // A target this long is a shell that did not split the arguments, and the
        // message is read in a terminal: printing six kilobytes of ids on one line
        // buries the sentence that says what went wrong.
        fontina_core::unicode::fit(target, 120)
    )
}

fn resolve_all_ids(index: &Index, targets: &[String]) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    for t in &expand_targets(targets)? {
        ids.extend(resolve_ids(index, t)?);
    }
    ids.dedup();
    Ok(ids)
}

/// Replace a `-` among the targets with whatever is on standard input.
///
/// Every fontina command prints `--json` and every printed type is in
/// `schemas/cli-output.json`; this is the other half, so a program can pipe *into*
/// fontina as well as out of it:
///
/// ```text
/// fontina list --json --free | jq '[.[] | select(.variable)]' | fontina tag add variable -
/// fontina list --json | fontina tag add everything -
/// printf '1\n2\n' | fontina activate -
/// ```
///
/// Two shapes, told apart by the first non-blank character, because both are things a
/// person will reasonably try:
///
/// - **JSON**, starting `[` or `{`: an array, a single object, or one object per line as
///   `jq -c` writes them. An object is read for its `id`, or its `path` if it has no id,
///   which is exactly what `fontina list --json` produces. Bare numbers and strings are
///   taken as written.
/// - **otherwise, one target per line** — an id, `family:<name>`, or a path — with blank
///   lines and `#` comments ignored, so a hand-written list works too.
///
/// A second `-` contributes nothing: standard input is read once.
fn expand_targets(targets: &[String]) -> Result<Vec<String>> {
    if !targets.iter().any(|t| t == "-") {
        return Ok(targets.to_vec());
    }
    let text = std::io::read_to_string(std::io::stdin()).context("reading targets from stdin")?;
    let mut piped = Some(parse_targets(&text)?);
    let mut out = Vec::new();
    for t in targets {
        if t == "-" {
            out.extend(piped.take().unwrap_or_default());
        } else {
            out.push(t.clone());
        }
    }
    if out.is_empty() {
        bail!("nothing on standard input");
    }
    Ok(out)
}

fn parse_targets(text: &str) -> Result<Vec<String>> {
    match text.trim_start().chars().next() {
        Some('[') | Some('{') => targets_from_json(text),
        _ => Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()),
    }
}

fn targets_from_json(text: &str) -> Result<Vec<String>> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        return match value {
            serde_json::Value::Array(items) => items.iter().map(target_of).collect(),
            other => Ok(vec![target_of(&other)?]),
        };
    }
    // Not one document, so it is `jq -c`: one per line.
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value =
                serde_json::from_str(l).context("parsing a line of JSON from stdin")?;
            target_of(&v)
        })
        .collect()
}

/// The target a piped JSON value stands for.
fn target_of(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Object(o) => {
            if let Some(id) = o.get("id").and_then(serde_json::Value::as_i64) {
                Ok(id.to_string())
            } else if let Some(path) = o.get("path").and_then(serde_json::Value::as_str) {
                Ok(path.to_string())
            } else {
                bail!("a JSON object on stdin has neither an `id` nor a `path`")
            }
        }
        other => bail!("{other} on stdin is not a face id, a family or a path"),
    }
}

/// Every type this binary prints with `--json`, one definition each.
///
/// The core's own listing covers the types it defines. The rest live here or in
/// `fontina-platform`, and neither crate can see them from the other, so the two sets are
/// generated separately and merged. CLAUDE.md's rule is that every printed type is in
/// this file; a test in `tests/schema_conformance.rs` checks that against the commands
/// `--help` reports rather than against anyone's memory.
fn cli_output_schema() -> serde_json::Value {
    use schemars::{JsonSchema, SchemaGenerator, generate::SchemaSettings};
    let mut schema = fontina_core::cli_output_schema();
    let mut g = SchemaGenerator::new(SchemaSettings::draft2020_12());
    fn add<T: JsonSchema>(g: &mut SchemaGenerator) {
        g.subschema_for::<T>();
    }
    add::<fontina_platform::SystemFontDir>(&mut g);
    add::<LicenseRow>(&mut g);
    add::<RestoreReport>(&mut g);
    add::<AgentInstalled>(&mut g);
    add::<ConfigReport>(&mut g);
    add::<AgentRemoved>(&mut g);
    add::<AgentStatus>(&mut g);
    add::<Paths>(&mut g);
    let mine = g.take_definitions(true);
    if let Some(defs) = schema.get_mut("$defs").and_then(|d| d.as_object_mut()) {
        for (name, def) in mine {
            defs.insert(name, def);
        }
    }
    schema
}

fn run_tag(cli: &Cli, cmd: &TagCmd) -> Result<()> {
    let mut index = open_index(cli)?;
    match cmd {
        TagCmd::List { json } => {
            let tags = index.tags()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&tags)?);
            } else if tags.is_empty() {
                println!("no tags");
            } else {
                for t in tags {
                    println!("{} {:>6}", pad(&t.name, 30), t.faces);
                }
            }
        }
        TagCmd::Add { tag, targets } => {
            let ids = resolve_all_ids(&index, targets)?;
            let n = index.tag(&ids, tag)?;
            println!("tagged {n} face(s) with {tag:?}");
        }
        TagCmd::Remove { tag, targets } => {
            let ids = resolve_all_ids(&index, targets)?;
            let n = index.untag(&ids, tag)?;
            println!("removed {tag:?} from {n} face(s)");
        }
        TagCmd::Rename { old, new } => {
            if !index.rename_tag(old, new)? {
                bail!("no tag named {old:?}");
            }
            println!("renamed {old:?} to {new:?}");
        }
        TagCmd::Delete { tag } => {
            if !index.delete_tag(tag)? {
                bail!("no tag named {tag:?}");
            }
            println!("deleted {tag:?}");
        }
        TagCmd::Sync {
            to_files,
            from_files,
            targets,
            dry_run,
            json,
        } => {
            if !to_files && !from_files {
                bail!(
                    "say which side is right: --to-files writes the index's tags onto the \
                     files, --from-files reads the files' tags into the index"
                );
            }
            let (report, failures) = tag_sync(&mut index, *to_files, targets, *dry_run)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_tag_sync(&report);
            }
            // The report is printed either way; the status is what a script reads. One
            // font that could not be written is a skip in a successful run, but a run in
            // which everything failed and nothing was done is a failure: a read-only
            // mount should not print `0 of 300 file(s) changed` and exit 0.
            if failures > 0 && report.changes.is_empty() {
                bail!("nothing was synced: {failures} file(s) could not be");
            }
        }
    }
    Ok(())
}

/// A file, the faces in it, and the union of their tags.
type FileTags = (PathBuf, Vec<i64>, BTreeSet<String>);

/// Faces of the index, by the file they live in.
///
/// A tag belongs to a file and fontina's tags belong to faces, and a TrueType collection
/// is several faces in one file. So the file's tags are the union of its faces', and a
/// tag read off a file lands on every face in it.
fn faces_by_file(index: &Index, targets: &[String]) -> Result<Vec<FileTags>> {
    let filter = if targets.is_empty() {
        FaceFilter::default()
    } else {
        FaceFilter {
            ids: Some(resolve_all_ids(index, targets)?),
            ..FaceFilter::default()
        }
    };
    let mut by_file: BTreeMap<PathBuf, Vec<i64>> = BTreeMap::new();
    for f in index.list(&filter)? {
        by_file
            .entry(PathBuf::from(&f.path))
            .or_default()
            .push(f.id);
    }
    // A file's tags are the tags of *every* face in it, not only the selected ones. A
    // font collection holds several faces in one file and the file has one set of tags,
    // so syncing a selection of them to disk with only their own tags would strip the
    // rest. Widening here means the written set is always the whole file's.
    let mut out = Vec::with_capacity(by_file.len());
    for (path, selected) in by_file {
        let ids = match selected.first() {
            Some(id) => index.file_faces(*id)?,
            None => selected,
        };
        let tags: BTreeSet<String> = index
            .summaries(&ids)?
            .into_iter()
            .flat_map(|s| s.tags)
            .collect();
        out.push((path, ids, tags));
    }
    Ok(out)
}

/// Sync tags one way, and say how many files failed.
///
/// A skip is not always a failure. Declining to write a font the operating system ships,
/// or naming a tag the file store cannot hold, is this command working as designed; a
/// file it could not read, write or record is not. Only the second kind is counted, and
/// only the count decides the exit status.
fn tag_sync(
    index: &mut Index,
    to_files: bool,
    targets: &[String],
    dry_run: bool,
) -> Result<(TagSyncReport, usize)> {
    if !fontina_platform::tags::supported() {
        bail!(
            "this system has no file tags: Windows keeps keywords per file format, and a \
             font file has none. `fontina tag` still works — the tags stay in the index."
        );
    }
    let files = faces_by_file(index, targets)?;
    // A font in an OS font directory is not ours to write to, whatever the user asked
    // for. `--to-files` on a library that was scanned with `--system` would otherwise try
    // to set an attribute on every font the operating system ships.
    let readonly: Vec<PathBuf> = if to_files {
        fontina_platform::system_font_dirs()
            .into_iter()
            .filter(|d| !d.user_writable)
            .map(|d| d.path)
            .collect()
    } else {
        Vec::new()
    };
    let mut report = TagSyncReport {
        direction: if to_files { "to-files" } else { "from-files" }.into(),
        files: files.len(),
        changed: 0,
        dry_run,
        changes: Vec::new(),
        skipped: Vec::new(),
    };

    let mut failures = 0usize;
    for (path, ids, indexed) in files {
        if let Some(dir) = readonly.iter().find(|d| path.starts_with(d)) {
            report.skipped.push(TagSyncSkip {
                path: path.to_string_lossy().into_owned(),
                reason: format!("in {}, which fontina does not write to", dir.display()),
            });
            continue;
        }
        let on_file: BTreeSet<String> = match fontina_platform::tags::read(&path) {
            Ok(t) => t.into_iter().collect(),
            Err(e) => {
                failures += 1;
                report.skipped.push(TagSyncSkip {
                    path: path.to_string_lossy().into_owned(),
                    reason: e.to_string(),
                });
                continue;
            }
        };
        let (from, to) = if to_files {
            (&indexed, &on_file)
        } else {
            (&on_file, &indexed)
        };
        // A tag the file store cannot hold is not synced in either direction, and that
        // has to be symmetric. Writing, it cannot go out. Reading, its absence from the
        // file is not evidence anyone removed it: the file was never able to carry it, so
        // treating the difference as a removal would delete from the index the very tag
        // the other direction says it kept.
        let refused: Vec<&String> = from
            .iter()
            .chain(to.iter())
            .filter(|t| fontina_platform::tags::unstorable(t).is_some())
            .collect();
        let writable: Vec<&String> = from
            .iter()
            .filter(|t| fontina_platform::tags::unstorable(t).is_none())
            .collect();
        if !refused.is_empty() {
            report.skipped.push(TagSyncSkip {
                path: path.to_string_lossy().into_owned(),
                reason: format!(
                    "kept in the index only: {}",
                    refused
                        .iter()
                        .map(|t| format!("{t:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        let wanted: BTreeSet<String> = writable.into_iter().cloned().collect();
        // The index holds tag names case-insensitively, so `Work` and `work` are one tag
        // there and two here. Comparing the sets byte-wise made a difference of case look
        // like an addition *and* a removal: the add resolved to the row that already
        // existed and did nothing, the remove then deleted it, and the run after that put
        // it back. Fold both sides before taking the difference.
        let fold = |t: &String| t.to_lowercase();
        let to_folded: BTreeMap<String, &String> = to.iter().map(|t| (fold(t), t)).collect();
        let wanted_folded: BTreeSet<String> = wanted.iter().map(fold).collect();
        // A refused tag is one this sync is deliberately not managing on this side. It
        // must not be counted as removed, or `--to-files` would drop from the file the
        // very tag the skip line says it kept.
        let untouched: BTreeSet<String> = refused.iter().map(|t| fold(t)).collect();
        let added: Vec<String> = wanted
            .iter()
            .filter(|t| !to_folded.contains_key(&fold(t)))
            .cloned()
            .collect();
        let removed: Vec<String> = to_folded
            .iter()
            .filter(|(k, _)| !wanted_folded.contains(*k) && !untouched.contains(*k))
            .map(|(_, v)| (*v).clone())
            .collect();
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        report.changed += 1;
        if !dry_run {
            if to_files {
                // Whatever the file already carried that this sync refused to manage
                // stays on it: `write` replaces the attribute wholesale, so a tag left
                // out of the list is a tag deleted from disk.
                let mut list: Vec<String> = wanted.iter().cloned().collect();
                list.extend(
                    on_file
                        .iter()
                        .filter(|t| untouched.contains(&fold(t)))
                        .cloned(),
                );
                list.sort();
                list.dedup();
                if let Err(e) = fontina_platform::tags::write(&path, &list) {
                    failures += 1;
                    report.changed -= 1;
                    report.skipped.push(TagSyncSkip {
                        path: path.to_string_lossy().into_owned(),
                        reason: e.to_string(),
                    });
                    continue;
                }
            } else {
                // One font that cannot be written should not stop the other three
                // hundred, and that holds for the index as much as for the files: these
                // used to propagate, abandoning a report of everything already done.
                let mut failed = None;
                for t in &added {
                    if let Err(e) = index.tag(&ids, t) {
                        failed = Some(e);
                        break;
                    }
                }
                if failed.is_none() {
                    for t in &removed {
                        if let Err(e) = index.untag(&ids, t) {
                            failed = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = failed {
                    failures += 1;
                    report.changed -= 1;
                    report.skipped.push(TagSyncSkip {
                        path: path.to_string_lossy().into_owned(),
                        reason: e.to_string(),
                    });
                    continue;
                }
            }
        }
        report.changes.push(TagSyncChange {
            path: path.to_string_lossy().into_owned(),
            added,
            removed,
        });
    }
    Ok((report, failures))
}

fn print_tag_sync(report: &TagSyncReport) {
    let side = if report.direction == "to-files" {
        "the files"
    } else {
        "the index"
    };
    for c in &report.changes {
        let mut what = Vec::new();
        if !c.added.is_empty() {
            what.push(format!("+{}", c.added.join(" +")));
        }
        if !c.removed.is_empty() {
            what.push(format!("-{}", c.removed.join(" -")));
        }
        println!("{}  {}", c.path, what.join("  "));
    }
    // Scanning `--system` puts hundreds of files behind one reason, and hundreds of
    // identical lines is not a report. The JSON still names every one.
    let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for s in &report.skipped {
        by_reason
            .entry(s.reason.as_str())
            .or_default()
            .push(s.path.as_str());
    }
    for (reason, paths) in by_reason {
        eprintln!("  not carried: {reason}");
        for p in paths.iter().take(3) {
            eprintln!("      {p}");
        }
        if paths.len() > 3 {
            eprintln!("      and {} more", paths.len() - 3);
        }
    }
    if report.dry_run {
        println!(
            "{} of {} file(s) would change in {}; nothing was written",
            report.changed, report.files, side
        );
    } else {
        println!(
            "{} of {} file(s) changed in {}",
            report.changed, report.files, side
        );
    }
}

fn run_collection(cli: &Cli, cmd: &CollectionCmd) -> Result<()> {
    let mut index = open_index(cli)?;
    match cmd {
        CollectionCmd::List { json } => {
            let cs = index.collections()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&cs)?);
            } else if cs.is_empty() {
                println!("no collections");
            } else {
                for c in cs {
                    println!("{} {:>6}", pad(&c.name, 30), c.faces);
                }
            }
        }
        CollectionCmd::Create { name } => {
            index.create_collection(name)?;
            println!("created {name:?}");
        }
        CollectionCmd::Delete { name } => {
            if !index.delete_collection(name)? {
                bail!("no collection named {name:?}");
            }
            println!("deleted {name:?}");
        }
        CollectionCmd::Rename { old, new } => {
            if !index.rename_collection(old, new)? {
                bail!("no collection named {old:?}");
            }
            println!("renamed {old:?} to {new:?}");
        }
        CollectionCmd::Add { name, targets } => {
            let ids = resolve_all_ids(&index, targets)?;
            let n = index.add_to_collection(name, &ids)?;
            println!("added {n} face(s) to {name:?}");
        }
        CollectionCmd::Remove { name, targets } => {
            let ids = resolve_all_ids(&index, targets)?;
            let n = index.remove_from_collection(name, &ids)?;
            println!("removed {n} face(s) from {name:?}");
        }
        CollectionCmd::Show { name, json } => {
            let faces = index.collection_faces(name)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&faces)?);
            } else {
                print_table(&faces);
            }
        }
        CollectionCmd::Export {
            name,
            output,
            bundle,
            json,
        } => {
            let export = index.export_collection(name)?;
            if let Some(dir) = bundle {
                let report = export.write_bundle(dir)?;
                if *json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    eprintln!(
                        "wrote {} ({} faces, {} files, {} KB)",
                        report.dir,
                        report.faces,
                        report.files,
                        report.bytes / 1024
                    );
                }
                return Ok(());
            }
            let text = serde_json::to_string_pretty(&export)?;
            if output.as_os_str() == "-" {
                println!("{text}");
            } else {
                std::fs::write(output, text.as_bytes())
                    .with_context(|| format!("writing {}", output.display()))?;
                eprintln!("wrote {} ({} faces)", output.display(), export.faces.len());
            }
        }
        CollectionCmd::Import {
            input,
            name,
            no_tags,
            json,
        } => {
            // A bundle is a directory, and naming the directory is what somebody who was
            // handed one will try first.
            let file = if input.is_dir() {
                input.join(fontina_core::BUNDLE_FILE)
            } else {
                input.clone()
            };
            let text = if file.as_os_str() == "-" {
                std::io::read_to_string(std::io::stdin())?
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?
            };
            let mut export: fontina_core::CollectionExport =
                serde_json::from_str(&text).context("parsing collection JSON")?;
            // A bundle's paths mean nothing until they are joined onto the directory the
            // file was read from — which is not knowable at all when it came from stdin.
            if export.relative_paths {
                let base = file
                    .parent()
                    .filter(|_| file.as_os_str() != "-")
                    .map(|p| {
                        // `collection.json` has an empty parent, which resolves to
                        // nothing and would leave every path still relative.
                        if p.as_os_str().is_empty() {
                            PathBuf::from(".")
                        } else {
                            p.to_path_buf()
                        }
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "this collection's paths are relative to the bundle holding it, \
                             so it has to be imported by path rather than through stdin"
                        )
                    })?;
                let escaped = export.resolve_paths(&base);
                if escaped > 0 {
                    eprintln!(
                        "warning: {escaped} path(s) in this bundle point outside it and were \
                         left alone"
                    );
                }
            }
            let report = index.import_collection(&export, name.as_deref(), !*no_tags)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "imported {:?}: {} face(s) matched, {} missing, {} tag(s) applied",
                    report.collection,
                    report.matched,
                    report.missing.len(),
                    report.tags_applied
                );
                for m in &report.missing {
                    eprintln!("  missing: {} {}  {}", m.family, m.subfamily, m.path);
                }
                if input.is_dir() && !report.missing.is_empty() {
                    eprintln!(
                        "a bundle carries the fonts but does not index them: \
                         run `fontina scan {}` first",
                        input.display()
                    );
                }
            }
        }
    }
    Ok(())
}

fn run_source(cli: &Cli, cmd: &SourceCmd) -> Result<()> {
    let mut index = open_index(cli)?;
    match cmd {
        SourceCmd::List { json } => {
            let sources = index.sources()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&sources)?);
            } else if sources.is_empty() {
                println!("no sources; run `fontina scan <dir>` or `fontina source add <dir>`");
            } else {
                for s in sources {
                    println!(
                        "{} {}{}",
                        pad(&s.path, 60),
                        match s.kind {
                            SourceKind::User => "user",
                            SourceKind::System => "system",
                        },
                        if s.watch { ", watched" } else { "" }
                    );
                }
            }
        }
        SourceCmd::Add {
            path,
            no_watch,
            json,
        } => {
            if !path.is_dir() {
                bail!("{} is not a directory", path.display());
            }
            let canonical = std::fs::canonicalize(path)?;
            let report = fontina_core::scan::scan(
                &mut index,
                std::slice::from_ref(&canonical),
                &ScanOptions::default(),
            )?;
            let source =
                index.add_source(&canonical.to_string_lossy(), !*no_watch, SourceKind::User)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&source)?);
            } else {
                println!(
                    "added {}: {} parsed ({} faces), {} unchanged, {} failed{}",
                    source.path,
                    report.parsed,
                    report.faces,
                    report.unchanged,
                    report.failed.len(),
                    if source.watch { ", watched" } else { "" }
                );
            }
        }
        SourceCmd::Remove { path, purge } => {
            let key = std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            if !index.remove_source(&key, *purge)? {
                bail!("{key} is not a source");
            }
            println!(
                "removed {key}{}",
                if *purge { " and its faces" } else { "" }
            );
        }
        SourceCmd::Watch { path, off } => {
            let key = std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            if !index.set_source_watch(&key, !*off)? {
                bail!("{key} is not a source");
            }
            println!("{key}: watch {}", if *off { "off" } else { "on" });
        }
    }
    Ok(())
}

fn print_families(families: &[fontina_core::Family]) {
    if families.is_empty() {
        println!("no families match");
        return;
    }
    let w = families
        .iter()
        .map(|f| fontina_core::unicode::columns(&f.name))
        .max()
        .unwrap_or(6)
        .clamp(6, 40);
    println!(
        "{:<w$}  {:>5}  {:<9}  {:<9}  {:<5}  {:<12}  scripts",
        "family", "faces", "weights", "widths", "flags", "license"
    );
    for f in families {
        let flags = format!(
            "{}{}{}{}",
            if f.variable { "V" } else { "-" },
            if f.color { "C" } else { "-" },
            if f.italic { "I" } else { "-" },
            if f.active > 0 { "A" } else { "-" }
        );
        let range = |lo: f32, hi: f32| {
            if (lo - hi).abs() < 0.5 {
                format!("{}", lo.round() as i64)
            } else {
                format!("{}-{}", lo.round() as i64, hi.round() as i64)
            }
        };
        println!(
            "{}  {:>5}  {:<9}  {:<9}  {:<5}  {}  {}",
            cell(&f.name, w),
            f.faces,
            range(f.weights[0], f.weights[1]),
            range(f.widths[0], f.widths[1]),
            flags,
            cell(f.license.as_deref().unwrap_or("-"), 12),
            f.scripts
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    println!("{} family(ies)", families.len());
}

fn print_facets(f: &fontina_core::Facets) {
    println!("{} face(s) in {} family(ies)", f.faces, f.families);
    let row =
        |label: &str, items: &[fontina_core::index::FacetCount], name: &dyn Fn(&str) -> String| {
            if items.is_empty() {
                return;
            }
            let parts: Vec<String> = items
                .iter()
                .take(12)
                .map(|c| format!("{} {}", name(&c.value), c.count))
                .collect();
            let more = if items.len() > 12 {
                format!(" · +{} more", items.len() - 12)
            } else {
                String::new()
            };
            println!("{label:<11} {}{more}", parts.join(" · "));
        };
    row("weight", &f.weight, &|v| {
        format!(
            "{v} {}",
            fontina_core::index::weight_name(v.parse().unwrap_or(400))
        )
    });
    row("width", &f.width, &|v| {
        format!(
            "{v}% {}",
            fontina_core::index::width_name(v.parse().unwrap_or(100.0))
        )
    });
    row("style", &f.style, &|v| v.to_string());
    println!("{:<11} {}   color {}", "variable", f.variable, f.color);
    row("container", &f.container, &|v| v.to_string());
    row("spacing", &f.spacing, &|v| v.to_string());
    row("script", &f.script, &|v| v.to_string());
    row("language", &f.language, &|v| v.to_string());
    row("license", &f.license, &|v| v.to_string());
    row("freedom", &f.freedom, &|v| v.to_string());
    row("vendor", &f.vendor, &|v| v.to_string());
    row("tag", &f.tag, &|v| v.to_string());
    row("collection", &f.collection, &|v| v.to_string());
    row("activation", &f.activation, &|v| v.to_string());
    row("source", &f.source, &|v| v.to_string());
}

/// The operating system's font directories, in the form the index stores paths in.
///
/// Canonical, because a conflict is found by matching a stored path against these as a
/// prefix, and the index canonicalises every path it stores. Where any component of a
/// font directory is a symlink the two spellings differ and the prefix never matches:
/// `/var/…` against a stored `/private/var/…` on macOS, or a home directory that is
/// itself a link. The answer then is a confident "no conflicts" about a font that is
/// sitting right there.
///
/// A directory that does not exist keeps the name the platform gave it: there is nothing
/// to canonicalise, and nothing indexed under it either.
fn system_roots() -> Vec<String> {
    fontina_platform::system_font_dirs()
        .into_iter()
        .map(|d| {
            std::fs::canonicalize(&d.path)
                .unwrap_or(d.path)
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// Whether the index holds anything from the operating system's own font directories.
///
/// `conflicts` can only report a clash with a face it knows about, and the faces that
/// matter most are the ones some other program installed: Font Book's copies in
/// `~/Library/Fonts`, a distribution's packages in `/usr/share/fonts`. If none of those
/// directories has ever been scanned, the answer to "does this clash with anything?" is
/// "nothing I can see", and printing that as "no conflicts" is a confident wrong answer
/// to the one question where being wrong costs something.
fn system_dirs_are_indexed(index: &Index) -> bool {
    system_roots().iter().any(|root| {
        index
            .list(&FaceFilter {
                path_prefix: Some(root.clone()),
                limit: Some(1),
                ..FaceFilter::default()
            })
            .is_ok_and(|faces| !faces.is_empty())
    })
}

/// Say what this index cannot see, on stderr.
///
/// `conflicts` says it whenever it finds nothing, because that is the question being
/// asked and the answer is only as good as what was indexed. `activate` says it only
/// before the first activation: there it is context for someone starting out, not an
/// answer, and repeating it on every activation is how a reader learns to skip notes.
fn note_the_blind_spot(index: &Index) {
    if !system_dirs_are_indexed(index) {
        eprintln!(
            "note: no operating-system font directory is in this index, so this cannot see \
             fonts installed outside fontina; `fontina scan --system` puts them in"
        );
    }
}

pub(crate) fn collect_conflicts(index: &Index, ids: &[i64]) -> Result<Vec<fontina_core::Conflict>> {
    let roots = system_roots();
    let mut out: Vec<fontina_core::Conflict> = Vec::new();
    for id in ids {
        for c in index.conflicts(*id, &roots)? {
            if !ids.contains(&c.face.id) && !out.iter().any(|o| o.face.id == c.face.id) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

fn print_conflicts(conflicts: &[fontina_core::Conflict]) {
    for c in conflicts {
        eprintln!(
            "conflict: [{}] {} {} ({})  {}",
            c.face.id, c.face.family, c.face.subfamily, c.reason, c.face.path
        );
    }
    eprintln!(
        "{} conflict(s); pass --replace to deactivate the ones fontina manages",
        conflicts.len()
    );
}

/// The distinct files behind a set of face ids, each with every face id in that file.
pub(crate) fn files_for(index: &Index, ids: &[i64]) -> Result<Vec<(PathBuf, Vec<i64>)>> {
    let mut out: Vec<(PathBuf, Vec<i64>)> = Vec::new();
    for s in index.summaries(ids)? {
        let path = PathBuf::from(&s.path);
        if out.iter().any(|(p, _)| *p == path) {
            continue;
        }
        let faces = index.file_faces(s.id)?;
        out.push((path, faces));
    }
    Ok(out)
}

/// Take back the registration this face already has, before it is given another.
///
/// Every state registers the font somewhere: session and user register the file where it
/// lies, installed puts a copy in the per-user font directory. The index records only
/// where a face has arrived, so a move between two states that skipped the leaving left
/// the earlier registration behind with nothing pointing at it.
///
/// Both directions were wrong, and both in a way a person would notice much later.
/// `activate` then `install` left the font registered in place: it stayed visible to
/// every application, `uninstall` removed only the copy, and no command could ever take
/// the registration back, because the record that would have named it had been
/// overwritten. `install` then `activate` left the copy in the font directory and
/// overwrote the path that named it, so `uninstall` then refused on the grounds that
/// nothing had been installed.
///
/// Re-entering the state a face is already in is not a transition and leaves it alone:
/// `activate` twice over is the same as `activate` once.
fn leave_current_state(
    index: &Index,
    activator: &dyn fontina_platform::FontActivator,
    id: i64,
    path: &std::path::Path,
    next: ActivationState,
) -> Result<()> {
    let Some(record) = index.activation(id)? else {
        return Ok(());
    };
    if record.state == next {
        return Ok(());
    }
    let installed = record.installed_path.as_deref().map(std::path::Path::new);
    fontina_platform::withdraw(activator, path, installed).with_context(|| {
        match record.installed_path.as_deref() {
            Some(p) => format!("uninstalling {p}, which {} replaces", verb(next)),
            None => format!(
                "deactivating {}, which {} replaces",
                path.display(),
                verb(next)
            ),
        }
    })?;
    Ok(())
}

/// What a state does, for an error message: "deactivating X, which installing replaces".
fn verb(state: ActivationState) -> &'static str {
    match state {
        ActivationState::Installed => "installing",
        _ => "activating",
    }
}

fn run_activate(
    cli: &Cli,
    targets: &[String],
    state: ActivationState,
    replace: bool,
    json: bool,
) -> Result<()> {
    let mut index = open_index(cli)?;
    let ids = resolve_all_ids(&index, targets)?;
    let activator = fontina_platform::activator();
    let conflicts = collect_conflicts(&index, &ids)?;
    if conflicts.is_empty() && index.activations()?.is_empty() {
        // Nothing to report, and possibly nothing to report *with*: say which — but
        // only the first time, when nothing has been activated through fontina yet.
        // The reader who has done this before has read it, and a note that prints on
        // every activation for the life of an index is noise, which is how a person
        // learns to stop reading notes.
        note_the_blind_spot(&index);
    }
    if !conflicts.is_empty() {
        if !replace {
            print_conflicts(&conflicts);
            std::process::exit(2);
        }
        for c in &conflicts {
            match c.face.activation {
                Some(ActivationState::Installed) => {
                    let rec = index.activation(c.face.id)?;
                    if let Some(p) = rec.and_then(|r| r.installed_path) {
                        activator.uninstall(std::path::Path::new(&p))?;
                    }
                    let faces = index.file_faces(c.face.id)?;
                    index.clear_activation(&faces)?;
                    eprintln!("uninstalled {} {}", c.face.family, c.face.subfamily);
                }
                Some(_) => {
                    let removed = activator.deactivate(std::path::Path::new(&c.face.path))?;
                    let faces = index.file_faces(c.face.id)?;
                    index.clear_activation(&faces)?;
                    let note = if removed {
                        ""
                    } else {
                        " (nothing was registered; cleared the record)"
                    };
                    eprintln!("deactivated {} {}{note}", c.face.family, c.face.subfamily);
                }
                None => eprintln!(
                    "warning: {} {} is a system font at {}; it cannot be replaced, the OS decides which wins",
                    c.face.family, c.face.subfamily, c.face.path
                ),
            }
        }
    }
    let mut done = Vec::new();
    for (path, faces) in files_for(&index, &ids)? {
        leave_current_state(&index, activator.as_ref(), faces[0], &path, state)?;
        match state {
            ActivationState::Installed => {
                let installed = activator
                    .install(&path)
                    .with_context(|| format!("installing {}", path.display()))?;
                index.set_activation(&faces, state, Some(&installed.to_string_lossy()))?;
            }
            ActivationState::Session | ActivationState::User => {
                let scope = if state == ActivationState::Session {
                    fontina_platform::Scope::Session
                } else {
                    fontina_platform::Scope::User
                };
                activator
                    .activate(&path, scope)
                    .with_context(|| format!("activating {}", path.display()))?;
                index.set_activation(&faces, state, None)?;
            }
        }
        done.extend(faces);
    }
    let records: Vec<_> = index
        .activations()?
        .into_iter()
        .filter(|r| done.contains(&r.face.id))
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else {
        for r in &records {
            println!(
                "{} {} {}{}",
                match state {
                    ActivationState::Installed => "installed",
                    _ => "activated",
                },
                r.face.family,
                r.face.subfamily,
                match (&r.installed_path, state) {
                    (Some(p), _) => format!(" -> {p}"),
                    (None, ActivationState::Session) => " (until logout)".into(),
                    _ => String::new(),
                }
            );
        }
    }
    Ok(())
}

fn run_deactivate(cli: &Cli, targets: &[String], uninstall: bool, json: bool) -> Result<()> {
    let mut index = open_index(cli)?;
    let ids = resolve_all_ids(&index, targets)?;
    let activator = fontina_platform::activator();
    let mut done = Vec::new();
    for (path, faces) in files_for(&index, &ids)? {
        let record = index.activation(faces[0])?;
        let installed = record.as_ref().and_then(|r| r.installed_path.clone());
        if uninstall {
            let Some(installed) = installed else {
                bail!("{} was not installed by fontina", path.display());
            };
            activator
                .uninstall(std::path::Path::new(&installed))
                .with_context(|| format!("uninstalling {installed}"))?;
        } else if let Some(installed) = installed {
            // What is registered is the copy, not this file, so deactivating the file
            // would take nothing back and clearing the record would leave the copy in
            // the font directory with nothing naming it. Say which command removes it.
            bail!(
                "{} is installed at {installed}; `fontina uninstall` removes it, \
                 `fontina deactivate` does not",
                path.display()
            );
        } else if !activator
            .deactivate(&path)
            .with_context(|| format!("deactivating {}", path.display()))?
        {
            // Nothing was registered under that path: the record is stale, so clearing it
            // is all there is to do, and saying so beats reporting a removal that was not.
            eprintln!("{}: nothing was active; cleared the record", path.display());
        }
        index.clear_activation(&faces)?;
        done.push(path);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&Paths(done.clone()))?);
    } else {
        for p in &done {
            println!(
                "{} {}",
                if uninstall {
                    "uninstalled"
                } else {
                    "deactivated"
                },
                p.display()
            );
        }
    }
    Ok(())
}

/// What `config --json` prints: the file, whether it is there, and every setting with
/// where its value came from.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ConfigReport<'a> {
    path: PathBuf,
    /// False when there is no file yet, which is not an error.
    found: bool,
    settings: &'a [config::Setting],
}

/// What `agent install --json` prints.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct AgentInstalled {
    installed: bool,
    path: PathBuf,
    /// The mechanism, for a human: `systemd user unit`, `LaunchAgent`, `Startup folder`.
    kind: &'static str,
    /// The command that starts it now rather than at the next login, if one is needed.
    activate_with: Option<String>,
}

/// What `agent uninstall --json` prints.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct AgentRemoved {
    removed: bool,
    /// The command that undoes the enablement, which deleting the file does not.
    deactivate_with: Option<String>,
}

/// What `agent status --json` prints.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct AgentStatus {
    installed: bool,
    enabled: bool,
    path: Option<PathBuf>,
    kind: Option<&'static str>,
}

/// A list of paths, printed as a bare array so it pipes straight back into `--stdin`.
///
/// The newtype exists to give the array a name in `schemas/cli-output.json`: a type
/// printed with `--json` has to be described there, and `Vec<PathBuf>` has no name to
/// describe. `transparent` keeps the JSON exactly what it was.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(transparent)]
struct Paths(Vec<PathBuf>);

#[derive(Debug, Default, serde::Serialize, schemars::JsonSchema)]
struct RestoreReport {
    restored: usize,
    reinstalled: usize,
    failed: Vec<(String, String)>,
}

fn run_restore(cli: &Cli, json: bool) -> Result<()> {
    let mut index = open_index(cli)?;
    let activator = fontina_platform::activator();
    let report = restore_activations(&mut index, activator.as_ref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "restored {} activation(s), {} reinstalled, {} failed",
            report.restored,
            report.reinstalled,
            report.failed.len()
        );
        for (p, e) in &report.failed {
            eprintln!("  ! {p}: {e}");
        }
    }
    Ok(())
}

/// Reapply every recorded activation with `activator`. A face is either restored or
/// failed, never both, and a reinstall counts only once the index has been told where
/// the new copy went.
fn restore_activations(
    index: &mut Index,
    activator: &dyn fontina_platform::FontActivator,
) -> Result<RestoreReport> {
    let mut report = RestoreReport::default();
    for r in index.activations()? {
        let path = std::path::Path::new(&r.face.path);
        let mut reinstalled = false;
        let result = match r.state {
            ActivationState::Session => activator.activate(path, fontina_platform::Scope::Session),
            ActivationState::User => activator.activate(path, fontina_platform::Scope::User),
            ActivationState::Installed => {
                match r
                    .installed_path
                    .as_deref()
                    .filter(|p| std::path::Path::new(p).exists())
                {
                    Some(_) => Ok(()),
                    None => activator.install(path).and_then(|p| {
                        // A database error here is a failure to record the install, not
                        // an empty face list to write nothing for.
                        let os = |e: fontina_core::Error| {
                            fontina_platform::PlatformError::Os(e.to_string())
                        };
                        let faces = index.file_faces(r.face.id).map_err(os)?;
                        index
                            .set_activation(&faces, r.state, Some(&p.to_string_lossy()))
                            .map_err(os)?;
                        reinstalled = true;
                        Ok(())
                    }),
                }
            }
        };
        match result {
            Ok(()) => {
                report.restored += 1;
                report.reinstalled += usize::from(reinstalled);
            }
            Err(e) => report.failed.push((r.face.path.clone(), e.to_string())),
        }
    }
    Ok(report)
}

/// Which inline-image protocol the terminal speaks, from the environment.
fn detect_protocol() -> &'static str {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let term = env("TERM");
    let program = env("TERM_PROGRAM");
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return "blocks";
    }
    if term.starts_with("xterm-kitty")
        || !env("KITTY_WINDOW_ID").is_empty()
        || program == "ghostty"
        || term.contains("ghostty")
        || env("KONSOLE_VERSION").parse::<u32>().unwrap_or(0) >= 220400
        || program == "WezTerm"
    {
        return "kitty";
    }
    if program == "iTerm.app" || program == "mintty" || !env("ITERM_SESSION_ID").is_empty() {
        return "iterm";
    }
    if term.starts_with("foot")
        || term == "mlterm"
        || term.contains("sixel")
        || !env("WT_SESSION").is_empty()
    {
        return "sixel";
    }
    "blocks"
}

/// Whether the terminal background is dark, from `COLORFGBG` ("15;0" = light on dark).
fn dark_background() -> bool {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|v| v.rsplit(';').next()?.parse::<u8>().ok())
        .map(|bg| bg <= 6 || bg == 8)
        .unwrap_or(true)
}

/// Say how many of the fonts about to be embedded are under a licence that withholds
/// redistribution.
///
/// A specimen with `--link` references the files; without it, it carries them, and the
/// file that comes out is a font you can send to somebody. That is exactly what a
/// specimen is for and exactly why it is worth a sentence: a designer's library is
/// mostly licensed fonts, and the difference between the two forms is not visible in
/// the output.
///
/// Said, not enforced, and not a warning. `freedom.rs` reports what a licence says and
/// leaves the decision where it belongs; this is the same rule one layer up.
fn embedded_nonfree(faces: &[fontina_core::FaceMetadata]) -> usize {
    faces
        .iter()
        .filter(|f| fontina_core::freedom::classify(f.license.spdx.as_deref()) != Freedom::Free)
        .count()
}

fn note_what_is_embedded(faces: &[fontina_core::FaceMetadata]) {
    let nonfree = embedded_nonfree(faces);
    if nonfree == 0 {
        return;
    }
    eprintln!(
        "note: {nonfree} of {} face(s) are under a licence that does not grant \
         redistribution, and this specimen embeds the font files; `--link` references \
         them instead",
        faces.len()
    );
}

fn run_preview(cli: &Cli, args: &PreviewArgs) -> Result<()> {
    use fontina_core::render::{RenderOptions, encode, render_face};
    let cfg = config::load()?.config.preview;
    let mut faces = Vec::new();
    for t in &args.targets {
        faces.extend(resolve_faces(cli, t)?);
    }
    // Flag, then the configuration file, then what fontina has always done.
    let asked = args.protocol.clone().or(cfg.protocol);
    let protocol = if args.output.is_some() {
        "png"
    } else {
        match asked.as_deref() {
            None | Some("auto") => detect_protocol(),
            Some(p) => p,
        }
    };
    if protocol == "png" && args.output.is_none() {
        bail!("--protocol png needs --output <file.png>");
    }
    if args.output.is_some() && faces.len() != 1 {
        bail!("--output writes one face; got {}", faces.len());
    }
    let dark = dark_background();
    let asked_fg = args.fg.clone().or(cfg.fg.clone());
    let asked_bg = args.bg.clone().or(cfg.bg.clone());
    let fg = match &asked_fg {
        Some(s) => encode::parse_rgb(s).with_context(|| format!("bad colour {s:?}"))?,
        None if dark => [235, 235, 235],
        None => [20, 20, 20],
    };
    let bg = match &asked_bg {
        Some(s) => encode::parse_rgb(s).with_context(|| format!("bad colour {s:?}"))?,
        None if dark => [0, 0, 0],
        None => [255, 255, 255],
    };
    let tmux = std::env::var_os("TMUX").is_some();
    let mut out = std::io::stdout().lock();
    for face in &faces {
        let text = args
            .text
            .clone()
            .or_else(|| cfg.text.clone())
            .or_else(|| face.names.sample_text.clone())
            .unwrap_or_else(|| fontina_core::typography::DEFAULT_TEXT.into())
            .replace("\\n", "\n");
        // Half blocks are two pixels to a cell, so the same number draws twice as tall
        // as it does through an image protocol; the default drops for them alone, and
        // only when nobody asked for a size.
        let asked_size = args.size.or(cfg.size);
        let size = match asked_size {
            Some(s) => s,
            None if protocol == "blocks" => 24.0,
            None => 48.0,
        };
        let bitmap = render_face(
            face,
            &RenderOptions {
                text,
                size,
                variations: args.axes.clone(),
                features: args.features.clone(),
                padding: 2,
                max_width: args.max_width.or_else(|| {
                    (protocol == "blocks").then(|| terminal_columns().saturating_sub(1))
                }),
            },
        )
        .with_context(|| format!("rendering {}", face.file.path))?;
        if protocol == "png" {
            let path = args.output.as_ref().expect("checked");
            std::fs::write(
                path,
                encode::png(&bitmap, fg, asked_bg.as_ref().map(|_| bg)),
            )
            .with_context(|| format!("writing {}", path.display()))?;
            eprintln!(
                "wrote {} ({}x{}, {} glyphs)",
                path.display(),
                bitmap.width,
                bitmap.height,
                bitmap.glyphs
            );
            continue;
        }
        writeln!(
            out,
            "{} {}  ({} {}px{})",
            face.names.family,
            face.names.subfamily,
            face.file.container.as_str(),
            size as u32,
            if bitmap.missing > 0 {
                // Said here rather than left to the reader: a row of empty boxes is what
                // a font prints for text it does not cover, and it looks like a
                // rendering fault rather than an answer.
                format!(
                    ", {} of {} glyph(s) not in this font",
                    bitmap.missing, bitmap.glyphs
                )
            } else {
                String::new()
            }
        )?;
        let rendered = match protocol {
            "kitty" => encode::kitty(&encode::png(&bitmap, fg, None), tmux),
            "iterm" => encode::iterm(&encode::png(&bitmap, fg, None), tmux),
            "sixel" => {
                let mut s = encode::sixel(&bitmap, fg, bg, 16);
                s.push('\n');
                s
            }
            "blocks" => encode::half_blocks(&bitmap, fg, bg),
            other => {
                bail!("unknown protocol {other:?}; use auto, kitty, iterm, sixel, blocks or png")
            }
        };
        out.write_all(rendered.as_bytes())?;
    }
    Ok(())
}

fn terminal_columns() -> u32 {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .or_else(|| {
            // SAFETY: TIOCGWINSZ fills a plain struct; a failure leaves it untouched.
            #[cfg(unix)]
            unsafe {
                let mut ws: [u16; 4] = [0; 4];
                if libc_ioctl_winsize(ws.as_mut_ptr()) == 0 && ws[1] > 0 {
                    return Some(ws[1] as u32);
                }
            }
            None
        })
        .unwrap_or(80)
}

#[cfg(unix)]
unsafe fn libc_ioctl_winsize(ws: *mut u16) -> i32 {
    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
    const TIOCGWINSZ: u64 = 0x4008_7468;
    #[cfg(not(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd")))]
    const TIOCGWINSZ: u64 = 0x5413;
    unsafe { ioctl(1, TIOCGWINSZ, ws) }
}

/// A stable key for a parsed face, for caches: the file's hash and the face index.
pub(crate) fn face_key(face: &fontina_core::FaceMetadata) -> i64 {
    let h = &face.file.blake3;
    let n = i64::from_str_radix(&h[..15.min(h.len())], 16).unwrap_or(0);
    n.wrapping_mul(31).wrapping_add(face.index as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontina_platform::{FontActivator, Scope};
    use std::path::Path;

    /// An activator that installs without touching the machine, and can break the index
    /// while it does so — the one way, from outside the core, to make the write that
    /// records an install fail.
    struct Fake {
        db: PathBuf,
        /// Table to drop from a second connection while `install` runs.
        breaks: Option<&'static str>,
    }

    impl FontActivator for Fake {
        fn install(&self, file: &Path) -> fontina_platform::Result<PathBuf> {
            if let Some(table) = self.breaks {
                rusqlite::Connection::open(&self.db)
                    .and_then(|c| c.execute_batch(&format!("DROP TABLE {table}")))
                    .expect("second connection");
            }
            Ok(file.with_extension("installed"))
        }
        fn uninstall(&self, _installed: &Path) -> fontina_platform::Result<()> {
            Ok(())
        }
        fn activate(&self, _file: &Path, _scope: Scope) -> fontina_platform::Result<()> {
            Ok(())
        }
        fn deactivate(&self, _file: &Path) -> fontina_platform::Result<bool> {
            Ok(true)
        }
    }

    /// An index over one fixture with its faces recorded as installed, but with no
    /// installed path, so `restore` has to install them again.
    fn installed_index(name: &str) -> (PathBuf, Index) {
        let dir =
            std::env::temp_dir().join(format!("fontina-restore-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.db");
        let mut index = Index::open(&db).unwrap();
        let font = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/Amiri-Regular.ttf");
        fontina_core::scan::scan(&mut index, &[font], &ScanOptions::default()).unwrap();
        let ids: Vec<i64> = index
            .list(&FaceFilter::default())
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(ids.len(), 1);
        index
            .set_activation(&ids, ActivationState::Installed, None)
            .unwrap();
        (db, index)
    }

    /// An activator that records what it was asked to do and does nothing else.
    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<String>>);

    impl Recorder {
        fn calls(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
        fn log(&self, call: String) {
            self.0.lock().unwrap().push(call);
        }
    }

    impl FontActivator for Recorder {
        fn install(&self, file: &Path) -> fontina_platform::Result<PathBuf> {
            self.log("install".into());
            Ok(file.with_extension("installed"))
        }
        fn uninstall(&self, installed: &Path) -> fontina_platform::Result<()> {
            self.log(format!("uninstall {}", installed.display()));
            Ok(())
        }
        fn activate(&self, _file: &Path, _scope: Scope) -> fontina_platform::Result<()> {
            self.log("activate".into());
            Ok(())
        }
        fn deactivate(&self, file: &Path) -> fontina_platform::Result<bool> {
            self.log(format!("deactivate {}", file.display()));
            Ok(true)
        }
    }

    /// An index over one fixture, with no activation recorded.
    fn scanned_index(name: &str) -> (PathBuf, Index, i64, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fontina-leave-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.db");
        let mut index = Index::open(&db).unwrap();
        let font = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/Amiri-Regular.ttf");
        fontina_core::scan::scan(
            &mut index,
            std::slice::from_ref(&font),
            &ScanOptions::default(),
        )
        .unwrap();
        let id = index.list(&FaceFilter::default()).unwrap()[0].id;
        (db, index, id, font)
    }

    /// Moving between two activation states takes the first one back.
    ///
    /// The defect this holds shut ran both ways. `activate` then `install` left the font
    /// registered in place and overwrote the only record that named that registration, so
    /// the font stayed visible to every application and nothing could take it back.
    /// `install` then `activate` left the copy in the per-user font directory and
    /// overwrote the path that named it, so `uninstall` refused on the grounds that
    /// nothing had been installed.
    #[test]
    fn a_transition_takes_back_the_registration_it_replaces() {
        let (_, mut index, id, font) = scanned_index("both-ways");
        let faces = index.file_faces(id).unwrap();

        // Activated in place, then installed: the in-place registration is taken back.
        index
            .set_activation(&faces, ActivationState::User, None)
            .unwrap();
        let rec = Recorder::default();
        leave_current_state(&index, &rec, id, &font, ActivationState::Installed).unwrap();
        assert_eq!(
            rec.calls(),
            vec![format!("deactivate {}", font.display())],
            "installing over an activation has to deactivate the file first"
        );

        // Installed, then activated in place: the copy is taken back.
        index
            .set_activation(&faces, ActivationState::Installed, Some("/fonts/copy.ttf"))
            .unwrap();
        let rec = Recorder::default();
        leave_current_state(&index, &rec, id, &font, ActivationState::User).unwrap();
        assert_eq!(
            rec.calls(),
            vec!["uninstall /fonts/copy.ttf".to_string()],
            "activating over an install has to remove the copy first"
        );
    }

    /// Two states that both register the file in place still swap properly, and
    /// re-entering the state a face is already in leaves it alone.
    #[test]
    fn session_and_user_swap_but_a_repeat_is_not_a_transition() {
        let (_, mut index, id, font) = scanned_index("same-state");
        let faces = index.file_faces(id).unwrap();

        index
            .set_activation(&faces, ActivationState::Session, None)
            .unwrap();
        let rec = Recorder::default();
        leave_current_state(&index, &rec, id, &font, ActivationState::User).unwrap();
        assert_eq!(
            rec.calls(),
            vec![format!("deactivate {}", font.display())],
            "a session activation is a registration too, and user scope replaces it"
        );

        let rec = Recorder::default();
        leave_current_state(&index, &rec, id, &font, ActivationState::Session).unwrap();
        assert!(
            rec.calls().is_empty(),
            "activating a font that is already activated that way is not a transition: {:?}",
            rec.calls()
        );

        // And a face with no record at all has nothing to take back.
        index.clear_activation(&faces).unwrap();
        let rec = Recorder::default();
        leave_current_state(&index, &rec, id, &font, ActivationState::Installed).unwrap();
        assert!(rec.calls().is_empty(), "{:?}", rec.calls());
    }

    /// A specimen that embeds says how many of the fonts it is carrying are licensed.
    ///
    /// Every fixture is free, so the nonfree side is a parsed fixture with its licence
    /// changed — the same way `tests/checks.rs` triggers `license/nonfree`, and for the
    /// same reason: we may not redistribute a font that says we may not.
    #[test]
    fn a_specimen_counts_the_licensed_fonts_it_would_carry() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/Amiri-Regular.ttf");
        let (_, faces) = fontina_core::load_file(&path).unwrap();
        assert_eq!(
            embedded_nonfree(&faces),
            0,
            "the fixtures are free and get no note"
        );

        let mut licensed = faces.clone();
        licensed[0].license.spdx = Some("LicenseRef-Proprietary".into());
        assert_eq!(embedded_nonfree(&licensed), 1);

        // A licence nobody has ruled on is not free either: the reader is the one who
        // knows, and the count is what tells them there is something to know.
        let mut unknown = faces.clone();
        unknown[0].license.spdx = None;
        assert_eq!(embedded_nonfree(&unknown), 1);
    }

    #[test]
    fn restore_counts_a_reinstall_once_it_is_recorded() {
        let (db, mut index) = installed_index("ok");
        let report = restore_activations(&mut index, &Fake { db, breaks: None }).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(report.reinstalled, 1);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(index.activations().unwrap()[0].installed_path.is_some());
    }

    #[test]
    fn restore_does_not_count_a_reinstall_it_could_not_record() {
        // The install succeeds, then writing where the copy went fails.
        let (db, mut index) = installed_index("write");
        let report = restore_activations(
            &mut index,
            &Fake {
                db,
                breaks: Some("activations"),
            },
        )
        .unwrap();
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.restored, 0, "{report:?}");
        assert_eq!(
            report.reinstalled, 0,
            "a face counted as reinstalled and failed: {report:?}"
        );
    }

    #[test]
    fn restore_surfaces_a_database_error_when_looking_up_the_faces() {
        // Reading the file's faces fails: that is a failure, not an empty face list to
        // silently write nothing for.
        let (db, mut index) = installed_index("read");
        let report = restore_activations(
            &mut index,
            &Fake {
                db,
                breaks: Some("faces"),
            },
        )
        .unwrap();
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.restored, 0, "{report:?}");
        assert_eq!(report.reinstalled, 0, "{report:?}");
    }
}
