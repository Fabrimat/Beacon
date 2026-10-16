use beacon_engine::trust::{is_trusted, trust};
use serde_json::json;

// Single test: BEACON_HOME is process-global.
#[test]
fn round_trip_and_invalidation() {
    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary; no other thread touches the environment.
    unsafe { std::env::set_var("BEACON_HOME", home.path()) };
    let p = [json!("a")];
    assert!(!is_trusted(proj.path(), &p));
    trust(proj.path(), &p).unwrap();
    assert!(is_trusted(proj.path(), &p));
    assert!(!is_trusted(proj.path(), &[json!("a"), json!("b")]));
    assert!(!is_trusted(home.path(), &p));
    assert!(home.path().join("trust.json").is_file());
    if cfg!(windows) {
        let up = proj.path().to_string_lossy().to_uppercase();
        assert!(is_trusted(std::path::Path::new(&up), &p));
    }
}
