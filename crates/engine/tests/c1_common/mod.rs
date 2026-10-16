#![allow(dead_code)]
//! The `fixtures/multipack/worlds/main/level.dat` generator and shared helpers.

use beacon_engine::nbt::{self, Tag};
use std::path::PathBuf;

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/multipack")
}

fn c(v: Vec<(&str, Tag)>) -> Tag {
    Tag::Compound(
        v.into_iter()
            .map(|(k, t)| (k.as_bytes().to_vec(), t))
            .collect(),
    )
}

fn ints(v: [i32; 5]) -> Tag {
    Tag::List(nbt::INT, v.map(Tag::Int).to_vec())
}

/// A minimal fresh Bedrock 1.21.50 flat creative world (fixed values: deterministic).
pub fn fixture_level() -> (i32, Tag) {
    let b = |v: i8| Tag::Byte(v);
    let layers = r#"{"biome_id":1,"block_layers":[{"block_name":"minecraft:bedrock","count":1},{"block_name":"minecraft:dirt","count":2},{"block_name":"minecraft:grass_block","count":1}],"encoding_version":6,"structure_options":null,"world_version":"version.post_1_18"}"#;
    let root = c(vec![
        ("StorageVersion", Tag::Int(10)),
        ("NetworkVersion", Tag::Int(766)),
        ("LevelName", Tag::string("Beacon Multipack")),
        ("GameType", Tag::Int(1)),
        ("Difficulty", Tag::Int(1)),
        ("Generator", Tag::Int(2)),
        ("RandomSeed", Tag::Long(1_234_567_890_123)),
        ("SpawnX", Tag::Int(0)),
        ("SpawnY", Tag::Int(4)),
        ("SpawnZ", Tag::Int(0)),
        ("LastPlayed", Tag::Long(1_760_000_000)),
        ("Time", Tag::Long(0)),
        ("currentTick", Tag::Long(0)),
        ("SizeOnDisk", Tag::Long(0)),
        ("ForceGameType", b(0)),
        ("commandsEnabled", b(1)),
        ("hasBeenLoadedInCreative", b(1)),
        ("educationFeaturesEnabled", b(0)),
        ("eduOffer", Tag::Int(0)),
        ("spawnMobs", b(1)),
        ("pvp", b(1)),
        ("lightningLevel", Tag::Float(0.0)),
        ("rainLevel", Tag::Float(0.0)),
        ("lightningTime", Tag::Int(100_000)),
        ("rainTime", Tag::Int(100_000)),
        ("FlatWorldLayers", Tag::string(layers)),
        ("InventoryVersion", Tag::string("1.21.50")),
        ("baseGameVersion", Tag::string("*")),
        ("MinimumCompatibleClientVersion", ints([1, 21, 50, 0, 0])),
        ("lastOpenedWithVersion", ints([1, 21, 50, 0, 0])),
        (
            "abilities",
            c(vec![
                ("flySpeed", Tag::Float(0.05)),
                ("walkSpeed", Tag::Float(0.1)),
                ("mayfly", b(0)),
                ("flying", b(0)),
                ("instabuild", b(0)),
                ("invulnerable", b(0)),
            ]),
        ),
        (
            "experiments",
            c(vec![
                ("experiments_ever_used", b(0)),
                ("saved_with_toggled_experiments", b(0)),
            ]),
        ),
        ("world_policies", c(vec![])),
    ]);
    (10, root)
}

pub fn fixture_bytes() -> Vec<u8> {
    let (v, t) = fixture_level();
    nbt::write_level_dat(v, &t)
}
