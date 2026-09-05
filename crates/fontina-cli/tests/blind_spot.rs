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

//! "No conflicts" has to mean "I looked", not "I had nothing to look at".
//!
//! `conflicts` reports a clash with a face in the index, and the clashes that matter
//! most are with fonts some other program installed: Font Book's copies in
//! `~/Library/Fonts`, a distribution's packages in `/usr/share/fonts`. Those are in the
//! index only if somebody ran `fontina scan --system`, and until then the answer to
//! "does this clash with anything?" is "nothing I can see" — which was printed as "no
//! conflicts" and exited 0.
//!
//! Found against a real library: a font sitting in `~/Library/Fonts`, active on the
//! machine, and `fontina conflicts` said there was nothing in its way.

use std::path::PathBuf;
use std::process::Command;

struct Session {
    root: PathBuf,
    db: PathBuf,
    fonts: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn session(name: &str) -> Session {
    let root = std::env::temp_dir().join(format!("fontina-blind-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fonts = root.join("fonts");
    std::fs::create_dir_all(&fonts).unwrap();
    std::fs::copy(
        fixtures().join("Amiri-Regular.ttf"),
        fonts.join("Amiri-Regular.ttf"),
    )
    .unwrap();
    let s = Session {
        db: root.join("index.db"),
        fonts,
        root,
    };
    let out = s.run(&["scan", &s.fonts.to_string_lossy()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    s
}

impl Session {
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_fontina"))
            .args(["--db", &self.db.to_string_lossy()])
            .args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join(".config"))
            .env("XDG_DATA_HOME", self.root.join(".local/share"))
            .env("LOCALAPPDATA", self.root.join("AppData/Local"))
            .output()
            .expect("fontina runs")
    }

    /// The per-user font directory fontina itself names, whichever it is on this system.
    fn user_font_dir(&self) -> PathBuf {
        let out = self.run(&["dirs", "--json"]);
        let dirs: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        dirs.as_array()
            .expect("directories")
            .iter()
            .find(|d| d["user_writable"].as_bool().unwrap_or(false))
            .map(|d| PathBuf::from(d["path"].as_str().expect("a path")))
            .expect("this system has a per-user font directory")
    }
}

/// With no system directory indexed, "no conflicts" says what it could not see.
#[test]
fn no_conflicts_says_so_when_there_was_nothing_to_compare_against() {
    let s = session("unscanned");
    let out = s.run(&["conflicts", "1"]);
    assert!(out.status.success(), "nothing found is not an error");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("no conflicts"), "{stdout}");
    assert!(
        stderr.contains("scan --system"),
        "it names the command that would let it look: {stderr:?}"
    );
}

/// `activate` says it before the first activation and not after.
///
/// The note is context for someone starting out. Printed on every activation for the
/// life of an index it is noise, and a reader who meets the same note every time learns
/// to skip notes — including the one that matters.
#[cfg(unix)]
#[test]
fn activate_says_it_before_the_first_one_only() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        eprintln!("skipped: activation reaches the running login session on this system");
        return;
    }
    let s = session("first-activation");
    let listed: serde_json::Value =
        serde_json::from_slice(&s.run(&["list", "--json"]).stdout).expect("JSON");
    let id = listed[0]["id"].to_string();

    let first = s.run(&["activate", "--session", &id]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("scan --system"),
        "the first activation carries the note"
    );

    let second = s.run(&["activate", "--session", &id]);
    assert!(
        !String::from_utf8_lossy(&second.stderr).contains("scan --system"),
        "and the next one does not: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let _ = s.run(&["deactivate", &id]);
}

/// Once a system directory is indexed, the note is gone and the clash is found.
///
/// The font is put in the per-user font directory by hand, which is what another program
/// installing a font looks like from here.
#[test]
fn a_font_another_program_installed_is_a_conflict_once_it_is_indexed() {
    let s = session("scanned");
    let dir = s.user_font_dir();
    assert!(
        dir.starts_with(&s.root),
        "the sandbox did not redirect the font directory ({})",
        dir.display()
    );
    std::fs::create_dir_all(&dir).unwrap();
    let planted = dir.join("Amiri-Regular.ttf");
    std::fs::copy(fixtures().join("Amiri-Regular.ttf"), &planted).unwrap();

    let scanned = s.run(&["scan", "--system"]);
    assert!(
        scanned.status.success(),
        "{}",
        String::from_utf8_lossy(&scanned.stderr)
    );

    // The face from `fonts/`, not the planted copy: ask what stands in its way.
    let listed: serde_json::Value =
        serde_json::from_slice(&s.run(&["list", "--json"]).stdout).expect("JSON");
    let mine = listed
        .as_array()
        .expect("a list")
        .iter()
        .find(|f| {
            f["path"]
                .as_str()
                .is_some_and(|p| p.contains("fonts") && !p.contains(&*dir.to_string_lossy()))
        })
        .expect("the scanned face is still there");
    let id = mine["id"].to_string();

    let out = s.run(&["conflicts", &id]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a clash with an installed font is exit 2: {stderr}"
    );
    assert!(
        stderr.contains("system font directory"),
        "and it says where the other one lives: {stderr}"
    );
    assert!(
        !stderr.contains("scan --system"),
        "the note is for an index that cannot see, and this one can: {stderr}"
    );
}
