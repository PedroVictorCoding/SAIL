//! Agent Skills (`SKILL.md` folders) for SAIL, compatible with Hermes Agent.
//!
//! Implements the loader and progressive disclosure of
//! `docs/SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md` §5 (phase P4): the model sees an index
//! (name and description), loads a body with `skill:view`, then individual files on
//! request. A skill is text: it never grants a permission, and `allowed-tools` is only
//! reported, never enforced as a grant.
//!
//! Sources, first match of a name wins:
//! 1. `$SAIL_DATA/skills/<name>/SKILL.md`, SAIL's own skills.
//! 2. `$HERMES_HOME/skills` (default `~/.hermes/skills`), the owner's Hermes skills, with
//!    or without one category folder (`skills/<category>/<name>/SKILL.md`).
//! 3. Skills of enabled Hermes plugins, named `<plugin>:<skill>`.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

const MAX_SKILL_MD: u64 = 256 * 1024;
const MAX_FILE: u64 = 64 * 1024;
const MAX_SKILLS: usize = 512;
const MAX_LISTED_FILES: usize = 200;

/// One loaded skill's index entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// `sail`, `hermes` or `plugin:<id>`.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    /// Requested tools only; Guardian still decides every call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(skip)]
    pub dir: PathBuf,
}

/// A `SKILL.md` that was found but not loaded, and why.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rejected {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<serde_norway::Value>,
}

/// Split `SKILL.md` into its YAML frontmatter and Markdown body.
pub fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// The spec's name rule: 1–64 of `a-z`, `0-9` and `-`, no leading, trailing or double `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// Parse and validate one skill folder.
pub fn load_skill(dir: &Path, source: &str) -> Result<Skill, String> {
    let file = dir.join("SKILL.md");
    let meta = fs::symlink_metadata(&file).map_err(|e| format!("SKILL.md: {e}"))?;
    if !meta.is_file() {
        return Err("SKILL.md is not a regular file".into());
    }
    if meta.len() > MAX_SKILL_MD {
        return Err("SKILL.md is larger than 256 KiB".into());
    }
    let text = fs::read_to_string(&file).map_err(|e| format!("SKILL.md: {e}"))?;
    let (front, _) = split_frontmatter(&text).ok_or("SKILL.md has no YAML frontmatter")?;
    let front: Frontmatter =
        serde_norway::from_str(front).map_err(|e| format!("frontmatter: {e}"))?;
    let name = front.name.ok_or("frontmatter has no name")?;
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if !valid_name(&name) {
        return Err(format!("invalid skill name {name:?}"));
    }
    if name != folder {
        return Err(format!(
            "skill name {name:?} does not match its folder {folder:?}"
        ));
    }
    let description = front
        .description
        .map(|d| d.trim().to_string())
        .unwrap_or_default();
    if description.is_empty() || description.chars().count() > 1024 {
        return Err("description must be 1–1024 characters".into());
    }
    if front
        .compatibility
        .as_ref()
        .is_some_and(|c| c.chars().count() > 500)
    {
        return Err("compatibility is longer than 500 characters".into());
    }
    let allowed_tools = match front.allowed_tools {
        Some(serde_norway::Value::String(s)) => s.split_whitespace().map(String::from).collect(),
        Some(serde_norway::Value::Sequence(items)) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    Ok(Skill {
        name,
        description,
        source: source.into(),
        license: front.license,
        compatibility: front.compatibility,
        allowed_tools,
        dir: dir.to_path_buf(),
    })
}

fn real_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

/// Every skill SAIL can offer, plus the folders it refused.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SkillLibrary {
    pub skills: BTreeMap<String, Skill>,
    pub rejected: Vec<Rejected>,
}

impl SkillLibrary {
    /// Default roots: SAIL's data directory, then the owner's Hermes home.
    pub fn default_roots() -> Vec<(String, PathBuf)> {
        let mut roots = vec![(
            "sail".to_string(),
            sail_common::storage::data_path("skills"),
        )];
        if let Some(home) = hermes_home() {
            roots.push(("hermes".into(), home.join("skills")));
        }
        roots
    }

