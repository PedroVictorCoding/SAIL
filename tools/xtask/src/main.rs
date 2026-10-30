use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

#[derive(Debug)]
struct Asset {
    id: String,
    kind: String,
    source: String,
    outputs: Vec<String>,
    provenance: String,
    license: String,
    dark: bool,
    light: bool,
    fallback: String,
    max_bytes: u64,
}

fn sha256(path: &Path) -> Result<String, String> {
    let output = if cfg!(target_os = "macos") {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
    } else {
        Command::new("sha256sum").arg(path).output()
    }
    .map_err(|error| format!("could not run system SHA-256 tool: {error}"))?;
    if !output.status.success() {
        return Err(format!("SHA-256 failed for {}", path.display()));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| error.to_string())?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "SHA-256 output was empty".to_owned())
}

fn parse_manifest(path: &Path) -> Result<Vec<Asset>, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut assets = Vec::new();
    let mut current = None::<Asset>;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((_, version)) = line.split_once('=') {
            if line.starts_with("version ") && version.trim() == "1" {
                continue;
            }
            if line.starts_with("version ") {
                return Err("unsupported asset manifest version".into());
            }
        }
        if line == "[[asset]]" {
            if let Some(asset) = current.take() {
                assets.push(asset);
            }
            current = Some(Asset {
                id: String::new(),
                kind: String::new(),
                source: String::new(),
                outputs: Vec::new(),
                provenance: String::new(),
                license: String::new(),
                dark: false,
                light: false,
                fallback: String::new(),
                max_bytes: 0,
            });
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("invalid manifest line: {line}"))?;
        let value = value.trim().trim_matches('"');
        let asset = current
            .as_mut()
            .ok_or_else(|| "asset field before [[asset]]".to_owned())?;
        match key.trim() {
            "id" => asset.id = value.into(),
            "type" => asset.kind = value.into(),
            "source" => asset.source = value.into(),
            "outputs" => {
                asset.outputs = value
                    .trim_matches(['[', ']'])
                    .split(',')
                    .map(|item| item.trim().trim_matches('"').to_owned())
                    .filter(|item| !item.is_empty())
                    .collect()
            }
            "provenance" => asset.provenance = value.into(),
            "license" => asset.license = value.into(),
            "dark" => asset.dark = value == "true",
            "light" => asset.light = value == "true",
            "fallback" => asset.fallback = value.into(),
            "max_bytes" => asset.max_bytes = value.parse().map_err(|_| "invalid max_bytes")?,
            other => return Err(format!("unknown asset field: {other}")),
        }
    }
    if let Some(asset) = current {
        assets.push(asset);
    }
    Ok(assets)
}

fn safe_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(relative);
    if path.is_absolute() || path.components().any(|part| part.as_os_str() == "..") {
        return Err(format!("asset path must stay in repository: {relative}"));
    }
    Ok(root.join(path))
}

fn materialize(root: &Path) -> Result<(), String> {
    for asset in parse_manifest(&root.join("assets/manifest.toml"))? {
        let source = safe_path(root, &asset.source)?;
        for output in asset
            .outputs
            .iter()
            .filter(|item| !item.starts_with("runtime:"))
        {
            let destination = safe_path(root, output)?;
            fs::create_dir_all(
                destination
                    .parent()
                    .ok_or_else(|| format!("invalid output path: {output}"))?,
            )
            .map_err(|error| error.to_string())?;
            fs::copy(&source, &destination)
                .map_err(|error| format!("copy {}: {error}", asset.id))?;
        }
    }
    Ok(())
}

