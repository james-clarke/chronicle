//! `docs/connectors.json` is what the site reads: Render has no cargo, so
//! the file is committed and this test is the thing that stops it drifting
//! from the registry (m41 chunk 0).

use std::path::PathBuf;

fn committed() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/connectors.json")
}

#[test]
fn committed_json_matches_the_registry() {
    let path = committed();
    let on_disk =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        on_disk,
        chronicle_core::connectors::to_json(),
        "docs/connectors.json is stale — regenerate it with `chronicle connections --json`"
    );
}
