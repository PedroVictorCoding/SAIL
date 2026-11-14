//! Planning and parsing for a per-plugin Python environment.
//!
//! SAIL never installs anything here: it normalizes what a plugin asks for, refuses the
//! forms pip would execute or fetch outside the wheel index, and reads pip's report after
//! Claude has built the environment inside the sandbox.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

/// A requirement line after PEP 503 normalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Requirement {
    /// Normalized PEP 503 name.
    pub name: String,
    /// Normalized extras, in input order.
    pub extras: Vec<String>,
    /// Version specifier text, e.g. `>=1.2,<2`.
    pub specifier: String,
    /// Environment marker text after `;`.
    pub marker: String,
}

/// What a plugin needs, and what could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EnvPlan {
    /// `requirements.txt`, `pyproject.toml`, or empty when the plugin needs nothing.
    pub source: String,
    /// Accepted requirements in file order.
    pub requirements: Vec<Requirement>,
    /// Refused lines with the reason.
    pub refused: Vec<String>,
    /// SHA-256 hex of the normalized requirement lines joined with `\n`.
    pub hash: String,
}

/// One installed package from pip's `--report` JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Locked {
    /// Distribution name.
    pub name: String,
    /// Installed version.
    pub version: String,
    /// Wheel archive SHA-256.
    pub sha256: String,
    /// Where the wheel was downloaded from.
    pub url: String,
}

/// What Claude writes to `<env_root>/<plugin_id>/env.json` after a build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EnvRecord {
    /// The plan hash this environment was built from.
    pub plan_hash: String,
    /// Unix seconds.
    pub built_at: i64,
    /// The interpreter path inside the environment.
    pub python: String,
    /// The packages pip installed.
    pub locked: Vec<Locked>,
}

/// The environment state the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvState {
    /// The plugin needs no packages.
    NotNeeded,
    /// There is no environment yet.
    Needed,
    /// The environment matches the plan.
    Ready,
    /// The plan changed since the build.
    Changed,
    /// The plan contains a refused line.
    Refused,
}

static REQUIREMENT: LazyLock<Regex> = LazyLock::new(|| {
    // Static pattern: the only `expect` is this local invariant.
    Regex::new(r"^([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[([^\]]*)\])?\s*(.*)$")
        .expect("static requirement pattern")
});

/// Read the plugin's `requirements.txt`, or `[project] dependencies` from
/// `pyproject.toml`, and build the plan. Reads only those two files.
pub fn plan(plugin_dir: &Path) -> Result<EnvPlan, String> {
    let requirements_path = plugin_dir.join("requirements.txt");
    let pyproject_path = plugin_dir.join("pyproject.toml");
    let (source, text) = if requirements_path.is_file() {
        (
            "requirements.txt",
            fs::read_to_string(&requirements_path)
                .map_err(|error| format!("requirements.txt: {error}"))?,
        )
    } else if pyproject_path.is_file() {
        let text = fs::read_to_string(&pyproject_path)
            .map_err(|error| format!("pyproject.toml: {error}"))?;
        let dependencies = parse_pyproject_dependencies(&text)?;
        ("pyproject.toml", dependencies.join("\n"))
    } else {
        ("", String::new())
    };
    let (requirements, refused) = parse_requirements(&text);
    let hash = sha256_hex(
        &requirements
            .iter()
            .map(requirement_text)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    Ok(EnvPlan {
        source: source.to_string(),
        requirements,
        refused,
        hash,
    })
}

/// Normalize accepted requirement lines and collect refused ones with their reason.
pub fn parse_requirements(text: &str) -> (Vec<Requirement>, Vec<String>) {
    let mut requirements = Vec::new();
    let mut refused = Vec::new();
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(reason) = option_reason(line) {
            refused.push(format!("{line}: {reason}"));
            continue;
        }
        if is_url(line) {
            refused.push(format!("{line}: URLs are not supported"));
            continue;
        }
        if is_local_path(line) {
            refused.push(format!("{line}: local paths are not supported"));
            continue;
        }
        let (body, marker) = match line.split_once(';') {
            Some((body, marker)) => (body.trim(), marker.trim()),
            None => (line, ""),
        };
        let Some(captures) = REQUIREMENT.captures(body) else {
            refused.push(format!("{line}: not a valid requirement"));
            continue;
        };
        let name = normalize_name(&captures[1]);
        let extras = captures
            .get(2)
            .map(|value| normalize_extras(value.as_str()))
            .unwrap_or_default();
        let specifier = captures
            .get(3)
            .map(|value| value.as_str().split_whitespace().collect::<String>())
            .unwrap_or_default();
        if specifier.starts_with('@') {
            refused.push(format!("{line}: direct URL references are not supported"));
            continue;
        }
        if !specifier.is_empty() && !specifier.starts_with(['=', '<', '>', '!', '~']) {
            if specifier.contains(['/', '\\']) {
                refused.push(format!("{line}: local paths are not supported"));
            } else {
                refused.push(format!("{line}: not a valid version specifier"));
            }
            continue;
        }
        if requirements.len() >= 50 {
            refused.push(format!("{line}: more than 50 requirements"));
            continue;
        }
        requirements.push(Requirement {
            name,
            extras,
            specifier,
            marker: marker.to_string(),
        });
    }
    (requirements, refused)
}

