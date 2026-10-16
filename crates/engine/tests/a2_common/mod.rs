#![allow(dead_code)]
use beacon_engine::config::ProjectConfig;
use beacon_engine::path::{Hash, RelPath, Vfs};
use std::collections::HashMap;

#[derive(Default)]
pub struct FakeVfs(pub HashMap<String, Vec<u8>>);

impl FakeVfs {
    pub fn with(files: &[(&str, &[u8])]) -> Self {
        FakeVfs(
            files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_vec()))
                .collect(),
        )
    }
}

impl Vfs for FakeVfs {
    fn exists(&self, p: &RelPath) -> bool {
        self.0.contains_key(p.as_str())
    }
    fn read(&self, p: &RelPath) -> std::io::Result<Vec<u8>> {
        self.0
            .get(p.as_str())
            .cloned()
            .ok_or(std::io::ErrorKind::NotFound.into())
    }
    fn hash(&self, p: &RelPath) -> std::io::Result<Hash> {
        Ok(*blake3::hash(&self.read(p)?).as_bytes())
    }
    fn list(&self, _dir: Option<&RelPath>) -> Vec<String> {
        Vec::new()
    }
    fn stream(&self, p: &RelPath, out: &mut dyn std::io::Write) -> std::io::Result<u64> {
        let b = self.read(p)?;
        out.write_all(&b)?;
        Ok(b.len() as u64)
    }
}

pub fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

pub fn cfg(beacon: &str) -> ProjectConfig {
    serde_json::from_str(&format!(
        r#"{{"type":"minecraftBedrock","name":"My Pack","targetVersion":"1.21.100","namespace":"demo","beacon":{beacon}}}"#
    ))
    .unwrap()
}
