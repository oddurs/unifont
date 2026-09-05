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

//! Tags, collections, sources, activation state, facets and families against the
//! fixture fonts in an in-memory index.

use fontina_core::{
    ActivationState, CollectionExport, FaceFilter, Freedom, Index, ScanOptions, SourceKind,
};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn indexed() -> Index {
    let mut index = Index::open_in_memory().unwrap();
    let report =
        fontina_core::scan::scan(&mut index, &[fixtures()], &ScanOptions::default()).unwrap();
    assert_eq!(report.faces, 6, "{:?}", report.failed);
    index
}

fn id_of(index: &Index, family: &str) -> i64 {
    index
        .list(&FaceFilter {
            family: Some(family.into()),
            ..Default::default()
        })
        .unwrap()[0]
        .id
}

#[test]
fn tags_round_trip_and_filter() {
    let mut index = indexed();
    let amiri = id_of(&index, "Amiri");
    let serif = id_of(&index, "Source Serif 4");
    assert_eq!(index.tag(&[amiri, serif], "serif").unwrap(), 2);
    assert_eq!(index.tag(&[amiri], "serif").unwrap(), 0, "idempotent");
    assert_eq!(index.tag(&[amiri], "Arabic").unwrap(), 1);
    assert!(index.tag(&[amiri], "  ").is_err(), "blank tag rejected");

    let tags = index.tags().unwrap();
    assert_eq!(
        tags.iter()
            .map(|t| (t.name.as_str(), t.faces))
            .collect::<Vec<_>>(),
        [("Arabic", 1), ("serif", 2)]
    );
    let tagged = index
        .list(&FaceFilter {
            tag: Some("SERIF".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(tagged.len(), 2);
    assert_eq!(
        tagged[0].tags,
        ["Arabic", "serif"],
        "tags ride on summaries, sorted"
    );

    assert!(index.rename_tag("serif", "Serif Fonts").unwrap());
    assert_eq!(index.untag(&[serif], "serif fonts").unwrap(), 1);
    assert!(index.delete_tag("arabic").unwrap());
    assert!(!index.delete_tag("nope").unwrap());
    assert_eq!(index.summaries(&[amiri]).unwrap()[0].tags, ["Serif Fonts"]);
}

/// A shared collection travels with its fonts, so its paths have to mean something on
/// the machine that receives it — and an absolute path both means nothing there and
/// carries a home directory along with it.
#[test]
fn a_relative_export_survives_being_moved() {
    let here = fixtures().canonicalize().unwrap();
    let mut export = collection_over_every_fixture("Shared");
    assert!(!export.relative_paths);
    assert!(
        export
            .faces
            .iter()
            .all(|f| Path::new(&f.path).is_absolute()),
        "an ordinary export is absolute"
    );

    export.relative_to(&here).unwrap();
    assert!(export.relative_paths);
    assert!(
        export
            .faces
            .iter()
            .all(|f| Path::new(&f.path).is_relative()),
        "every fixture is under the base, so every path became relative"
    );
    assert!(
        !serde_json::to_string(&export)
            .unwrap()
            .contains(here.to_string_lossy().as_ref()),
        "the exported file no longer names the directory it came from"
    );
    assert!(
        export.faces.iter().all(|f| !f.path.contains('\\')),
        "paths go on the wire with `/`, so a bundle written on Windows opens elsewhere"
    );

    // The same bundle, opened from somewhere else, points at the fonts beside it.
    let mut moved = export.clone();
    let escaped = moved.resolve_paths(&here);
    assert_eq!(escaped, 0);
    assert!(!moved.relative_paths);
    assert!(
        moved
            .faces
            .iter()
            .all(|f| Path::new(&f.path).starts_with(&here)),
        "{:?}",
        moved.faces.first().map(|f| &f.path)
    );

    // Resolving an export that was never relative leaves it alone.
    let mut absolute = collection_over_every_fixture("Absolute");
    let before: Vec<String> = absolute.faces.iter().map(|f| f.path.clone()).collect();
    assert_eq!(absolute.resolve_paths(&here), 0);
    let after: Vec<String> = absolute.faces.iter().map(|f| f.path.clone()).collect();
    assert_eq!(after, before);
}

/// A collection over every fixture, exported with absolute paths.
fn collection_over_every_fixture(name: &str) -> CollectionExport {
    let mut index = indexed();
    let ids: Vec<i64> = index
        .list(&FaceFilter::default())
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    index.create_collection(name).unwrap();
    index.add_to_collection(name, &ids).unwrap();
    index.export_collection(name).unwrap()
}

/// Half-relative is worse than absolute: a reader that trusts the flag and joins the
/// base onto a path that never became relative gets nonsense. So it refuses.
#[test]
fn an_export_that_cannot_be_made_relative_refuses_rather_than_lying() {
    let mut export = collection_over_every_fixture("Outside");
    let before: Vec<String> = export.faces.iter().map(|f| f.path.clone()).collect();

    // A real directory that holds none of the fonts.
    let elsewhere = std::env::temp_dir().canonicalize().unwrap();
    let err = export.relative_to(&elsewhere).unwrap_err();
    assert!(format!("{err}").contains("outside"), "{err}");
    assert!(
        !export.relative_paths,
        "a refused export is not marked relative"
    );
    assert_eq!(
        export
            .faces
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>(),
        before,
        "and nothing was rewritten on the way to refusing"
    );

    // A base that cannot be canonicalised at all is an error, not a silent no-op.
    // `strip_prefix("")` succeeds for every path, so falling back to an empty base would
    // have marked the export relative while leaving every path absolute — a file that
    // still names someone's home directory and resolves to nothing anywhere else.
    let mut export = collection_over_every_fixture("Missing");
    assert!(export.relative_to(Path::new("/no/such/bundle")).is_err());
    assert!(!export.relative_paths);
    assert!(
        export
            .faces
            .iter()
            .all(|f| Path::new(&f.path).is_absolute())
    );
}

/// A collection file is written by somebody else.
#[test]
fn a_path_that_climbs_out_of_the_bundle_is_not_resolved() {
    let mut export = collection_over_every_fixture("Hostile");
    export.relative_paths = true;
    export.faces[0].path = "../../../../etc/hosts".into();
    let escaped = export.resolve_paths(Path::new("/srv/bundle"));
    assert_eq!(escaped, 1);
    assert_eq!(
        export.faces[0].path, "../../../../etc/hosts",
        "it is left as it was, so nothing downstream is handed it as a real path"
    );
}

/// The whole point of a bundle: hand somebody a directory and they have the collection
/// *and* the fonts, with nothing in the file that only meant something on your disk.
#[test]
fn a_bundle_carries_the_fonts_with_it() {
    let dir = scratch("bundle");
    let export = collection_over_every_fixture("Handoff");
    let report = export.write_bundle(&dir).unwrap();
    assert_eq!(report.faces, 6);
    assert_eq!(report.files, 6, "one file per fixture");
    assert!(report.bytes > 0);

    // `self` is the caller's; writing a bundle does not rewrite it underneath them.
    assert!(!export.relative_paths);
    assert!(
        export
            .faces
            .iter()
            .all(|f| Path::new(&f.path).is_absolute())
    );

    let text = std::fs::read_to_string(dir.join(fontina_core::BUNDLE_FILE)).unwrap();
    assert!(
        !text.contains(
            fixtures()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        ),
        "the file no longer names the machine it was written on"
    );
    let mut read: CollectionExport = serde_json::from_str(&text).unwrap();
    assert!(read.relative_paths);
    assert!(
        read.faces
            .iter()
            .all(|f| f.path.starts_with("fonts/") && !f.path.contains('\\')),
        "{:?}",
        read.faces.iter().map(|f| &f.path).collect::<Vec<_>>()
    );

    // Opened from where it sits, every path is a font that is really there.
    assert_eq!(read.resolve_paths(&dir), 0);
    for f in &read.faces {
        assert!(Path::new(&f.path).is_file(), "{}", f.path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fonts live in per-family directories, so a bundle flattening them will meet two files
/// called the same thing — and holding one of them twice under the other's name would be
/// a collection that silently lies about what is in it.
#[test]
fn two_fonts_with_one_name_both_survive_the_flattening() {
    let dir = scratch("bundle-clash");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::copy(fixtures().join("Amiri-Regular.ttf"), a.join("Regular.ttf")).unwrap();
    std::fs::copy(
        fixtures().join("SourceSerif4-Regular.otf"),
        b.join("Regular.ttf"),
    )
    .unwrap();

    let mut export = collection_over_every_fixture("Clash");
    export.faces.truncate(2);
    export.faces[0].path = a.join("Regular.ttf").to_string_lossy().into_owned();
    export.faces[1].path = b.join("Regular.ttf").to_string_lossy().into_owned();

    let out = dir.join("out");
    let report = export.write_bundle(&out).unwrap();
    assert_eq!(report.files, 2);
    let mut read: CollectionExport = serde_json::from_str(
        &std::fs::read_to_string(out.join(fontina_core::BUNDLE_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(read.faces[0].path, "fonts/Regular.ttf");
    assert_eq!(read.faces[1].path, "fonts/Regular-2.ttf");
    read.resolve_paths(&out);
    assert_ne!(
        std::fs::read(&read.faces[0].path).unwrap(),
        std::fs::read(&read.faces[1].path).unwrap(),
        "the second is the second font, not a second copy of the first"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TrueType collection, or two instances of one variable font, are several faces over
/// one file. Copying it per face would inflate the bundle and leave duplicates behind.
#[test]
fn faces_that_share_a_file_share_its_copy() {
    let dir = scratch("bundle-shared");
    let mut export = collection_over_every_fixture("Shared file");
    export.faces.truncate(2);
    let one = fixtures()
        .canonicalize()
        .unwrap()
        .join("Amiri-Regular.ttf")
        .to_string_lossy()
        .into_owned();
    export.faces[0].path = one.clone();
    export.faces[1].path = one;
    export.faces[1].index = 1;

    let report = export.write_bundle(&dir).unwrap();
    assert_eq!(report.faces, 2);
    assert_eq!(report.files, 1, "copied once");
    let read: CollectionExport = serde_json::from_str(
        &std::fs::read_to_string(dir.join(fontina_core::BUNDLE_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(read.faces[0].path, read.faces[1].path);
    assert_eq!(
        std::fs::read_dir(dir.join(fontina_core::BUNDLE_FONTS))
            .unwrap()
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Writing a second collection into a directory that already holds one would leave a
/// `collection.json` naming half the fonts beside it. Making a new directory is free.
#[test]
fn a_bundle_will_not_be_written_over_one_that_is_already_there() {
    let dir = scratch("bundle-twice");
    let first = collection_over_every_fixture("First");
    first.write_bundle(&dir).unwrap();
    let before = std::fs::read_to_string(dir.join(fontina_core::BUNDLE_FILE)).unwrap();

    let mut second = collection_over_every_fixture("Second");
    second.faces.truncate(1);
    let err = second.write_bundle(&dir).unwrap_err();
    assert!(format!("{err}").contains("already holds a bundle"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join(fontina_core::BUNDLE_FILE)).unwrap(),
        before,
        "and the one that was there is untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory nothing else in this run is using.
fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fontina-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // macOS puts the temp directory behind a symlink, and `relative_to` strips a
    // canonical base.
    std::fs::canonicalize(&dir).unwrap()
}

/// A variable font is not one weight, and filtering on its default instance loses it.
///
/// Bricolage spans `wght` 200 to 800 and defaults to 800, so `--weight 400` used to
/// return everything except the one font in the fixtures that actually does 400 across
/// the whole range. This is the assertion the fix exists for.
#[test]
fn a_variable_font_is_found_at_every_weight_it_spans() {
    let index = indexed();
    let at = |lo: u16, hi: u16| -> Vec<String> {
        let mut families: Vec<String> = index
            .list(&FaceFilter {
                weight: Some((lo, hi)),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|f| f.family)
            .collect();
        families.sort();
        families.dedup();
        families
    };

    assert!(
        at(400, 400).contains(&"Bricolage Grotesque".to_string()),
        "wght 200-800 covers 400: {:?}",
        at(400, 400)
    );
    assert!(
        at(200, 200) == vec!["Bricolage Grotesque".to_string()],
        "and nothing static reaches 200: {:?}",
        at(200, 200)
    );
    assert!(
        at(800, 800).contains(&"Bricolage Grotesque".to_string()),
        "its default instance still matches"
    );

    // The range is a range, not a licence to match everything.
    assert!(
        at(900, 900).is_empty(),
        "nothing in the fixtures reaches 900: {:?}",
        at(900, 900)
    );
    assert!(
        !at(100, 199).contains(&"Bricolage Grotesque".to_string()),
        "below the axis is below the axis"
    );

    // A static face is still exactly its own weight, which is the old behaviour.
    let regulars = at(400, 400);
    assert!(regulars.contains(&"Amiri".to_string()));
    assert!(!at(500, 500).contains(&"Amiri".to_string()));

    // An asked-for range that merely overlaps is a match: 350-450 crosses Bricolage.
    assert!(at(350, 450).contains(&"Bricolage Grotesque".to_string()));
}

/// Two scripts mean a face that covers both.
///
/// `faces.scripts` is a comma-joined string matched with `LIKE '%,Arab,%'`, and one
/// `LIKE` cannot express two scripts at once. `face_scripts` is a row per script, so
/// each one asked for is its own clause and they compose.
#[test]
fn asking_for_two_scripts_means_a_face_that_has_both() {
    let index = indexed();
    let of = |scripts: &[&str]| -> Vec<String> {
        let mut names: Vec<String> = index
            .list(&FaceFilter {
                scripts: scripts.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|f| f.family)
            .collect();
        names.sort();
        names.dedup();
        names
    };

    assert_eq!(of(&["Arab"]), vec!["Amiri".to_string()]);
    assert_eq!(of(&["Cyrl"]), vec!["Source Serif 4".to_string()]);
    // Both, from two different faces, is not a face that has both.
    assert!(
        of(&["Arab", "Cyrl"]).is_empty(),
        "no fixture covers Arabic and Cyrillic: {:?}",
        of(&["Arab", "Cyrl"])
    );
    // But Source Serif has Cyrillic and Greek.
    assert_eq!(
        of(&["Cyrl", "Grek"]),
        vec!["Source Serif 4".to_string()],
        "{:?}",
        of(&["Cyrl", "Grek"])
    );
    // A script nothing has takes everything else with it.
    assert!(of(&["Latn", "Hani"]).is_empty());
    // And no script asked for is no clause at all.
    assert_eq!(of(&[]).len(), 5, "{:?}", of(&[]));
}

/// Coverage has depth, and three Arabic codepoints is not an Arabic font.
///
/// `Coverage.scripts` has counted this since M0 and the filter threw it away.
#[test]
fn a_script_filter_can_ask_how_much_of_it() {
    let index = indexed();
    let latin = |min: u32| -> usize {
        index
            .list(&FaceFilter {
                scripts: vec!["Latn".into()],
                script_min: Some(min),
                ..Default::default()
            })
            .unwrap()
            .len()
    };
    let all = latin(1);
    assert_eq!(all, 6, "every fixture has some Latin");

    // The depth the fixtures actually hold, so the test says something real rather than
    // asserting a number somebody typed.
    let depths: Vec<u32> = index
        .list(&FaceFilter::default())
        .unwrap()
        .iter()
        .map(|f| {
            index
                .script_coverage(f.id)
                .unwrap()
                .into_iter()
                .find(|(s, _)| s == "Latn")
                .map(|(_, n)| n)
                .unwrap_or(0)
        })
        .collect();
    let deepest = *depths.iter().max().unwrap();
    let shallowest = *depths.iter().min().unwrap();
    assert!(
        deepest > shallowest,
        "the fixtures differ in how much Latin they have: {depths:?}"
    );

    assert_eq!(latin(shallowest), 6, "the floor keeps everyone");
    assert!(
        latin(shallowest + 1) < 6,
        "and asking for more drops the shallowest: {depths:?}"
    );
    assert_eq!(latin(deepest + 1), 0, "nothing is deeper than the deepest");
}

/// "Which of my fonts declare Vietnamese" is a question the index could not be asked.
///
/// Both answers were parsed and stored from the start — language system tags under each
/// OpenType script, and BCP 47 on every localised name record — and `FaceFilter` had no
/// language field at all.
#[test]
fn a_face_can_be_found_by_a_language_it_claims() {
    let index = indexed();
    let of = |tag: &str| -> Vec<String> {
        let mut names: Vec<String> = index
            .list(&FaceFilter {
                lang: Some(tag.into()),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|f| f.family)
            .collect();
        names.sort();
        names.dedup();
        names
    };

    // Arabic and Urdu are Amiri's alone; Kazakh is Bricolage's; Dutch is Source Serif's.
    assert_eq!(of("ARA"), vec!["Amiri".to_string()]);
    assert_eq!(of("URD"), vec!["Amiri".to_string()]);
    assert_eq!(of("KAZ"), vec!["Bricolage Grotesque".to_string()]);
    assert_eq!(of("NLD"), vec!["Source Serif 4".to_string()]);
    // Turkish is declared by three of the five families, which is the useful shape of
    // this question: it narrows without being unique.
    assert_eq!(
        of("TRK"),
        vec![
            "Amiri".to_string(),
            "Bricolage Grotesque".to_string(),
            "Source Serif 4".to_string()
        ]
    );

    // Case is not part of the claim.
    assert_eq!(of("trk"), of("TRK"));
    // The tag is stored unpadded: OpenType pads to four bytes and the padding is the
    // format's, not the language's.
    assert!(of("TRK ").is_empty(), "the space is not part of it");
    // Nothing here claims Klingon.
    assert!(of("TLH").is_empty());
}

/// The two claims are different claims, and the filter can say which it means.
///
/// A language system tag says the shaping engine has rules for that language. A BCP 47
/// tag on a name record only says the font names itself in it, which says nothing about
/// whether it can set a word of it. Merging them would over-report in one direction and
/// under-report in the other, with no way for a reader to tell which had happened.
///
/// Source Serif makes both claims about Bulgarian and makes them under different tags:
/// `BGR` to the shaping engine, `bg` on its name records.
#[test]
fn an_opentype_claim_and_a_name_record_are_not_the_same_claim() {
    use fontina_core::LanguageSource;
    let index = indexed();
    let id = id_of(&index, "Source Serif 4");
    let claims = index.languages(id).unwrap();

    let kinds = |source: LanguageSource| -> Vec<String> {
        claims
            .iter()
            .filter(|(_, s)| *s == source)
            .map(|(t, _)| t.clone())
            .collect()
    };
    assert!(kinds(LanguageSource::Opentype).contains(&"BGR".to_string()));
    assert!(kinds(LanguageSource::Name).contains(&"bg".to_string()));
    assert!(
        !kinds(LanguageSource::Opentype).contains(&"bg".to_string()),
        "the two namespaces are kept apart: {claims:?}"
    );

    let by_kind = |tag: &str, source: LanguageSource| {
        index
            .list(&FaceFilter {
                lang: Some(tag.into()),
                lang_source: Some(source),
                ..Default::default()
            })
            .unwrap()
            .len()
    };
    assert_eq!(by_kind("BGR", LanguageSource::Opentype), 1);
    assert_eq!(
        by_kind("BGR", LanguageSource::Name),
        0,
        "no name record carries the OpenType tag"
    );
    assert_eq!(by_kind("bg", LanguageSource::Name), 1);
    assert_eq!(
        by_kind("bg", LanguageSource::Opentype),
        0,
        "and no shaping rule carries the BCP 47 one"
    );

    // Nabla declares no language system at all, and still names itself in English.
    let nabla = index.languages(id_of(&index, "Nabla")).unwrap();
    assert!(
        nabla.iter().all(|(_, s)| *s == LanguageSource::Name),
        "{nabla:?}"
    );
    assert!(
        !nabla.is_empty(),
        "naming itself is still a claim, just a weaker one"
    );
}

/// Identical coverage is not identity, and the fixtures already hold the pair that
/// proves it.
///
/// `inter-latin-400-normal.woff` and `.woff2` cover exactly the same codepoints in the
/// same ranges and are built a few glyphs apart. They must score 1.0 while remaining two
/// different files — which is the whole argument for printing the metrics beside the
/// score rather than thresholding on it.
#[test]
fn coverage_overlap_finds_the_same_font_in_two_containers() {
    let index = indexed();
    let inter = index
        .list(&FaceFilter {
            family: Some("Inter".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(inter.len(), 2, "one WOFF and one WOFF2");

    let related = index.related(inter[0].id, 0.0).unwrap();
    let twin = related
        .iter()
        .find(|r| r.face.id == inter[1].id)
        .expect("the other container is in the answer");
    assert_eq!(twin.overlap, 1.0, "the same codepoints, exactly");
    assert_eq!(twin.shared, twin.union);
    assert!(twin.metrics_agree, "and the same design");
    assert_ne!(
        twin.face.path, inter[0].path,
        "still two different files, which is why the score is not the verdict"
    );

    // Sorted most alike first, so the twin leads.
    assert_eq!(related[0].face.id, inter[1].id, "{related:?}");

    // Amiri against Source Serif is near zero: both cover Latin, and little else in
    // common. A threshold that admits them would admit anything.
    let amiri = id_of(&index, "Amiri");
    let source = id_of(&index, "Source Serif 4");
    let far = index
        .related(amiri, 0.0)
        .unwrap()
        .into_iter()
        .find(|r| r.face.id == source)
        .expect("still in the answer at min 0.0");
    assert!(
        far.overlap < 0.25,
        "Arabic and Cyrillic barely intersect: {}",
        far.overlap
    );

    // `min` is a floor, not a suggestion.
    let strict = index.related(inter[0].id, 0.99).unwrap();
    assert_eq!(strict.len(), 1, "only the twin clears 0.99: {strict:?}");
    assert!(index.related(inter[0].id, 1.01).unwrap().is_empty());

    // A face is never related to itself; that is what `dupes` is for.
    assert!(
        index
            .related(inter[0].id, 0.0)
            .unwrap()
            .iter()
            .all(|r| r.face.id != inter[0].id)
    );

    // Asking about a face that is not there is an error, not an empty answer.
    assert!(index.related(99999, 0.0).is_err());
}

/// One row this build cannot read must not take the whole query with it.
///
/// Every M4 backfill tolerates exactly this row and says why, and `list` keeps working on
/// an index that holds one. `related` read the metadata of every *candidate* to compare
/// their metrics, and propagated — so a single unreadable face, very likely not even the
/// one being asked about, failed all of `fontina variants`.
#[test]
fn a_candidate_with_unreadable_metadata_does_not_fail_the_whole_question() {
    let dir = std::env::temp_dir().join(format!("fontina-related-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("index.db");
    {
        let mut idx = Index::open(&db).unwrap();
        fontina_core::scan::scan(&mut idx, &[fixtures()], &ScanOptions::default()).unwrap();
    }

    let target = {
        let idx = Index::open(&db).unwrap();
        let inter = idx
            .list(&FaceFilter {
                family: Some("Inter".into()),
                ..Default::default()
            })
            .unwrap();
        // Corrupt the twin — the one candidate that matters most to this target.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE faces SET metadata = '{ not json' WHERE id = ?1",
            [inter[1].id],
        )
        .unwrap();
        inter[0].id
    };

    let idx = Index::open(&db).unwrap();
    let related = idx
        .related(target, 0.0)
        .expect("one bad row is not a reason to answer nothing");
    assert!(!related.is_empty());

    // The corrupt candidate is still offered, still scored — coverage comes from
    // `face_ranges`, which is intact — and reported as not known to agree.
    let twin = related
        .iter()
        .max_by(|a, b| a.overlap.total_cmp(&b.overlap))
        .unwrap();
    assert_eq!(
        twin.overlap, 1.0,
        "the coverage question is still answerable"
    );
    assert!(
        !twin.metrics_agree,
        "fontina cannot read its metrics, so it must not claim they agree"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The score is a real Jaccard, not a coverage ratio dressed up as one.
///
/// A subset scores below 1.0 even though every codepoint it has is shared, because the
/// union is the larger set. Getting this wrong would rank every small font as a perfect
/// match for every large one that contains it.
#[test]
fn overlap_is_the_intersection_over_the_union() {
    let index = indexed();
    let inter = index
        .list(&FaceFilter {
            family: Some("Inter".into()),
            ..Default::default()
        })
        .unwrap();
    let related = index.related(inter[0].id, 0.0).unwrap();

    for r in &related {
        assert!(
            (0.0..=1.0).contains(&r.overlap),
            "{} is not a similarity",
            r.overlap
        );
        assert!(r.shared <= r.union, "{r:?}");
        assert!(
            (r.overlap - f64::from(r.shared) / f64::from(r.union)).abs() < 1e-9,
            "the score is the ratio it says it is: {r:?}"
        );
    }

    // Every candidate that is not the twin shares less than it covers in total, so none
    // of them can reach 1.0 by covering a subset.
    let others: Vec<_> = related
        .iter()
        .filter(|r| r.face.id != inter[1].id)
        .collect();
    assert!(!others.is_empty());
    assert!(
        others.iter().all(|r| r.overlap < 1.0),
        "a subset is not a match: {others:?}"
    );
}

/// What `post.isFixedPitch` says, reported and never second-guessed.
///
/// §12: a font whose advance widths contradict its own flag is a health check, not a
/// filter that quietly disagrees with the file. So the filter reads the flag, and the
/// only monospace claim in these tests is one written into a font's own metadata.
///
/// No fixture is monospaced — the positive case is made by mutating a parsed fixture,
/// the way `license/nonfree` is triggered, and then driving it through the real backfill.
#[test]
fn a_font_is_monospaced_when_it_says_it_is() {
    let index = indexed();
    let count = |mono: Option<bool>| {
        index
            .list(&FaceFilter {
                monospace: mono,
                ..Default::default()
            })
            .unwrap()
            .len()
    };
    assert_eq!(count(None), 6);
    assert_eq!(count(Some(false)), 6, "none of the fixtures claims to be");
    assert_eq!(count(Some(true)), 0);

    let facets = index.facets(&FaceFilter::default()).unwrap();
    let spacing: std::collections::BTreeMap<&str, i64> = facets
        .spacing
        .iter()
        .map(|c| (c.value.as_str(), c.count))
        .collect();
    assert_eq!(spacing.get("proportional"), Some(&6));
    assert!(!spacing.contains_key("monospace"), "{spacing:?}");
    assert!(
        index
            .list(&FaceFilter::default())
            .unwrap()
            .iter()
            .all(|f| !f.monospace),
        "and the summary agrees with the filter"
    );

    // A font that does claim it. The claim is written into the stored metadata and read
    // back by the v7 backfill, which is the code path a real library will take.
    let dir = std::env::temp_dir().join(format!("fontina-mono-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("v6.db");
    {
        let mut idx = Index::open(&db).unwrap();
        fontina_core::scan::scan(
            &mut idx,
            &[fixtures().join("Amiri-Regular.ttf")],
            &ScanOptions::default(),
        )
        .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let json: String = conn
            .query_row("SELECT metadata FROM faces LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let mut face: fontina_core::FaceMetadata = serde_json::from_str(&json).unwrap();
        assert!(
            !face.metrics.is_fixed_pitch,
            "the fixture starts out saying no"
        );
        face.metrics.is_fixed_pitch = true;
        conn.execute(
            "UPDATE faces SET metadata = ?1",
            [serde_json::to_string(&face).unwrap()],
        )
        .unwrap();
        conn.execute_batch(
            "DROP INDEX faces_fixed_pitch;
             ALTER TABLE faces DROP COLUMN is_fixed_pitch;
             PRAGMA user_version = 6;",
        )
        .unwrap();
    }

    let idx = Index::open(&db).unwrap();
    let mono = idx
        .list(&FaceFilter {
            monospace: Some(true),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        mono.len(),
        1,
        "the backfill read the claim out of the metadata"
    );
    assert!(mono[0].monospace);
    assert!(
        idx.list(&FaceFilter {
            monospace: Some(false),
            ..Default::default()
        })
        .unwrap()
        .is_empty()
    );
    let facets = idx.facets(&FaceFilter::default()).unwrap();
    assert_eq!(
        facets
            .spacing
            .iter()
            .map(|c| c.value.as_str())
            .collect::<Vec<_>>(),
        vec!["monospace"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The language facet offers both kinds of claim, and every offer is honoured.
#[test]
fn the_language_facet_offers_what_the_filter_will_return() {
    let index = indexed();
    let facets = index.facets(&FaceFilter::default()).unwrap();
    assert!(!facets.language.is_empty());

    for c in &facets.language {
        let found = index
            .list(&FaceFilter {
                lang: Some(c.value.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            found.len() as i64,
            c.count,
            "the facet offers {} for {} and the filter returns {}",
            c.count,
            c.value,
            found.len()
        );
    }

    let by_value: std::collections::BTreeMap<&str, i64> = facets
        .language
        .iter()
        .map(|c| (c.value.as_str(), c.count))
        .collect();
    // Both namespaces are on the list, side by side and not merged.
    assert_eq!(
        by_value.get("en"),
        Some(&6),
        "every fixture names itself in English"
    );
    assert_eq!(by_value.get("BGR"), Some(&1), "{by_value:?}");
    assert_eq!(by_value.get("bg"), Some(&1), "{by_value:?}");
    assert_eq!(by_value.get("TRK"), Some(&3), "{by_value:?}");

    // A face claiming a language twice under one tag is still one face: the count is
    // faces, not claims.
    assert!(
        facets.language.iter().all(|c| c.count <= facets.faces),
        "{:?}",
        facets.language
    );

    // Following the filter, as every other facet does.
    let arabic = index
        .facets(&FaceFilter {
            lang: Some("ARA".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(arabic.faces, 1);
    assert!(
        arabic.language.iter().all(|c| c.count == 1),
        "{:?}",
        arabic.language
    );
}

/// Filled from what is already stored, so an older index answers without a rescan.
#[test]
fn an_older_index_learns_its_languages_without_a_rescan() {
    let dir = std::env::temp_dir().join(format!("fontina-langs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("v5.db");
    {
        let mut idx = Index::open(&db).unwrap();
        fontina_core::scan::scan(
            &mut idx,
            &[fixtures().join("BricolageGrotesque[opsz,wdth,wght].ttf")],
            &ScanOptions::default(),
        )
        .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
                "DROP TABLE face_languages; DROP INDEX faces_fixed_pitch; ALTER TABLE faces DROP COLUMN is_fixed_pitch;
                 PRAGMA user_version = 5;",
            )
            .unwrap();
    }
    let idx = Index::open(&db).unwrap();
    let found = idx
        .list(&FaceFilter {
            lang: Some("KAZ".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(found.len(), 1, "read out of the stored metadata");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The script facet is ordered by how much of each script there is.
///
/// `Zyyy` and `Zinh` — common punctuation and inherited marks — are in almost every font
/// and are never what it is for. Ordering by face count puts them at the top of the list
/// a person is scanning for "which of my fonts do Arabic". Depth says which is which, and
/// the index has counted it since the `face_scripts` table.
#[test]
fn the_script_facet_leads_with_the_scripts_a_font_is_actually_for() {
    let index = indexed();
    let facets = index.facets(&FaceFilter::default()).unwrap();
    let order: Vec<&str> = facets.script.iter().map(|c| c.value.as_str()).collect();

    let pos = |s: &str| order.iter().position(|v| *v == s).unwrap_or(usize::MAX);
    assert!(
        pos("Arab") < pos("Zinh"),
        "Amiri's thousands of Arabic codepoints outrank marks every font has: {order:?}"
    );
    assert!(pos("Latn") < pos("Zinh"), "{order:?}");
    assert!(
        pos("Zyyy") < pos("Zinh"),
        "and depth orders the two common scripts against each other: {order:?}"
    );

    // The count is faces, which is what the facet means and what clicking it returns.
    for c in &facets.script {
        let found = index
            .list(&FaceFilter {
                scripts: vec![c.value.clone()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            found.len() as i64,
            c.count,
            "the facet offers {} for {} and the filter returns {}",
            c.count,
            c.value,
            found.len()
        );
    }

    // Following the filter, as every other facet does.
    let arabic = index
        .facets(&FaceFilter {
            scripts: vec!["Arab".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(arabic.faces, 1);
    assert!(
        arabic.script.iter().all(|c| c.count == 1),
        "one face, so every script it has is offered once: {:?}",
        arabic.script
    );
}

/// The table is filled from the metadata already stored, so an older index answers the
/// new question without anyone rescanning.
#[test]
fn an_older_index_learns_its_scripts_without_a_rescan() {
    let dir = std::env::temp_dir().join(format!("fontina-scripts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("v4.db");
    {
        let mut idx = Index::open(&db).unwrap();
        fontina_core::scan::scan(
            &mut idx,
            &[fixtures().join("Amiri-Regular.ttf")],
            &ScanOptions::default(),
        )
        .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE face_scripts; DROP TABLE face_languages; DROP INDEX faces_fixed_pitch; ALTER TABLE faces DROP COLUMN is_fixed_pitch;
             PRAGMA user_version = 4;",
        )
        .unwrap();
    }
    let idx = Index::open(&db).unwrap();
    let found = idx
        .list(&FaceFilter {
            scripts: vec!["Arab".into()],
            script_min: Some(100),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(found.len(), 1, "the depth came out of the stored metadata");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The facet and the filter beside it must give the same answer.
///
/// A facet is a menu of what asking would return. Counting a variable face only under
/// its default instance, while the filter finds it across its whole axis, makes the
/// browser show "400 Regular 5" next to a query that returns six — a disagreement the
/// reader has no way to explain.
#[test]
fn every_weight_the_facet_offers_returns_what_it_promised() {
    let index = indexed();
    let facets = index.facets(&FaceFilter::default()).unwrap();
    assert!(
        facets.weight.len() > 1,
        "the fixtures span more than one bucket: {:?}",
        facets.weight
    );
    for bucket in &facets.weight {
        let b: u16 = bucket.value.parse().expect("a weight bucket is a number");
        let found = index
            .list(&FaceFilter {
                weight: Some((b, b)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            found.len() as i64,
            bucket.count,
            "the facet offers {} at weight {b} and the filter returns {}",
            bucket.count,
            found.len()
        );
    }

    // Bricolage spans 200-800, so it is counted in all seven of those buckets and not
    // in the two outside them.
    let by_value: std::collections::BTreeMap<&str, i64> = facets
        .weight
        .iter()
        .map(|c| (c.value.as_str(), c.count))
        .collect();
    assert_eq!(by_value.get("200"), Some(&1));
    assert_eq!(by_value.get("800"), Some(&1));
    assert_eq!(by_value.get("400"), Some(&6), "five static plus Bricolage");
    assert!(!by_value.contains_key("100"), "{by_value:?}");
    assert!(!by_value.contains_key("900"), "{by_value:?}");

    // A face is counted once per bucket, so the column sums to more than the library.
    // That is what a multi-valued facet does — `script` has always behaved this way.
    let total: i64 = facets.weight.iter().map(|c| c.count).sum();
    assert!(total > facets.faces, "{total} vs {}", facets.faces);
}

/// The same, for width.
#[test]
fn every_width_the_facet_offers_returns_what_it_promised() {
    let index = indexed();
    let facets = index.facets(&FaceFilter::default()).unwrap();
    for bucket in &facets.width {
        let b: f32 = bucket.value.parse().expect("a width bucket is a number");
        // The filter takes whole percents; every bucket that is not a half step round
        // trips, and 87.5 is asked for as the range it sits in.
        let (lo, hi) = (b.floor() as u16, b.ceil() as u16);
        let found = index
            .list(&FaceFilter {
                width: Some((lo, hi)),
                ..Default::default()
            })
            .unwrap();
        assert!(
            found.len() as i64 >= bucket.count,
            "the facet offers {} at width {b} and the filter returns {}",
            bucket.count,
            found.len()
        );
    }
    let by_value: std::collections::BTreeMap<&str, i64> = facets
        .width
        .iter()
        .map(|c| (c.value.as_str(), c.count))
        .collect();
    assert_eq!(by_value.get("75"), Some(&1), "{by_value:?}");
    assert_eq!(by_value.get("87.5"), Some(&1), "{by_value:?}");
    assert_eq!(by_value.get("100"), Some(&6), "{by_value:?}");
}

/// A summary says what it can be set to, and a static face says nothing extra.
#[test]
fn a_summary_carries_the_range_only_where_there_is_one() {
    let index = indexed();
    let faces = index.list(&FaceFilter::default()).unwrap();
    let bricolage = faces
        .iter()
        .find(|f| f.family == "Bricolage Grotesque")
        .expect("a variable fixture");
    assert_eq!(bricolage.weight_range, Some([200.0, 800.0]));
    assert_eq!(bricolage.width_range, Some([75.0, 100.0]));

    let amiri = faces.iter().find(|f| f.family == "Amiri").unwrap();
    assert_eq!(amiri.weight_range, None, "a static face is one weight");
    assert_eq!(amiri.width_range, None);
    assert!(
        !serde_json::to_string(amiri)
            .unwrap()
            .contains("weight_range"),
        "so a reader of a static face sees exactly the JSON it saw before"
    );

    // Nabla is variable, but on EDPT and EHLT — neither of which is wght or wdth.
    let nabla = faces.iter().find(|f| f.family == "Nabla").unwrap();
    assert!(nabla.variable);
    assert_eq!(
        nabla.weight_range, None,
        "variable is not the same as variable in weight"
    );

    // And a family reports the range its faces reach, not their default instances.
    let families = index.families(&FaceFilter::default()).unwrap();
    let fam = families
        .iter()
        .find(|f| f.name == "Bricolage Grotesque")
        .unwrap();
    assert_eq!(fam.weights, [200.0, 800.0]);
    assert_eq!(fam.widths, [75.0, 100.0]);
}

/// The same for width, over `wdth`.
#[test]
fn width_spans_its_axis_too() {
    let index = indexed();
    let faces = index
        .list(&FaceFilter {
            width: Some((75, 90)),
            ..Default::default()
        })
        .unwrap();
    let families: Vec<&str> = faces.iter().map(|f| f.family.as_str()).collect();
    assert_eq!(
        families,
        vec!["Bricolage Grotesque"],
        "only the face with a wdth axis reaches below 100%: {families:?}"
    );
}

/// A row whose stored metadata will not parse keeps the behaviour it had before v4.
///
/// The migration seeds the four columns from the static values before reading any JSON,
/// so a face the backfill has to skip is still findable at the weight it reports. Without
/// that seed the `DEFAULT 0` would give it a zero-width span and no weight filter could
/// ever match it again — a font quietly disappearing from the index that still lists it.
#[test]
fn a_face_whose_metadata_will_not_parse_keeps_its_static_weight() {
    let dir = std::env::temp_dir().join(format!("fontina-spans-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("v3.db");
    {
        let mut idx = Index::open(&db).unwrap();
        fontina_core::scan::scan(
            &mut idx,
            &[fixtures().join("Amiri-Regular.ttf")],
            &ScanOptions::default(),
        )
        .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE face_scripts;
             DROP TABLE face_languages;
             DROP INDEX faces_fixed_pitch;
             ALTER TABLE faces DROP COLUMN is_fixed_pitch;
             DROP INDEX faces_weight_span; DROP INDEX faces_width_span;
             ALTER TABLE faces DROP COLUMN weight_min;
             ALTER TABLE faces DROP COLUMN weight_max;
             ALTER TABLE faces DROP COLUMN width_min;
             ALTER TABLE faces DROP COLUMN width_max;
             PRAGMA user_version = 3;",
        )
        .unwrap();
        // Whatever wrote this row, this build cannot read it.
        conn.execute("UPDATE faces SET metadata = '{ not json'", [])
            .unwrap();
    }

    let idx = Index::open(&db).unwrap();
    let at_400 = idx
        .list(&FaceFilter {
            weight: Some((400, 400)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(at_400.len(), 1, "still found at the weight the row says");
    let at_700 = idx
        .list(&FaceFilter {
            weight: Some((700, 700)),
            ..Default::default()
        })
        .unwrap();
    assert!(at_700.is_empty(), "and not found anywhere else");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The columns are filled from the metadata already stored, so an index built by an
/// older fontina answers the new question without anyone rescanning their library.
#[test]
fn an_older_index_learns_the_ranges_without_a_rescan() {
    let dir = std::env::temp_dir().join(format!("fontina-spans-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("v3.db");
    {
        let mut idx = Index::open(&db).unwrap();
        fontina_core::scan::scan(
            &mut idx,
            &[fixtures().join("BricolageGrotesque[opsz,wdth,wght].ttf")],
            &ScanOptions::default(),
        )
        .unwrap();
    }
    {
        // Make the file look like a v3 index: the columns gone, the version rolled back.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE face_scripts;
             DROP TABLE face_languages;
             DROP INDEX faces_fixed_pitch;
             ALTER TABLE faces DROP COLUMN is_fixed_pitch;
             DROP INDEX faces_weight_span; DROP INDEX faces_width_span;
             ALTER TABLE faces DROP COLUMN weight_min;
             ALTER TABLE faces DROP COLUMN weight_max;
             ALTER TABLE faces DROP COLUMN width_min;
             ALTER TABLE faces DROP COLUMN width_max;
             PRAGMA user_version = 3;",
        )
        .unwrap();
    }

    let idx = Index::open(&db).unwrap();
    let found = idx
        .list(&FaceFilter {
            weight: Some((400, 400)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        found.len(),
        1,
        "the migration read the axes out of the stored metadata"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collections_keep_order_and_export_import() {
    let mut index = indexed();
    let serif = id_of(&index, "Source Serif 4");
    let amiri = id_of(&index, "Amiri");
    let nabla = id_of(&index, "Nabla");
    assert_eq!(
        index
            .add_to_collection("Editorial", &[serif, amiri])
            .unwrap(),
        2
    );
    assert_eq!(
        index
            .add_to_collection("editorial", &[nabla, amiri])
            .unwrap(),
        1
    );
    let faces = index.collection_faces("Editorial").unwrap();
    assert_eq!(
        faces.iter().map(|f| f.id).collect::<Vec<_>>(),
        [serif, amiri, nabla],
        "insertion order, not family order"
    );
    assert_eq!(index.collections().unwrap()[0].faces, 3);
    assert!(index.collection_faces("nope").is_err());
    let filtered = index
        .list(&FaceFilter {
            collection: Some("Editorial".into()),
            italic: Some(false),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(filtered.len(), 3);

    index.tag(&[amiri], "arabic").unwrap();
    let export = index.export_collection("Editorial").unwrap();
    assert_eq!(export.name, "Editorial");
    assert_eq!(export.schema_version, fontina_core::SCHEMA_VERSION);
    assert_eq!(export.faces.len(), 3);
    assert_eq!(export.faces[1].tags, ["arabic"]);
    assert!(export.exported_at.contains('T'), "{}", export.exported_at);
    let json = serde_json::to_string(&export).unwrap();
    let back: CollectionExport = serde_json::from_str(&json).unwrap();

    // Import into a fresh index whose paths differ: identity hashes still match.
    let mut other = indexed();
    let mut moved = back.clone();
    for f in &mut moved.faces {
        f.path = format!("/elsewhere/{}", f.path.rsplit('/').next().unwrap());
    }
    moved.faces.push(fontina_core::CollectionFace {
        family: "Ghost".into(),
        subfamily: "Regular".into(),
        postscript_name: Some("Ghost-Regular".into()),
        identity_hash: "0000".into(),
        blake3: "0000".into(),
        path: "/nowhere/Ghost.ttf".into(),
        index: 0,
        tags: vec![],
    });
    let report = other.import_collection(&moved, None, true).unwrap();
    assert_eq!(report.matched, 3);
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].family, "Ghost");
    assert_eq!(report.tags_applied, 1);
    let imported = other.collection_faces("Editorial").unwrap();
    assert_eq!(imported.len(), 3);
    assert_eq!(imported[1].family, "Amiri");
    assert_eq!(imported[1].tags, ["arabic"]);

    let renamed = other.import_collection(&back, Some("Copy"), false).unwrap();
    assert_eq!(renamed.collection, "Copy");
    assert_eq!(other.collections().unwrap().len(), 2);
    assert!(other.rename_collection("Copy", "Copy 2").unwrap());
    assert!(other.delete_collection("copy 2").unwrap());
    assert_eq!(
        other.remove_from_collection("Editorial", &[serif]).unwrap(),
        1
    );
    assert_eq!(other.collection_faces("Editorial").unwrap().len(), 2);

    let newer = CollectionExport {
        schema_version: fontina_core::SCHEMA_VERSION + 1,
        ..back
    };
    assert!(other.import_collection(&newer, None, false).is_err());
}

#[test]
fn sources_are_recorded_by_scan_and_managed() {
    let mut index = indexed();
    let root = std::fs::canonicalize(fixtures()).unwrap();
    let sources = index.sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, root.to_string_lossy());
    assert_eq!(sources[0].kind, SourceKind::User);
    assert!(
        sources[0].watch,
        "explicit directories are watched by default"
    );

    assert!(index.set_source_watch(&sources[0].path, false).unwrap());
    assert!(!index.sources().unwrap()[0].watch);
    let sys = index
        .add_source("/nonexistent/system/fonts", false, SourceKind::System)
        .unwrap();
    assert_eq!(sys.kind, SourceKind::System);
    assert_eq!(index.sources().unwrap().len(), 2);
    assert!(
        index
            .remove_source("/nonexistent/system/fonts", false)
            .unwrap()
    );
    assert!(
        !index
            .remove_source("/nonexistent/system/fonts", false)
            .unwrap()
    );

    // Purging a source drops its faces.
    assert!(index.remove_source(&sources[0].path, true).unwrap());
    assert_eq!(index.stats().unwrap().faces, 0);
}

#[test]
fn activation_state_filters_and_survives_rescan() {
    let mut index = indexed();
    let amiri = id_of(&index, "Amiri");
    let nabla = id_of(&index, "Nabla");
    index
        .set_activation(&[amiri], ActivationState::Session, None)
        .unwrap();
    index
        .set_activation(
            &[nabla],
            ActivationState::Installed,
            Some("/home/u/.local/share/fonts/fontina/Nabla.ttf"),
        )
        .unwrap();
    let active = index
        .list(&FaceFilter {
            active: Some(true),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0].activation, Some(ActivationState::Session));
    let inactive = index
        .list(&FaceFilter {
            active: Some(false),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(inactive.len(), 4);
    let installed = index
        .list(&FaceFilter {
            activation: Some(ActivationState::Installed),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(installed.len(), 1);
    let rec = index.activation(nabla).unwrap().unwrap();
    assert_eq!(rec.state, ActivationState::Installed);
    assert!(
        rec.installed_path
            .as_deref()
            .unwrap()
            .ends_with("Nabla.ttf")
    );
    assert_eq!(index.activations().unwrap().len(), 2);

    // A forced rescan replaces the rows; user data carries over by (path, face index).
    index.tag(&[amiri], "kept").unwrap();
    index.add_to_collection("Kept", &[amiri]).unwrap();
    let report = fontina_core::scan::scan(
        &mut index,
        &[fixtures()],
        &ScanOptions {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.parsed, 6);
    let amiri2 = id_of(&index, "Amiri");
    assert_ne!(amiri, amiri2, "rows were replaced");
    let s = index.summaries(&[amiri2]).unwrap().remove(0);
    assert_eq!(s.tags, ["kept"]);
    assert_eq!(s.activation, Some(ActivationState::Session));
    assert_eq!(index.collection_faces("Kept").unwrap()[0].id, amiri2);
    assert_eq!(index.activations().unwrap().len(), 2);
    assert_eq!(index.clear_activation(&[amiri2]).unwrap(), 1);
    assert_eq!(index.activations().unwrap().len(), 1);
    assert_eq!(index.file_faces(amiri2).unwrap(), [amiri2]);
}

#[test]
fn a_parse_failure_keeps_the_user_s_curation() {
    // A font rewritten in place, a truncated download, a file caught mid-copy by the
    // watcher: the parse fails, and the tags, collections and activation the user built
    // by hand must still be there when it parses again.
    let dir = std::env::temp_dir().join(format!("fontina-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // scan canonicalises its roots, and on macOS the temp dir is a symlink.
    let dir = std::fs::canonicalize(&dir).unwrap();
    let font = dir.join("Amiri-Regular.ttf");
    std::fs::copy(fixtures().join("Amiri-Regular.ttf"), &font).unwrap();
    let mut index = Index::open_in_memory().unwrap();
    fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    let id = id_of(&index, "Amiri");
    index.tag(&[id], "editorial").unwrap();
    index.add_to_collection("Books", &[id]).unwrap();
    index
        .set_activation(&[id], ActivationState::Session, None)
        .unwrap();

    // The file is replaced by something unparseable and rescanned.
    std::fs::write(&font, b"not a font at all").unwrap();
    let report = fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    assert_eq!(report.failed.len(), 1, "{report:?}");
    let stats = index.stats().unwrap();
    assert_eq!(stats.failed_files, 1);
    let s = index.summaries(&[id]).unwrap().remove(0);
    assert_eq!(s.tags, ["editorial"]);
    assert_eq!(s.activation, Some(ActivationState::Session));
    assert_eq!(index.collection_faces("Books").unwrap()[0].id, id);
    assert_eq!(index.activations().unwrap().len(), 1);

    // And when it parses again, the curation is still attached to the new rows.
    std::fs::copy(fixtures().join("Amiri-Regular.ttf"), &font).unwrap();
    fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    assert_eq!(index.stats().unwrap().failed_files, 0);
    let id2 = id_of(&index, "Amiri");
    let s = index.summaries(&[id2]).unwrap().remove(0);
    assert_eq!(s.tags, ["editorial"]);
    assert_eq!(s.activation, Some(ActivationState::Session));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pruning_only_forgets_files_that_are_really_gone() {
    let dir = std::env::temp_dir().join(format!("fontina-prune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // scan canonicalises its roots, and on macOS the temp dir is a symlink.
    let dir = std::fs::canonicalize(&dir).unwrap();
    for name in ["Amiri-Regular.ttf", "SourceSerif4-Regular.otf"] {
        std::fs::copy(fixtures().join(name), dir.join(name)).unwrap();
    }
    let mut index = Index::open_in_memory().unwrap();
    fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    assert_eq!(index.stats().unwrap().faces, 2);

    // One file really deleted, with a sibling left: pruned.
    std::fs::remove_file(dir.join("Amiri-Regular.ttf")).unwrap();
    assert_eq!(index.prune_missing(&dir.to_string_lossy()).unwrap(), 1);
    assert_eq!(index.stats().unwrap().faces, 1);

    // A root we cannot read is not a root whose files are gone.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&dir, perms).unwrap();
        let pruned = index.prune_missing(&dir.to_string_lossy()).unwrap();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dir, perms).unwrap();
        assert_eq!(pruned, 0, "an unreadable root must prune nothing");
        assert_eq!(index.stats().unwrap().faces, 1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unavailable_directory_is_not_an_empty_one() {
    // An unmounted share leaves the mount point behind as an empty directory, which looks
    // exactly like a directory whose fonts were deleted. Pruning every last file under a
    // root is refused; `remove_under` is how you say you meant it.
    let dir = std::env::temp_dir().join(format!("fontina-unmount-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // scan canonicalises its roots, and on macOS the temp dir is a symlink.
    let dir = std::fs::canonicalize(&dir).unwrap();
    for name in ["Amiri-Regular.ttf", "SourceSerif4-Regular.otf"] {
        std::fs::copy(fixtures().join(name), dir.join(name)).unwrap();
    }
    let mut index = Index::open_in_memory().unwrap();
    fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    let id = id_of(&index, "Amiri");
    index.tag(&[id], "kept").unwrap();

    for name in ["Amiri-Regular.ttf", "SourceSerif4-Regular.otf"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    assert_eq!(index.prune_missing(&dir.to_string_lossy()).unwrap(), 0);
    assert_eq!(index.stats().unwrap().faces, 2, "nothing was forgotten");
    assert_eq!(index.remove_under(&dir.to_string_lossy()).unwrap(), 2);
    assert_eq!(index.stats().unwrap().faces, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_writer_waits_instead_of_failing() {
    // `fontina watch` is meant to run as a user service while you tag something in the
    // browser and activate something else from a shell: three processes, one index. With
    // the default deferred transaction and no busy timeout, whichever writer arrives
    // second fails on the spot with "database is locked".
    let dir = std::env::temp_dir().join(format!("fontina-busy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = std::fs::canonicalize(&dir).unwrap().join("index.db");

    let mut first = Index::open(&db).unwrap();
    fontina_core::scan::scan(&mut first, &[fixtures()], &ScanOptions::default()).unwrap();

    let (holding, held) = std::sync::mpsc::channel();
    let db_for_thread = db.clone();
    let holder = std::thread::spawn(move || {
        let mut other = Index::open(&db_for_thread).unwrap();
        let tx = other.begin().unwrap();
        tx.execute("UPDATE files SET scanned_at = scanned_at", [])
            .unwrap();
        holding.send(()).unwrap();
        // Long enough that a writer which does not wait cannot possibly succeed.
        std::thread::sleep(std::time::Duration::from_millis(600));
        drop(tx);
    });

    held.recv().unwrap();
    let start = std::time::Instant::now();
    // A scan is the case that actually broke: it reads (has this file changed?) and then
    // writes, inside one transaction. A deferred transaction asks to upgrade its read
    // lock at that point, and SQLite refuses an upgrade instantly when another connection
    // is writing, without consulting the busy timeout, because waiting could deadlock.
    // Only taking the write lock up front makes the timeout apply.
    fontina_core::scan::scan(
        &mut first,
        &[fixtures()],
        &ScanOptions {
            force: true,
            ..Default::default()
        },
    )
    .expect("the second writer must wait, not fail");
    let waited = start.elapsed();
    holder.join().unwrap();
    // A forced rescan replaces the rows, so the face has a new id.
    let id = id_of(&first, "Amiri");
    first.tag(&[id], "waited").unwrap();

    assert!(
        waited >= std::time::Duration::from_millis(300),
        "it returned in {waited:?}, so it cannot have waited for the other writer"
    );
    let s = first.summaries(&[id]).unwrap().remove(0);
    assert_eq!(s.tags, ["waited"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conflicts_see_active_and_system_faces_only() {
    let mut index = indexed();
    let woff = index
        .list(&FaceFilter {
            container: Some("woff".into()),
            ..Default::default()
        })
        .unwrap()[0]
        .id;
    let woff2 = index
        .list(&FaceFilter {
            container: Some("woff2".into()),
            ..Default::default()
        })
        .unwrap()[0]
        .id;
    // Same PostScript name, but neither is active or in a system directory: no conflict.
    assert!(index.conflicts(woff, &[]).unwrap().is_empty());
    index
        .set_activation(&[woff2], ActivationState::User, None)
        .unwrap();
    let c = index.conflicts(woff, &[]).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].face.id, woff2);
    assert_eq!(c[0].reason, "same PostScript name, active (user)");
    index.clear_activation(&[woff2]).unwrap();
    // Treat the fixtures directory as a system font directory.
    let root = std::fs::canonicalize(fixtures())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let c = index.conflicts(woff, &[root]).unwrap();
    assert_eq!(c.len(), 1);
    assert!(c[0].reason.ends_with("present in a system font directory"));
    assert!(index.conflicts(99999, &[]).is_err());
}

/// Every fixture is OFL, so the whole index is free and the other three states are
/// empty. The filter and the facet must agree with `freedom::classify` on each face.
#[test]
fn freedom_filters_and_counts_agree() {
    let index = indexed();
    let all = index.list(&FaceFilter::default()).unwrap();
    assert_eq!(all.len(), 6);
    assert!(all.iter().all(|f| f.freedom == Freedom::Free), "{all:?}");

    let free = index
        .list(&FaceFilter {
            freedom: Some(Freedom::Free),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(free.len(), 6);
    for state in [Freedom::Nonfree, Freedom::Unknown, Freedom::Unstated] {
        let rows = index
            .list(&FaceFilter {
                freedom: Some(state),
                ..Default::default()
            })
            .unwrap();
        assert!(rows.is_empty(), "{state} matched {} face(s)", rows.len());
    }

    let facets = index.facets(&FaceFilter::default()).unwrap();
    assert_eq!(facets.freedom.len(), 1);
    assert_eq!(facets.freedom[0].value, "free");
    assert_eq!(facets.freedom[0].count, 6);
}

#[test]
fn facets_count_every_dimension() {
    let mut index = indexed();
    let amiri = id_of(&index, "Amiri");
    index.tag(&[amiri], "arabic").unwrap();
    index.add_to_collection("Editorial", &[amiri]).unwrap();
    index
        .set_activation(&[amiri], ActivationState::Session, None)
        .unwrap();
    let f = index.facets(&FaceFilter::default()).unwrap();
    assert_eq!(f.faces, 6);
    assert_eq!(f.families, 5);
    assert_eq!(f.variable, 2);
    assert_eq!(f.color, 1);
    let get = |v: &[fontina_core::index::FacetCount], k: &str| {
        v.iter().find(|c| c.value == k).map(|c| c.count)
    };
    // Six, not five: Bricolage spans wght 200-800 and so is offered at 400 like every
    // static Regular. This used to read `Some(5)`, counting it only at its default
    // instance while the filter beside it returned six — see
    // `every_weight_the_facet_offers_returns_what_it_promised`.
    assert_eq!(get(&f.weight, "400"), Some(6), "{:?}", f.weight);
    assert_eq!(
        get(&f.weight, "800"),
        Some(1),
        "Bricolage reaches ExtraBold, and nothing else does"
    );
    assert_eq!(get(&f.weight, "200"), Some(1), "{:?}", f.weight);
    assert_eq!(get(&f.width, "100"), Some(6), "{:?}", f.width);
    assert_eq!(get(&f.style, "upright"), Some(6));
    assert_eq!(get(&f.container, "ttf"), Some(3));
    assert_eq!(get(&f.container, "woff2"), Some(1));
    assert_eq!(get(&f.script, "Arab"), Some(1));
    assert!(get(&f.script, "Latn").unwrap() >= 5);
    assert_eq!(get(&f.license, "OFL-1.1"), Some(6));
    assert_eq!(get(&f.tag, "arabic"), Some(1));
    assert_eq!(get(&f.collection, "Editorial"), Some(1));
    assert_eq!(get(&f.activation, "session"), Some(1));
    assert_eq!(get(&f.activation, "none"), Some(5));
    assert_eq!(f.source.len(), 1);
    assert_eq!(f.source[0].count, 6);
    assert!(!f.vendor.is_empty());

    // Facets follow the filter.
    let arabic = index
        .facets(&FaceFilter {
            scripts: vec!["Arab".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(arabic.faces, 1);
    assert_eq!(get(&arabic.tag, "arabic"), Some(1));
    assert_eq!(get(&arabic.activation, "none"), None);
    assert_eq!(fontina_core::index::weight_name(700), "Bold");
    assert_eq!(fontina_core::index::weight_bucket(651.0), 700);
    assert_eq!(fontina_core::index::width_bucket(80.0), 75.0);
    assert_eq!(fontina_core::index::width_name(75.0), "Condensed");
}

#[test]
fn families_group_faces_and_pick_a_representative() {
    let mut index = indexed();
    let fams = index.families(&FaceFilter::default()).unwrap();
    assert_eq!(fams.len(), 5);
    let inter = fams.iter().find(|f| f.name == "Inter").unwrap();
    assert_eq!(inter.faces, 2, "woff and woff2 of the same face");
    assert_eq!(inter.containers, ["woff", "woff2"]);
    assert_eq!(inter.weights, [400.0, 400.0]);
    let bricolage = fams
        .iter()
        .find(|f| f.name == "Bricolage Grotesque")
        .unwrap();
    assert!(bricolage.variable);
    assert_eq!(bricolage.faces, 1);
    assert_eq!(bricolage.representative, bricolage.ids[0]);
    let nabla = fams.iter().find(|f| f.name == "Nabla").unwrap();
    assert!(nabla.color);
    index.tag(&nabla.ids, "fun").unwrap();
    let limited = index
        .families(&FaceFilter {
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(limited.len(), 2);
    let tagged = index
        .families(&FaceFilter {
            tag: Some("fun".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].tags, ["fun"]);
    let by_ids = index
        .list(&FaceFilter {
            ids: Some(inter.ids.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_ids.len(), 2);
    // Bricolage's `wdth` axis runs 75-100, so a face that reaches into 50-99 is found
    // there. This assertion used to read `is_empty`, which was the filter looking only at
    // the default instance; see `width_spans_its_axis_too`.
    let width = index
        .list(&FaceFilter {
            width: Some((50, 99)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        width.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(),
        vec!["Bricolage Grotesque"]
    );
    let narrower = index
        .list(&FaceFilter {
            width: Some((50, 74)),
            ..Default::default()
        })
        .unwrap();
    assert!(narrower.is_empty(), "below the axis is still below it");
    let vendor = index
        .list(&FaceFilter {
            vendor: by_ids[0].vendor.clone(),
            ..Default::default()
        })
        .unwrap();
    assert!(vendor.len() >= 2);
}

#[test]
fn schemas_cover_the_new_types() {
    let coll = fontina_core::collection_schema();
    assert_eq!(coll["title"], "CollectionExport");
    let cli = fontina_core::cli_output_schema();
    let defs = cli["$defs"].as_object().unwrap();
    for name in [
        "FaceSummary",
        "Family",
        "Facets",
        "DuplicateGroup",
        "Stats",
        "ScanReport",
        "CheckReport",
        "BlockCoverage",
        "TagInfo",
        "CollectionInfo",
        "CollectionExport",
        "ImportReport",
        "BundleReport",
        "TagSyncReport",
        "Related",
        "Source",
        "ActivationRecord",
        "Conflict",
        "ActivationState",
    ] {
        assert!(defs.contains_key(name), "missing {name}");
    }
}

#[test]
fn watch_applies_file_and_directory_changes() {
    use std::collections::BTreeSet;
    let dir = std::env::temp_dir().join(format!("fontina-watch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let mut index = Index::open_in_memory().unwrap();
    fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    let roots = vec![std::fs::canonicalize(&dir).unwrap()];
    let opts = fontina_core::watch::WatchOptions::default();

    // A new file is parsed on its own.
    let amiri = roots[0].join("Amiri-Regular.ttf");
    std::fs::copy(fixtures().join("Amiri-Regular.ttf"), &amiri).unwrap();
    let ev = fontina_core::watch::apply(&mut index, &roots, &opts, BTreeSet::from([amiri.clone()]))
        .unwrap();
    assert_eq!(ev.report.parsed, 1);
    assert_eq!(ev.paths, [amiri.to_string_lossy().into_owned()]);
    assert_eq!(index.stats().unwrap().faces, 1);

    // Non-font and unchanged paths are no-ops.
    let readme = roots[0].join("README.txt");
    std::fs::write(&readme, "hi").unwrap();
    let ev = fontina_core::watch::apply(
        &mut index,
        &roots,
        &opts,
        BTreeSet::from([readme, amiri.clone()]),
    )
    .unwrap();
    assert_eq!(ev.report.parsed, 0);
    assert_eq!(ev.report.unchanged, 1);

    // A directory event rescans it with pruning; a removed file is dropped.
    let sub = roots[0].join("sub");
    std::fs::copy(
        fixtures().join("SourceSerif4-Regular.otf"),
        sub.join("S.otf"),
    )
    .unwrap();
    std::fs::remove_file(&amiri).unwrap();
    let ev = fontina_core::watch::apply(
        &mut index,
        &roots,
        &opts,
        BTreeSet::from([sub.clone(), amiri.clone()]),
    )
    .unwrap();
    assert_eq!(ev.report.parsed, 1, "{ev:?}");
    assert_eq!(ev.report.removed, 1, "{ev:?}");
    assert_eq!(index.stats().unwrap().faces, 1);

    // A directory that vanished takes its files with it.
    std::fs::remove_dir_all(&sub).unwrap();
    let ev = fontina_core::watch::apply(&mut index, &roots, &opts, BTreeSet::from([sub])).unwrap();
    assert_eq!(ev.report.removed, 1, "{ev:?}");
    assert_eq!(index.stats().unwrap().faces, 0);

    // The live watcher delivers a batch for a copied file.
    let (tx, rx) = std::sync::mpsc::channel();
    let root_for_thread = roots[0].clone();
    let handle = std::thread::spawn(move || {
        let mut index = Index::open_in_memory().unwrap();
        fontina_core::watch::watch(
            &mut index,
            &[root_for_thread],
            &fontina_core::watch::WatchOptions {
                debounce: std::time::Duration::from_millis(200),
                ..Default::default()
            },
            |ev| {
                // FSEvents on macOS can replay events from just before the stream
                // started, so earlier batches may carry nothing new; keep going until
                // the copied file has been parsed.
                let parsed = ev.report.parsed;
                tx.send(parsed).unwrap();
                parsed == 0
            },
        )
        .unwrap();
    });
    // The watcher runs on its own thread and nothing says when its stream is registered,
    // so copy the file and, if no batch arrives within a couple of seconds, copy it
    // again: a slow start on a loaded runner then cannot lose the only event, and a
    // batch that parsed nothing (an FSEvents replay, or a file caught mid-copy) is just
    // waited past.
    let nabla = roots[0].join("Nabla.ttf");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        std::fs::copy(fixtures().join("Nabla[EDPT,EHLT].ttf"), &nabla).unwrap();
        let wait = std::time::Duration::from_secs(2)
            .min(deadline.saturating_duration_since(std::time::Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(parsed) if parsed >= 1 => break,
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                if std::time::Instant::now() < deadline =>
            {
                continue;
            }
            Err(e) => panic!("watcher never reported the new file: {e}"),
        }
    }
    handle.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn watch_removes_a_vanished_directory_whose_name_has_a_dot() {
    use std::collections::BTreeSet;
    let dir = std::env::temp_dir().join(format!("fontina-watch-dot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // A version in the directory's name gives it a file extension as far as
    // `Path::extension` is concerned. It is still a directory.
    std::fs::create_dir_all(dir.join("Inter v4.0")).unwrap();
    std::fs::copy(
        fixtures().join("Amiri-Regular.ttf"),
        dir.join("Inter v4.0").join("Amiri-Regular.ttf"),
    )
    .unwrap();
    let mut index = Index::open_in_memory().unwrap();
    fontina_core::scan::scan(
        &mut index,
        std::slice::from_ref(&dir),
        &ScanOptions::default(),
    )
    .unwrap();
    assert_eq!(index.stats().unwrap().faces, 1);

    let roots = vec![std::fs::canonicalize(&dir).unwrap()];
    let versioned = roots[0].join("Inter v4.0");
    let opts = fontina_core::watch::WatchOptions::default();
    std::fs::remove_dir_all(&versioned).unwrap();
    let ev = fontina_core::watch::apply(
        &mut index,
        &roots,
        &opts,
        BTreeSet::from([versioned.clone()]),
    )
    .unwrap();
    assert_eq!(ev.report.removed, 1, "{ev:?}");
    assert_eq!(index.stats().unwrap().faces, 0, "{ev:?}");
    assert!(
        ev.paths.contains(&versioned.to_string_lossy().into_owned()),
        "{ev:?}"
    );

    // A vanished plain file the index never knew about is still a no-op.
    let ev = fontina_core::watch::apply(
        &mut index,
        &roots,
        &opts,
        BTreeSet::from([roots[0].join("README.txt")]),
    )
    .unwrap();
    assert!(ev.paths.is_empty(), "{ev:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_shapes_and_rasterises() {
    use fontina_core::render::{RenderOptions, encode, render_face, shaped_glyphs};
    let (_, faces) = fontina_core::load_file(&fixtures().join("Amiri-Regular.ttf")).unwrap();
    let bytes = std::fs::read(fixtures().join("Amiri-Regular.ttf")).unwrap();
    // Shaping is real: Arabic letters come back as contextual forms, not the isolated
    // glyphs, and Latin ligatures collapse.
    let word = shaped_glyphs(&bytes, 0, "سلام").unwrap();
    assert_eq!(word.len(), 4);
    let isolated: Vec<u32> = "سلام"
        .chars()
        .map(|c| shaped_glyphs(&bytes, 0, &c.to_string()).unwrap()[0])
        .collect();
    assert_ne!(word, isolated);
    let serif = std::fs::read(fixtures().join("SourceSerif4-Regular.otf")).unwrap();
    assert_eq!(
        shaped_glyphs(&serif, 0, "fi").unwrap().len(),
        1,
        "fi ligature"
    );
    assert_eq!(shaped_glyphs(&serif, 0, "ab").unwrap().len(), 2);

    let bm = render_face(
        &faces[0],
        &RenderOptions {
            text: "سلام\nAmiri".into(),
            size: 32.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        bm.width > 40 && bm.height > 60,
        "{}x{}",
        bm.width,
        bm.height
    );
    assert!(!bm.is_blank());
    assert_eq!(bm.missing, 0);
    assert!(bm.glyphs >= 8);
    // Ink sits below the first baseline and above the second.
    assert!(bm.baseline > 20.0 && bm.baseline < bm.height as f32);

    let png = encode::png(&bm, [255, 255, 255], None);
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(&png[12..16], b"IHDR");
    assert!(png.ends_with(b"IEND\xaeB`\x82"));
    let opaque = encode::png(&bm, [0, 0, 0], Some([255, 255, 255]));
    assert!(opaque.len() > 100);

    let six = encode::sixel(&bm, [255, 255, 255], [0, 0, 0], 16);
    assert!(six.starts_with("\x1bP0;1;0q\"1;1;"));
    assert!(six.ends_with("\x1b\\"));
    assert!(six.contains('-'), "band separators");

    let blocks = encode::half_blocks(&bm, [255, 255, 255], [0, 0, 0]);
    assert_eq!(blocks.lines().count(), (bm.height as usize).div_ceil(2));
    assert!(blocks.contains('▀'));
    assert!(blocks.contains("\x1b[38;2;"));

    let k = encode::kitty(&png, false);
    assert!(k.starts_with("\x1b_Gf=100,a=T,t=d,q=2,m="));
    assert!(encode::kitty(&png, true).starts_with("\x1bPtmux;\x1b\x1b_G"));
    assert!(encode::iterm(&png, false).starts_with("\x1b]1337;File=inline=1;size="));
    assert_eq!(encode::parse_rgb("#1a2B3c"), Some([0x1a, 0x2b, 0x3c]));
    assert_eq!(encode::parse_rgb("nope"), None);

    // Variable axes and features are honoured.
    let (_, bric) =
        fontina_core::load_file(&fixtures().join("BricolageGrotesque[opsz,wdth,wght].ttf"))
            .unwrap();
    let light = render_face(
        &bric[0],
        &RenderOptions {
            text: "Bold".into(),
            variations: vec![("wght".into(), 200.0)],
            ..Default::default()
        },
    )
    .unwrap();
    let heavy = render_face(
        &bric[0],
        &RenderOptions {
            text: "Bold".into(),
            variations: vec![("wght".into(), 800.0)],
            ..Default::default()
        },
    )
    .unwrap();
    let ink = |b: &fontina_core::render::Bitmap| b.coverage.iter().map(|&c| c as u64).sum::<u64>();
    assert!(
        ink(&heavy) > ink(&light) * 3 / 2,
        "{} vs {}",
        ink(&heavy),
        ink(&light)
    );
    assert!(
        render_face(
            &bric[0],
            &RenderOptions {
                variations: vec![("weight".into(), 1.0)],
                ..Default::default()
            }
        )
        .is_err(),
        "bad tag is an error"
    );
    let woff = fontina_core::load_file(&fixtures().join("inter-latin-400-normal.woff2")).unwrap();
    let w = render_face(&woff.1[0], &RenderOptions::default()).unwrap();
    assert!(!w.is_blank(), "WOFF2 is unwrapped before rendering");
}