    /// Scan `roots` in order. A root may hold skills directly or one category level deep.
    pub fn scan(roots: &[(String, PathBuf)]) -> Self {
        let mut library = Self::default();
        for (source, root) in roots {
            for dir in real_dirs(root) {
                if dir.join("SKILL.md").exists() {
                    library.add(&dir, source, None);
                } else {
                    for inner in real_dirs(&dir) {
                        if inner.join("SKILL.md").exists() {
                            library.add(&inner, source, None);
                        }
                    }
                }
            }
        }
        library
    }

    /// Add one skill folder; `prefix` namespaces plugin skills as `<prefix>:<name>`.
    pub fn add(&mut self, dir: &Path, source: &str, prefix: Option<&str>) {
        if self.skills.len() >= MAX_SKILLS {
            return;
        }
        match load_skill(dir, source) {
            Ok(mut skill) => {
                if let Some(prefix) = prefix {
                    skill.name = format!("{prefix}:{}", skill.name);
                }
                self.skills.entry(skill.name.clone()).or_insert(skill);
            }
            Err(reason) => self.rejected.push(Rejected {
                path: dir.display().to_string(),
                reason,
            }),
        }
    }

    /// Index entries whose name or description contains every word of `query`.
    pub fn index(&self, query: Option<&str>, limit: usize) -> Vec<&Skill> {
        let words: Vec<String> = query
            .unwrap_or_default()
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
        self.skills
            .values()
            .filter(|s| {
                let hay = format!("{} {}", s.name, s.description).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .take(limit)
            .collect()
    }

    /// The body of a skill (frontmatter removed) and its resource files, or one resource
    /// file by relative path. Text only, bounded, never outside the skill folder.
    pub fn view(&self, name: &str, file: Option<&str>) -> Result<SkillView, String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| format!("no skill named {name}"))?;
        match file {
            None => {
                let text = fs::read_to_string(skill.dir.join("SKILL.md"))
                    .map_err(|e| format!("SKILL.md: {e}"))?;
                let body = split_frontmatter(&text).map(|(_, b)| b).unwrap_or(&text);
                Ok(SkillView {
                    name: skill.name.clone(),
                    file: "SKILL.md".into(),
                    content: body.trim_start().to_string(),
                    truncated: false,
                    files: list_files(&skill.dir),
                })
            }
            Some(rel) => {
                let path = confined(&skill.dir, rel)?;
                let meta = fs::symlink_metadata(&path).map_err(|_| format!("no file {rel}"))?;
                if !meta.is_file() {
                    return Err(format!("{rel} is not a regular file"));
                }
                let bytes = fs::read(&path).map_err(|e| e.to_string())?;
                let truncated = bytes.len() as u64 > MAX_FILE;
                let bytes = &bytes[..bytes.len().min(MAX_FILE as usize)];
                let content = match std::str::from_utf8(bytes) {
                    Ok(s) => s.to_string(),
                    // A cut inside a multi-byte character is still text.
                    Err(e) if truncated && e.error_len().is_none() => {
                        String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
                    }
                    Err(_) => return Err(format!("{rel} is not a text file")),
                };
                Ok(SkillView {
                    name: skill.name.clone(),
                    file: rel.into(),
                    content,
                    truncated,
                    files: Vec::new(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
    pub name: String,
    pub file: String,
    pub content: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// `$HERMES_HOME`, else `~/.hermes`.
pub fn hermes_home() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".hermes")))
}

/// Resolve `rel` inside `root` with no `..`, absolute parts or symlinks on the way.
fn confined(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || rel.len() > 512
        || !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err("file must be a relative path inside the skill".into());
    }
    let mut path = root.to_path_buf();
    for part in rel_path.components() {
        path.push(part);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("symbolic links are not followed".into());
        }
    }
    Ok(path)
}

fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "__pycache__" {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 3 {
                stack.push((entry.path(), depth + 1));
            } else if kind.is_file()
                && out.len() < MAX_LISTED_FILES
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                let rel = rel.to_string_lossy().to_string();
                if rel != "SKILL.md" {
                    out.push(rel);
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    const HERMES_SKILL: &str = "---\nname: arxiv\ndescription: >\n  Search and retrieve\n  papers from arXiv.\nversion: 1.0.0\nauthor: Hermes Agent\nlicense: MIT\nmetadata:\n  hermes:\n    tags: [Research]\nallowed-tools: web_search web_extract\n---\n# arXiv\n\nUse the API.\n";

    #[test]
    fn names_follow_the_spec() {
        assert!(valid_name("pdf-processing"));
        for bad in ["", "PDF", "-pdf", "pdf-", "pdf--x", "a_b", &"a".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_hermes_skill_imports_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "research/arxiv/SKILL.md", HERMES_SKILL);
        write(tmp.path(), "research/arxiv/scripts/search.py", "print(1)\n");
        let lib = SkillLibrary::scan(&[("hermes".into(), tmp.path().into())]);
        let skill = &lib.skills["arxiv"];
        assert_eq!(skill.description, "Search and retrieve papers from arXiv.");
        assert_eq!(skill.license.as_deref(), Some("MIT"));
        assert_eq!(skill.allowed_tools, ["web_search", "web_extract"]);
        let view = lib.view("arxiv", None).unwrap();
        assert!(view.content.starts_with("# arXiv"));
        assert_eq!(view.files, ["scripts/search.py"]);
        assert_eq!(
            lib.view("arxiv", Some("scripts/search.py"))
                .unwrap()
                .content,
            "print(1)\n"
        );
    }

    #[test]
    fn earlier_roots_win_and_bad_skills_are_reported() {
        let sail = tempfile::tempdir().unwrap();
        let hermes = tempfile::tempdir().unwrap();
        write(
            sail.path(),
            "arxiv/SKILL.md",
            "---\nname: arxiv\ndescription: SAIL's own\n---\nbody",
        );
        write(hermes.path(), "arxiv/SKILL.md", HERMES_SKILL);
        write(
            hermes.path(),
            "wrong/SKILL.md",
            "---\nname: other\ndescription: x\n---\n",
        );
        write(hermes.path(), "nofront/SKILL.md", "# no frontmatter");
        let lib = SkillLibrary::scan(&[
            ("sail".into(), sail.path().into()),
            ("hermes".into(), hermes.path().into()),
        ]);
        assert_eq!(lib.skills["arxiv"].source, "sail");
        assert_eq!(lib.skills.len(), 1);
        assert_eq!(lib.rejected.len(), 2);
        assert!(
            lib.rejected
                .iter()
                .any(|r| r.reason.contains("does not match"))
        );
    }

    #[test]
    fn view_stays_inside_the_skill() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "arxiv/SKILL.md", HERMES_SKILL);
        write(tmp.path(), "secret.txt", "no");
        std::os::unix::fs::symlink(tmp.path().join("secret.txt"), tmp.path().join("arxiv/link"))
            .unwrap();
        let lib = SkillLibrary::scan(&[("hermes".into(), tmp.path().into())]);
        for bad in ["../secret.txt", "/etc/passwd", "link", "./SKILL.md", ""] {
            assert!(lib.view("arxiv", Some(bad)).is_err(), "{bad}");
        }
        assert!(lib.view("missing", None).is_err());
    }

    #[test]
    fn index_filters_by_words() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "arxiv/SKILL.md", HERMES_SKILL);
        let lib = SkillLibrary::scan(&[("hermes".into(), tmp.path().into())]);
        assert_eq!(lib.index(Some("papers ARXIV"), 10).len(), 1);
        assert!(lib.index(Some("music"), 10).is_empty());
    }
}
