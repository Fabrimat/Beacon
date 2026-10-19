//! Index builder over small in-test tarballs (no network).

use beacon_api::{Tarball, build, to_json};
use beacon_engine::api_index::ApiKind;
use beacon_engine::diag::Code;
use flate2::{Compression, write::GzEncoder};

fn tgz(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut b = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, format!("package/{name}"), data.as_slice())
            .unwrap();
    }
    b.into_inner().unwrap().finish().unwrap()
}

fn fx(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn manifest(pkg: &str, ver: &str, license: &str) -> Vec<u8> {
    format!(r#"{{"name":"{pkg}","version":"{ver}","license":"{license}"}}"#).into_bytes()
}

fn hash(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}

struct Pkgs {
    server_stable: Vec<u8>,
    server_beta: Vec<u8>,
    vanilla: Vec<u8>,
    schemas: Vec<u8>,
}

fn pkgs() -> Pkgs {
    let server = |ver: &str| {
        tgz(&[
            ("package.json", manifest("@minecraft/server", ver, "MIT")),
            ("index.d.ts", fx("server.d.ts")),
            ("README.md", b"ignored".to_vec()),
        ])
    };
    Pkgs {
        server_stable: server("2.10.0"),
        server_beta: server("2.11.0-beta.1.26.50-stable"),
        vanilla: tgz(&[
            (
                "package.json",
                manifest("@minecraft/vanilla-data", "1.26.50", "MIT"),
            ),
            ("lib/mojang-entity.d.ts", fx("mojang-entity.d.ts")),
            (
                "lib/mojang-cameraPresets.d.ts",
                fx("mojang-cameraPresets.d.ts"),
            ),
            ("lib/index.js", b"ignored".to_vec()),
        ]),
        schemas: tgz(&[
            (
                "package.json",
                manifest("@minecraft/bedrock-schemas", "1.26.50", "MIT"),
            ),
            (
                "schemas/bp/entities/entity_entity_component_definitions.schema.json",
                fx("entity-components.schema.json"),
            ),
            (
                "types/bp/items/components/MinecraftAllowOffHand.d.ts",
                fx("MinecraftAllowOffHand.d.ts"),
            ),
        ]),
    }
}

fn index_json(p: &Pkgs) -> Vec<u8> {
    let (hs, hb, hv, hc) = (
        hash(&p.server_stable),
        hash(&p.server_beta),
        hash(&p.vanilla),
        hash(&p.schemas),
    );
    let list: Vec<Tarball> = vec![
        (
            "@minecraft/server",
            "2.11.0-beta.1.26.50-stable",
            &hb,
            &p.server_beta,
        ),
        ("@minecraft/server", "2.10.0", &hs, &p.server_stable),
        ("@minecraft/vanilla-data", "1.26.50", &hv, &p.vanilla),
        ("@minecraft/bedrock-schemas", "1.26.50", &hc, &p.schemas),
    ];
    to_json(&build("1.26.50", &list).unwrap())
}

#[test]
fn builds_and_looks_up_export_vanilla_id_and_component() {
    let p = pkgs();
    let (hs, hb, hv, hc) = (
        hash(&p.server_stable),
        hash(&p.server_beta),
        hash(&p.vanilla),
        hash(&p.schemas),
    );
    let ix = build(
        "1.26.50",
        &[
            ("@minecraft/server", "2.10.0", &hs, &p.server_stable),
            (
                "@minecraft/server",
                "2.11.0-beta.1.26.50-stable",
                &hb,
                &p.server_beta,
            ),
            ("@minecraft/vanilla-data", "1.26.50", &hv, &p.vanilla),
            ("@minecraft/bedrock-schemas", "1.26.50", &hc, &p.schemas),
        ],
    )
    .unwrap();

    // two module versions, stable before beta, manifest-style version strings
    let mods = &ix.script_modules["@minecraft/server"];
    assert_eq!(
        mods.iter()
            .map(|m| (m.version.as_str(), m.beta))
            .collect::<Vec<_>>(),
        [("2.10.0", false), ("2.11.0-beta", true)]
    );
    let world = &mods[0].exports["World"];
    assert_eq!(world.kind, ApiKind::Class);
    assert_eq!(world.doc.as_deref(), Some("A world."));
    assert_eq!(
        world.members["getDimension"],
        "getDimension(id: string): Dimension" // overload: first wins
    );
    assert_eq!(world.members["name"], "readonly name: string");
    assert!(!world.members.contains_key("constructor"));
    assert_eq!(mods[0].exports["GameMode"].kind, ApiKind::Enum);
    assert_eq!(mods[0].exports["Vec3"].members.len(), 3);
    let old = &mods[0].exports["oldThing"];
    assert!((old.kind, old.deprecated) == (ApiKind::Function, true));
    assert_eq!(mods[0].exports["world"].kind, ApiKind::Const);
    assert_eq!(mods[0].exports["Id"].kind, ApiKind::Type);

    assert!(ix.vanilla["entity"].contains("minecraft:zombie"));
    assert!(ix.vanilla["camera_presets"].contains("minecraft:free"));
    assert_eq!(ix.components["entity"].len(), 2);
    assert_eq!(
        ix.components["entity"]["minecraft:health"]
            .description
            .as_deref(),
        Some("Hit points.")
    );
    let item = &ix.components["item"]["minecraft:allow_off_hand"];
    assert!(
        item.description
            .as_deref()
            .unwrap()
            .starts_with("The allow_off_hand component")
    );

    // provenance, sorted, license recorded
    assert_eq!(ix.sources.len(), 4);
    assert!(ix.sources.iter().all(|s| s.license == "MIT"));
    assert_eq!(ix.sources[0].package, "@minecraft/bedrock-schemas");

    let hit = |q: &str| ix.lookup(q, 10);
    assert_eq!(
        hit("World.getDimension")[0].path,
        "@minecraft/server@2.10.0#World.getDimension"
    );
    assert_eq!(
        hit("minecraft:zombie")[0].path,
        "vanilla:entity#minecraft:zombie"
    );
    assert_eq!(
        hit("minecraft:health")[0].path,
        "component:entity#minecraft:health"
    );
    assert_eq!(
        hit("minecraft:allow_off")[0].path,
        "component:item#minecraft:allow_off_hand"
    );
    assert!(hit("oldthing").iter().any(|h| h.deprecated));
    assert!(hit("no-such-thing").is_empty());
    assert_eq!(ix.lookup("minecraft:", 3).len(), 3);
    // exact beats prefix
    assert_eq!(hit("World")[0].path, "@minecraft/server@2.10.0#World");
}

#[test]
fn index_is_byte_identical_across_builds() {
    let p = pkgs();
    assert_eq!(index_json(&p), index_json(&p));
}

#[test]
fn hash_mismatch_is_refused() {
    let p = pkgs();
    let wrong = hash(b"something else");
    let e = build(
        "1.26.50",
        &[("@minecraft/server", "2.10.0", &wrong, &p.server_stable)],
    )
    .unwrap_err();
    assert_eq!(e.code, Code::DownloadHashMismatch);
    assert!(e.message.contains("hash mismatch"), "{}", e.message);
}

#[test]
fn non_mit_license_is_refused() {
    let t = tgz(&[
        (
            "package.json",
            manifest("@minecraft/server", "2.10.0", "Apache-2.0"),
        ),
        ("index.d.ts", fx("server.d.ts")),
    ]);
    let e = build("1.26.50", &[("@minecraft/server", "2.10.0", &hash(&t), &t)]).unwrap_err();
    assert_eq!(e.code, Code::DownloadFailed);
    assert!(e.message.contains("only MIT"), "{}", e.message);
    // no license field at all
    let t = tgz(&[
        (
            "package.json",
            br#"{"name":"@minecraft/server","version":"2.10.0"}"#.to_vec(),
        ),
        ("index.d.ts", fx("server.d.ts")),
    ]);
    assert!(build("1.26.50", &[("@minecraft/server", "2.10.0", &hash(&t), &t)]).is_err());
}

#[test]
fn wrong_package_name_and_unsafe_members_are_refused() {
    let t = tgz(&[
        ("package.json", manifest("@evil/server", "2.10.0", "MIT")),
        ("index.d.ts", fx("server.d.ts")),
    ]);
    assert!(build("1.26.50", &[("@minecraft/server", "2.10.0", &hash(&t), &t)]).is_err());

    // a `..` member (raw header: the Builder API refuses to write one)
    let mut b = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut h = tar::Header::new_gnu();
    h.set_size(1);
    h.as_old_mut().name[..16].copy_from_slice(b"package/../evil\0");
    h.set_cksum();
    b.append(&h, &b"x"[..]).unwrap();
    let t = b.into_inner().unwrap().finish().unwrap();
    let e = build("1.26.50", &[("@minecraft/server", "2.10.0", &hash(&t), &t)]).unwrap_err();
    assert!(e.message.contains("unsafe"), "{}", e.message);
}

#[test]
fn a_syntax_error_in_a_d_ts_fails_the_build() {
    let t = tgz(&[
        (
            "package.json",
            manifest("@minecraft/server", "2.10.0", "MIT"),
        ),
        ("index.d.ts", b"export class {{{".to_vec()),
    ]);
    assert!(build("1.26.50", &[("@minecraft/server", "2.10.0", &hash(&t), &t)]).is_err());
}
