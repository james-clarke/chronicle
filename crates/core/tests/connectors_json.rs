//! `docs/connectors.json` is what the site reads: Render has no cargo, so
//! the file is committed and this test is the thing that stops it drifting
//! from the registry (m41 chunk 0). The tools page's table is committed for
//! the same reason (chunk 5) and checked the same way.

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

#[test]
fn committed_tools_page_matches_the_registry() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../site/tools.html");
    let page = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (open, close) = ("<!-- tools -->\n", "<!-- /tools -->");
    let start = page
        .find(open)
        .expect("site/tools.html has a <!-- tools --> marker")
        + open.len();
    let end = page[start..]
        .find(close)
        .expect("site/tools.html has a <!-- /tools --> marker")
        + start;
    assert_eq!(
        &page[start..end],
        chronicle_core::connectors::to_site_html(),
        "site/tools.html is stale — regenerate it with `sh scripts/site-tools.sh`"
    );
}
