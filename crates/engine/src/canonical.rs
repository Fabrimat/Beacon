//! Workstream A2: canonical JSON writer, `canonicalVersion` 1 (5.2): sorted keys, 2-space
//! indent, `\n` line endings, shortest round-trip numbers, minimal escaping, trailing newline.

pub const CANONICAL_VERSION: u32 = 1;

pub fn to_vec(_v: &serde_json::Value) -> Vec<u8> {
    todo!("A2")
}
