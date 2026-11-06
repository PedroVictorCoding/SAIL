//! Guards the Dialogue static route table (`ui_assets::ASSETS`). `index.html` is served with every asset it
//! references, so a page that references a file with no route silently 404s in the
//! running program instead of failing a build. Factory uses the shared Tools Map
//! assets and must not request the retired room renderer.
use std::collections::BTreeSet;
use std::path::Path;

const INDEX: &str = include_str!("../web/index.html");
use sail_dialogue::ui_assets::ASSETS;

/// Every `src="…"` / `href="…"` in index.html that points at a script or stylesheet.
fn referenced_assets(html: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for attribute in ["src=\"", "href=\""] {
        let mut rest = html;
        while let Some(start) = rest.find(attribute) {
            rest = &rest[start + attribute.len()..];
            let Some(end) = rest.find('"') else { break };
            let value = &rest[..end];
            rest = &rest[end..];
            let path = value.split('#').next().unwrap_or(value);
            let is_asset = path.starts_with('/')
                && (path.ends_with(".js") || path.ends_with(".css") || path.ends_with(".svg"));
            if is_asset {
                found.insert(path.trim_start_matches('/').to_string());
            }
        }
    }
    found
}

#[test]
fn every_referenced_asset_has_a_static_route() {
    let assets = referenced_assets(INDEX);
    assert!(
        assets.contains("toolsmap.js") && assets.contains("toolsmap.css"),
        "index.html should load the Factory tools map: {assets:?}"
    );
    assert!(
        !assets.contains("factory.js"),
        "the old Factory renderer is retired"
    );
    let missing: Vec<&String> = assets
        .iter()
        .filter(|asset| !ASSETS.iter().any(|a| a.url == format!("/{asset}")))
        .collect();
    assert!(
        missing.is_empty(),
        "index.html requests assets with no route in ui_assets.rs: {missing:?}"
    );
}

#[test]
fn every_served_file_exists_on_disk_for_hot_reload() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing: Vec<_> = ASSETS
        .iter()
        .filter(|a| !crate_dir.join(a.file).is_file())
        .map(|a| a.file)
        .collect();
    assert!(
        missing.is_empty(),
        "served files missing on disk: {missing:?}"
    );
}