/// Read the `dependencies = [ … ]` string array of the `[project]` table.
pub fn parse_pyproject_dependencies(text: &str) -> Result<Vec<String>, String> {
    let mut section = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let clean = strip_comment(line).trim();
        if clean.starts_with('[') && clean.ends_with(']') {
            section = clean[1..clean.len() - 1].trim().to_lowercase();
            continue;
        }
        if section != "project" {
            continue;
        }
        let Some(rest) = dependency_key(clean) else {
            continue;
        };
        let Some(equals) = rest.find('=') else {
            continue;
        };
        let value = rest[equals + 1..].trim_start();
        if !value.starts_with('[') {
            return Err("dependencies must be an array of strings".into());
        }
        let mut source = value[1..].to_string();
        for next in lines.by_ref() {
            source.push('\n');
            source.push_str(next);
        }
        return parse_string_array(&source);
    }
    Ok(Vec::new())
}

/// The exact pip command for a build. Pure.
pub fn pip_args(venv: &Path, report_path: &Path, plan: &EnvPlan) -> Vec<String> {
    let mut args = vec![
        venv.join("bin/python").display().to_string(),
        "-I".into(),
        "-m".into(),
        "pip".into(),
        "install".into(),
        "--only-binary=:all:".into(),
        "--no-input".into(),
        "--disable-pip-version-check".into(),
        "--no-cache-dir".into(),
        "--report".into(),
        report_path.display().to_string(),
    ];
    args.extend(plan.requirements.iter().map(requirement_text));
    args
}

/// Read pip's `--report` JSON. Extra fields are ignored.
pub fn parse_report(json: &[u8]) -> Result<Vec<Locked>, String> {
    let value: Value =
        serde_json::from_slice(json).map_err(|error| format!("invalid pip report: {error}"))?;
    let install = value
        .get("install")
        .and_then(Value::as_array)
        .ok_or_else(|| "pip report has no install list".to_string())?;
    let mut locked = Vec::with_capacity(install.len());
    for (index, item) in install.iter().enumerate() {
        let metadata = item
            .get("metadata")
            .ok_or_else(|| format!("install[{index}]: missing metadata"))?;
        let name = metadata
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("install[{index}]: missing name"))?;
        let version = metadata
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("install[{index}]: missing version"))?;
        let download = item
            .get("download_info")
            .ok_or_else(|| format!("install[{index}]: missing download_info"))?;
        let url = download
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("install[{index}]: missing download url"))?;
        let sha256 = download
            .get("archive_info")
            .and_then(|archive| archive.get("hashes"))
            .and_then(|hashes| hashes.get("sha256"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("install[{index}]: missing wheel sha256"))?;
        locked.push(Locked {
            name: name.to_string(),
            version: version.to_string(),
            sha256: sha256.to_string(),
            url: url.to_string(),
        });
    }
    Ok(locked)
}

