//! Phase 3 A3: `equiv::compare` normalization and every `Diff` kind.

use beacon_engine::migrate::equiv::{Diff, compare};
use beacon_engine::nbt::{self, Tag};
use std::fs;
use std::path::Path;

fn tree(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, b) in files {
        let f = d.path().join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, b).unwrap();
    }
    d
}

fn p(s: &str) -> String {
    s.to_string()
}

#[test]
fn equal_modulo_normalization() {
    let a = tree(&[
        ("RP/texts/en_US.lang", b"a=1\t## c\nb=2\na=3  \n## x\n\n"),
        ("BP/x.json", br#"{"a":1,"b":[1,2]}"#),
        ("BP/f.mcfunction", b"say hi  \r\nsay 2\r\n"),
        ("BP/contents.json", b"1"),
        ("BP/.beacon-emit.json", b"1"),
        ("RP/texts/languages.json", br#"["en_US","de_DE"]"#),
    ]);
    let b = tree(&[
        ("RP/texts/en_US.lang", b"b=2\r\na=3\r\n"),
        ("BP/x.json", b"// c\n{\"b\":[1,2],\"a\":1}"),
        ("BP/f.mcfunction", b"say hi\nsay 2\n"),
        ("RP/texts/languages.json", br#"["de_DE","en_US"]"#),
    ]);
    let r = compare(a.path(), b.path(), &[]);
    assert_eq!(r.diffs, []);
    assert_eq!((r.compared, r.ignored), (4, 2));
}

#[test]
fn every_diff_kind() {
    let a = tree(&[
        ("BP/only_a.txt", b"1"),
        ("BP/j.json", br#"{"a":1,"k":[1,2]}"#),
        ("RP/l.lang", b"a=1\nb=2\n"),
        ("BP/t.ts", b"x\ny\n"),
        ("RP/p.png", b"\0a"),
        ("BP/scripts/main.js", b"// src/a.ts\n1\n// src/b.ts\n2\n"),
        ("RP/textures/textures_list.json", br#"["a","b"]"#),
        ("BP/skip.txt", b"1"),
    ]);
    let b = tree(&[
        ("BP/only_b.txt", b"1"),
        ("BP/j.json", br#"{"a":2,"k":[1]}"#),
        ("RP/l.lang", b"a=1\nc=3\n"),
        ("BP/t.ts", b"x\nz\n"),
        ("RP/p.png", b"\0b"),
        ("BP/scripts/main.js", b"// ../src/b.ts\n2\n// src/a.ts\n1\n"),
        ("RP/textures/textures_list.json", br#"["b","c"]"#),
        ("BP/skip.txt", b"2"),
    ]);
    let ig = ["BP/skip.txt".to_string()];
    let r = compare(a.path(), b.path(), &ig);
    assert_eq!(r.ignored, 1);
    let want = [
        Diff::Missing {
            path: p("BP/only_a.txt"),
        },
        Diff::Extra {
            path: p("BP/only_b.txt"),
        },
        Diff::Json {
            path: p("BP/j.json"),
            pointer: p("/a"),
            left: Some(1.into()),
            right: Some(2.into()),
        },
        Diff::Json {
            path: p("BP/j.json"),
            pointer: p("/k/1"),
            left: Some(2.into()),
            right: None,
        },
        Diff::Lang {
            path: p("RP/l.lang"),
            key: p("b"),
            left: Some(p("2")),
            right: None,
        },
        Diff::Lang {
            path: p("RP/l.lang"),
            key: p("c"),
            left: None,
            right: Some(p("3")),
        },
        Diff::Text {
            path: p("BP/t.ts"),
            line: 2,
        },
        Diff::Binary {
            path: p("RP/p.png"),
        },
        Diff::Bundle {
            path: p("BP/scripts/main.js"),
            left: p("src/a.ts"),
            right: p("src/b.ts"),
        },
        Diff::Json {
            path: p("RP/textures/textures_list.json"),
            pointer: p("/0"),
            left: Some("a".into()),
            right: None,
        },
        Diff::Json {
            path: p("RP/textures/textures_list.json"),
            pointer: p("/1"),
            left: None,
            right: Some("c".into()),
        },
    ];
    for d in &want {
        assert!(r.diffs.contains(d), "missing {d:?}");
    }
    assert_eq!(r.diffs.len(), want.len());
    // deterministic, sorted by path
    assert_eq!(r, compare(a.path(), b.path(), &ig));
    let paths: Vec<String> = r.diffs.iter().map(|d| format!("{d:?}")).collect();
    assert!(
        paths[0].contains("only_a") || paths[0].contains("j.json") || paths[0].contains("only_b")
    );
}

#[test]
fn bundle_byte_diff_alone_is_not_a_diff() {
    let a = tree(&[("BP/scripts/main.js", b"// src/a.ts\nvar x = 1;\n")]);
    let b = tree(&[(
        "BP/scripts/main.js",
        b"// ./src/a.ts\nvar y = 2;\n//# sourceMappingURL=x\n",
    )]);
    assert_eq!(compare(a.path(), b.path(), &[]).diffs, []);
}

#[test]
fn level_dat_ignores_volatile_tags() {
    let lvl = |t: i64, name: &str| {
        let root = Tag::Compound(vec![
            (b"LastPlayed".to_vec(), Tag::Long(t)),
            (b"LevelName".to_vec(), Tag::string(name)),
        ]);
        nbt::write_level_dat(10, &root)
    };
    let a = tree(&[("level.dat", &lvl(1, "x"))]);
    let b = tree(&[("level.dat", &lvl(2, "x"))]);
    assert_eq!(compare(a.path(), b.path(), &[]).diffs, []);
    let c = tree(&[("level.dat", &lvl(2, "y"))]);
    assert_eq!(
        compare(a.path(), c.path(), &[]).diffs,
        [Diff::Nbt {
            path: p("level.dat"),
            key: p("LevelName")
        }]
    );
}

#[test]
fn expected_fixture_compares_equal_to_itself() {
    let d =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/migrate/modular_mc.expected");
    let r = compare(&d, &d, &[]);
    assert!(r.diffs.is_empty() && r.compared > 3);
}

#[test]
fn ignore_globs_do_not_cross_separators() {
    let a = tree(&[
        ("BP/manifest.json", b"1"),
        ("BP/a/manifest.json", b"1"),
        ("BP/a/b/contents.json", b"1"),
        ("RP/.beacon-emit.json", b"1"),
    ]);
    let b = tree(&[
        ("BP/manifest.json", b"2"),
        ("BP/a/manifest.json", b"2"),
        ("BP/a/b/contents.json", b"2"),
        ("RP/.beacon-emit.json", b"2"),
    ]);
    let r = compare(a.path(), b.path(), &["*/manifest.json".to_string()]);
    assert_eq!(r.ignored, 3); // BP/manifest.json + both defaults
    assert_eq!(r.diffs.len(), 1);
    assert!(matches!(&r.diffs[0], Diff::Json { path, .. } if path == "BP/a/manifest.json"));
}

#[test]
fn invalid_glob_is_reported_and_ignores_nothing() {
    let a = tree(&[("BP/x.txt", b"1")]);
    let b = tree(&[("BP/x.txt", b"2")]);
    let r = compare(a.path(), b.path(), &["[".to_string()]);
    assert_eq!(r.invalid_ignores, ["["]);
    assert_eq!(r.diffs.len(), 1);
}

#[test]
fn unreadable_root_is_reported_not_empty() {
    let a = tree(&[("BP/x.txt", b"1")]);
    let gone = a.path().join("nope");
    let r = compare(a.path(), &gone, &[]);
    assert!(r.diffs.contains(&Diff::Unreadable {
        path: ".".into(),
        side: "built".into()
    }));
    assert!(r.diffs.contains(&Diff::Missing {
        path: "BP/x.txt".into()
    }));
}

#[test]
fn headerless_scripts_compare_as_text() {
    let a = tree(&[("BP/scripts/plain.js", b"var a = 1;\n")]);
    let b = tree(&[("BP/scripts/plain.js", b"var a = 2;\n")]);
    assert_eq!(
        compare(a.path(), b.path(), &[]).diffs,
        [Diff::Text {
            path: "BP/scripts/plain.js".into(),
            line: 1
        }]
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_reported() {
    let a = tree(&[("BP/x.txt", b"1")]);
    let b = tree(&[("BP/y.txt", b"1")]);
    std::os::unix::fs::symlink(b.path().join("BP/y.txt"), b.path().join("BP/x.txt")).unwrap();
    let r = compare(a.path(), b.path(), &[]);
    assert!(r.diffs.contains(&Diff::Unreadable {
        path: "BP/x.txt".into(),
        side: "built".into()
    }));
}
