//! API checks with no index installed at all: one Note, no API diagnostics, nothing cached or
//! created in the data dir. Own test binary: `BEACON_HOME` is process-wide.

mod b1_common;
use b1_common::*;
use beacon_engine::api_index::{resolve, shared};
use beacon_engine::diag::{Code, Severity};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

#[test]
fn missing_index_is_one_note() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test of this binary, set before the engine reads the environment
    unsafe { std::env::set_var("BEACON_HOME", home.path()) };
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "config.json",
        r#"{ "type": "minecraftBedrock", "name": "t", "targetVersion": "1.0.1", "namespace": "demo",
  "beacon": { "scriptModules": { "@minecraft/server": "2.6.0" } } }"#,
    );
    let fnf = "summon minecraft:zomblie
";
    write(d.path(), "src/m/functions/a.mcfunction", fnf);
    write(d.path(), "src/m/functions/b.mcfunction", fnf);
    write(
        d.path(),
        "src/m/scripts/main.ts",
        "import { Bogus } from '@minecraft/server';",
    );
    assert!(shared("1.0.1").is_err());
    assert!(resolve("1.0.1").is_err());
    let o = beacon_engine::build::BuildOptions {
        all: true,
        ..opts(d.path(), None)
    };
    let runs = Arc::new(AtomicUsize::new(0));
    let o = session(o, d.path(), &runs).check();
    let n: Vec<_> = o
        .diagnostics
        .iter()
        .filter(|x| x.code == Code::ApiIndexMissing)
        .collect();
    assert_eq!(n.len(), 1, "{:?}", o.diagnostics);
    assert_eq!(n[0].severity, Severity::Note);
    for c in [
        Code::ApiUnknownVanillaId,
        Code::ApiUnknownScriptMember,
        Code::ApiScriptModuleVersion,
        Code::ApiUnknownComponent,
    ] {
        assert!(o.diagnostics.iter().all(|x| x.code != c), "{c:?}");
    }
    // a miss is not cached, and nothing was created in the data dir
    assert!(shared("1.0.1").is_err());
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