/// Decide the environment state from the plan and the optional build record. Pure.
pub fn state(plan: &EnvPlan, record: Option<&EnvRecord>) -> EnvState {
    if plan.requirements.is_empty() && plan.refused.is_empty() {
        return EnvState::NotNeeded;
    }
    if !plan.refused.is_empty() {
        return EnvState::Refused;
    }
    match record {
        None => EnvState::Needed,
        Some(record) if record.plan_hash != plan.hash => EnvState::Changed,
        Some(_) => EnvState::Ready,
    }
}

fn requirement_text(requirement: &Requirement) -> String {
    let mut text = requirement.name.clone();
    if !requirement.extras.is_empty() {
        text.push('[');
        text.push_str(&requirement.extras.join(","));
        text.push(']');
    }
    text.push_str(&requirement.specifier);
    if !requirement.marker.is_empty() {
        text.push_str("; ");
        text.push_str(&requirement.marker);
    }
    text
}

fn normalize_name(name: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for character in name.to_ascii_lowercase().chars() {
        if character == '-' || character == '_' || character == '.' {
            separator = true;
        } else {
            if separator && !out.is_empty() {
                out.push('-');
            }
            separator = false;
            out.push(character);
        }
    }
    if separator && !out.is_empty() {
        out.push('-');
    }
    out
}

fn normalize_extras(extras: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for extra in extras.split(',') {
        let extra = normalize_name(extra.trim());
        if !extra.is_empty() && !out.contains(&extra) {
            out.push(extra);
        }
    }
    out
}

fn option_reason(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    if lower.starts_with("-e") && (lower.len() == 2 || lower.as_bytes()[2].is_ascii_whitespace())
        || lower.starts_with("--editable")
    {
        return Some("editable installs are not supported");
    }
    if lower.starts_with("-r") && (lower.len() == 2 || lower.as_bytes()[2].is_ascii_whitespace())
        || lower.starts_with("--requirement")
        || lower.starts_with("-c")
        || lower.starts_with("--constraint")
    {
        return Some("includes are not supported");
    }
    if line.starts_with('-') {
        return Some("options are not supported");
    }
    None
}

fn is_url(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("git+")
        || lower.starts_with("hg+")
        || lower.starts_with("svn+")
        || lower.starts_with("bzr+")
        || lower.starts_with("file:")
}

fn is_local_path(line: &str) -> bool {
    line.starts_with('.')
        || line.starts_with('/')
        || line.starts_with('~')
        || line.contains('\\')
        || line.ends_with(".whl")
        || line.ends_with(".tar.gz")
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match (quote, character) {
            (None, '"') | (None, '\'') => quote = Some(character),
            (Some(open), character) if open == character => quote = None,
            (None, '#') => return &line[..index],
            _ => {}
        }
    }
    line
}

fn dependency_key(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("dependencies")?;
    let boundary = rest.chars().next()?;
    (boundary.is_whitespace() || boundary == '=').then_some(rest)
}

fn parse_string_array(source: &str) -> Result<Vec<String>, String> {
    let mut characters = source.chars().peekable();
    let mut values = Vec::new();
    loop {
        while let Some(character) = characters.peek().copied() {
            if character.is_whitespace() || character == ',' {
                characters.next();
            } else if character == '#' {
                for character in characters.by_ref() {
                    if character == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        match characters.next() {
            Some(']') => return Ok(values),
            Some(quote @ ('"' | '\'')) => values.push(parse_string(&mut characters, quote)?),
            Some(_) => return Err("dependencies must be an array of strings".into()),
            None => return Err("unterminated dependencies array".into()),
        }
    }
}

fn parse_string(
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
    quote: char,
) -> Result<String, String> {
    let mut value = String::new();
    loop {
        match characters.next() {
            Some(character) if character == quote => return Ok(value),
            Some('\\') if quote == '"' => match characters.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('r') => value.push('\r'),
                Some(character) => value.push(character),
                None => return Err("unterminated string in dependencies".into()),
            },
            Some(character) => value.push(character),
            None => return Err("unterminated string in dependencies".into()),
        }
    }
}

fn sha256_hex(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
