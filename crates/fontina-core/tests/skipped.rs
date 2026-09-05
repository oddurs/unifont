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

//! A scan that walks past a font has to say so.
//!
//! The extension filter runs before anything is opened, so a font in a format fontina
//! cannot read was never considered and never mentioned: three fonts in a directory
//! reported one candidate and no failures. A parse error can be acted on; a file nobody
//! looked at leaves a person believing their library is indexed.

use fontina_core::scan;
use std::path::{Path, PathBuf};

/// A directory of our own, removed on the way out.
///
/// The same shape the other tests in this crate use rather than a new dependency:
/// `std::env::temp_dir` plus the process id, which is unique enough for a test binary
/// and needs nothing added to the manifest.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fontina-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        Dir(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Macintosh resource fork header: data at 256, and a map that ends at the file's end.
/// This is the shape macOS ships its datafork Type 1 fonts in.
fn resource_fork(total: usize) -> Vec<u8> {
    let map_len: u32 = 64;
    let map_at = total as u32 - map_len;
    let data_len = map_at - 256;
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&256u32.to_be_bytes());
    bytes.extend_from_slice(&map_at.to_be_bytes());
    bytes.extend_from_slice(&data_len.to_be_bytes());
    bytes.extend_from_slice(&map_len.to_be_bytes());
    bytes.resize(total, 0);
    bytes
}

#[test]
fn a_scan_names_the_fonts_it_cannot_read() {
    let dir = Dir::new("skipped");
    let at = |name: &str| dir.path().join(name);

    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/Amiri-Regular.ttf"),
        at("Amiri-Regular.ttf"),
    )
    .expect("the fixture copies");
    std::fs::write(at("fixed.bdf"), {
        let mut v = b"STARTFONT 2.1\nFONT -misc-fixed\n".to_vec();
        v.resize(256, b'\n');
        v
    })
    .unwrap();
    // No extension at all, which is exactly why the filter never saw these.
    std::fs::write(at("HelveLTMM"), resource_fork(4096)).unwrap();
    std::fs::write(at("notes.txt"), vec![b'x'; 512]).unwrap();

    let (candidates, skipped) = scan::walk(&[dir.path().to_path_buf()], false);

    assert_eq!(candidates.len(), 1, "only the TTF is a candidate");
    let mut formats: Vec<&str> = skipped.iter().map(|s| s.format).collect();
    formats.sort_unstable();
    assert_eq!(
        formats,
        vec!["BDF bitmap", "Mac resource fork (.dfont, datafork Type 1)"],
        "both fonts are named, and the text file is not"
    );
}

/// The resource-fork check is arithmetic rather than a magic number, because
/// `00 00 01 00` opens plenty of files that are not fonts. Telling somebody their
/// spreadsheet is a font is worse than saying nothing.
#[test]
fn a_file_that_merely_starts_like_a_resource_fork_is_not_a_font() {
    let dir = Dir::new("notafork");
    let path = dir.path().join("something.bin");
    // The right first four bytes, and a map that does not end where the file does.
    let mut bytes = resource_fork(4096);
    bytes.truncate(2048);
    std::fs::write(&path, bytes).unwrap();

    let (_, skipped) = scan::walk(&[dir.path().to_path_buf()], false);
    assert!(
        skipped.is_empty(),
        "the header has to add up, not merely begin correctly: {skipped:?}"
    );
}

/// Sniffing costs a read per non-candidate, so it refuses the sizes a font never is.
#[test]
fn a_file_too_small_to_be_a_font_is_not_opened() {
    let dir = Dir::new("tiny");
    std::fs::write(dir.path().join("tiny"), b"STARTFONT").unwrap();
    let (_, skipped) = scan::walk(&[dir.path().to_path_buf()], false);
    assert!(skipped.is_empty(), "nine bytes is not a bitmap font");
}
