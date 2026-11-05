//! Guards the Dialogue static route table. `index.html` is served with every asset it
//! references, so a page that references a file with no route silently 404s in the
//! running program instead of failing a build. Factory uses the shared Tools Map
//! assets and must not request the retired room renderer.
use std::collections::BTreeSet;
use std::path::Path;

const INDEX: &str = include_str!("../web/index.html");
const SERVER: &str = include_str!("../src/server.rs");

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

/// The `include_str!` path the route for `asset` embeds, resolved from `src/`.
fn include_path_for(asset: &str) -> Option<String> {
    let marker = format!("\"/{asset}\"");
    let start = SERVER.find(&marker)?;
    let end = SERVER.len().min(start + 400);
    let window = &SERVER[start..end];
    let open = window.find("include_str!(\"")? + "include_str!(\"".len();
    let rest = &window[open..];
    let close = rest.find('"')?;
    Some(rest[..close].to_string())
}

#[test]
fn every_referenced_asset_has_a_static_route() {
    let assets = referenced_assets(INDEX);
    assert!(
        assets.contains("toolsmap.js") && assets.contains("toolsmap.css"),
        "index.html should load the Factory tools map: {assets:?}"
    );
    assert!(!assets.contains("factory.js"), "the old Factory renderer is retired");
    let missing: Vec<&String> = assets
        .iter()
        .filter(|asset| !SERVER.contains(&format!("\"/{asset}\"")))
        .collect();
    assert!(
        missing.is_empty(),
        "index.html requests assets with no route in server.rs: {missing:?}"
    );
}

#[test]
fn every_referenced_asset_exists_on_disk() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let assets = referenced_assets(INDEX);
    let mut missing = Vec::new();
    for asset in &assets {
        match include_path_for(asset) {
            Some(path) if src.join(&path).exists() => {}
            Some(path) => missing.push(format!("{asset} -> {path}")),
            None => missing.push(format!("{asset} -> no include_str! route")),
        }
    }
    assert!(missing.is_empty(), "embedded assets missing on disk: {missing:?}");
}