fn render_index(root: &Path) -> Result<String, String> {
    let assets = parse_manifest(&root.join("assets/manifest.toml"))?;
    if assets.is_empty() {
        return Err("manifest contains no assets".into());
    }
    let mut ids = HashSet::new();
    let mut rows = Vec::new();
    for asset in assets {
        if asset.id.is_empty() || !ids.insert(asset.id.clone()) {
            return Err(format!("empty or duplicate asset id: {}", asset.id));
        }
        if !matches!(
            asset.kind.as_str(),
            "procedural" | "svg" | "raster" | "reference"
        ) {
            return Err(format!("unknown asset type for {}", asset.id));
        }
        if asset.license.is_empty()
            || asset.provenance.is_empty()
            || asset.fallback.is_empty()
            || asset.max_bytes == 0
            || asset.outputs.is_empty()
            || (!asset.dark && !asset.light)
        {
            return Err(format!("incomplete metadata for {}", asset.id));
        }
        let source = safe_path(root, &asset.source)?;
        let metadata =
            fs::metadata(&source).map_err(|error| format!("{}: {error}", asset.source))?;
        if metadata.len() > asset.max_bytes {
            return Err(format!(
                "{} exceeds its {} byte budget",
                asset.id, asset.max_bytes
            ));
        }
        if asset.kind == "svg"
            && !fs::read_to_string(&source)
                .map_err(|error| error.to_string())?
                .contains("<svg")
        {
            return Err(format!("{} is not SVG", asset.id));
        }
        let mut output_rows = Vec::new();
        for output in &asset.outputs {
            if output.starts_with("runtime:") {
                output_rows.push(format!("{{\"target\":{},\"sha256\":null}}", json(output)));
                continue;
            }
            let output_path = safe_path(root, output)?;
            let output_size = fs::metadata(&output_path)
                .map_err(|_| format!("{} output missing; run cargo xtask assets build", output))?
                .len();
            if output_size > asset.max_bytes
                || fs::read(&output_path).map_err(|error| error.to_string())?
                    != fs::read(&source).map_err(|error| error.to_string())?
            {
                return Err(format!(
                    "generated output drifted or exceeds budget: {output}"
                ));
            }
            output_rows.push(format!(
                "{{\"target\":{},\"sha256\":{}}}",
                json(output),
                json(&sha256(&output_path)?)
            ));
        }
        rows.push(format!(
            "    {{\"id\":{},\"type\":{},\"source\":{},\"outputs\":[{}],\"provenance\":{},\"dark\":{},\"light\":{},\"license\":{},\"fallback\":{},\"bytes\":{},\"sha256\":{}}}",
            json(&asset.id),
            json(&asset.kind),
            json(&asset.source),
            output_rows.join(","),
            json(&asset.provenance),
            asset.dark,
            asset.light,
            json(&asset.license),
            json(&asset.fallback),
            metadata.len(),
            json(&sha256(&source)?)
        ));
    }
    Ok(format!(
        "{{\n  \"version\": 1,\n  \"assets\": [\n{}\n  ]\n}}\n",
        rows.join(",\n")
    ))
}

fn json(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn run(command: &str, root: &Path) -> Result<(), String> {
    let output = root.join("assets/generated/asset-index.json");
    match command {
        "build" => {
            materialize(root)?;
            let index = render_index(root)?;
            fs::create_dir_all(output.parent().unwrap()).map_err(|error| error.to_string())?;
            fs::write(&output, index).map_err(|error| error.to_string())?;
            println!("Built {}", output.display());
        }
        "verify" => {
            let expected = render_index(root)?;
            let actual = fs::read_to_string(&output)
                .map_err(|_| "asset index missing; run cargo xtask assets build".to_owned())?;
            if actual != expected {
                return Err("asset metadata, generated outputs, or source hashes changed; run cargo xtask assets build and review the result".into());
            }
            println!(
                "Verified {} manifest assets",
                parse_manifest(&root.join("assets/manifest.toml"))?.len()
            );
        }
        "preview" => {
            println!("SAIL UI asset preview\n\n{}", render_index(root)?);
        }
        "clean" => {
            if output.exists() {
                fs::remove_file(&output).map_err(|error| error.to_string())?;
            }
            for asset in parse_manifest(&root.join("assets/manifest.toml"))? {
                for output in asset
                    .outputs
                    .iter()
                    .filter(|item| !item.starts_with("runtime:"))
                {
                    let path = safe_path(root, output)?;
                    if path.exists() {
                        fs::remove_file(path).map_err(|error| error.to_string())?;
                    }
                }
            }
            println!("Removed generated asset outputs and index");
        }
        _ => return Err("usage: cargo xtask assets <build|verify|preview|clean>".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let domain = args.next().unwrap_or_default();
    let command = args.next().unwrap_or_default();
    if domain != "assets" {
        eprintln!("usage: cargo xtask assets <build|verify|preview|clean>");
        return ExitCode::FAILURE;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match run(&command, &root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("asset task failed: {error}");
            ExitCode::FAILURE
        }
    }
}
