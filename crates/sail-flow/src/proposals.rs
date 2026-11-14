//! Self-proposed skills, staged until the owner approves them (P4, round 20).
//!
//! The model can write down a procedure that worked as a new Agent Skill with the
//! `skill:propose` tool. The proposal is stored at
//! `$SAIL_DATA/skills-proposed/<id>/` (`SKILL.md` + `proposal.json`), scanned by the market
//! scanner with trust `AgentCreated`, and shown in the Skills & plugins section. It
//! becomes a real skill in `$SAIL_DATA/skills/<name>/` only when the owner approves it;
//! the model never sees a proposal as a skill before that. A proposal never overwrites an
//! existing skill, and the scanner's `block` decision cannot be approved.
use crate::market::{Bundle, Decision, Kind, Report, Trust, guard};
use crate::skills::valid_name;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

pub const MAX_PENDING: usize = 20;
pub const MAX_INSTRUCTIONS_BYTES: usize = 32 * 1024;
pub const MAX_REASON_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// 32 lowercase hex characters.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Why the model proposed it, in its words (shown to the owner as untrusted text).
    pub reason: String,
    /// The full `SKILL.md` the owner would approve.
    pub body: String,
    /// Unix seconds.
    pub proposed_at: i64,
    pub report: Report,
    pub decision: Decision,
    #[serde(default)]
    pub decision_reason: String,
    /// True when a skill with this name already exists (filled when listing).
    #[serde(default)]
    pub conflict: bool,
}

#[derive(Debug, Clone)]
pub struct Proposals {
    /// `$SAIL_DATA/skills-proposed`
    pub dir: PathBuf,
    /// `$SAIL_DATA/skills`
    pub skills: PathBuf,
}

#[derive(Serialize)]
struct Frontmatter<'a> {
    name: &'a str,
    description: &'a str,
    metadata: FrontmatterMeta,
}

#[derive(Serialize)]
struct FrontmatterMeta {
    #[serde(rename = "sail-proposed")]
    sail_proposed: bool,
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Proposals {
    pub fn default_paths() -> Self {
        Self {
            dir: sail_common::storage::data_path("skills-proposed"),
            skills: sail_common::storage::data_path("skills"),
        }
    }

    fn folder(&self, id: &str) -> Result<PathBuf, String> {
        if valid_id(id) {
            Ok(self.dir.join(id))
        } else {
            Err("invalid proposal id".into())
        }
    }

    /// Stage a proposal. A pending proposal with the same name is replaced.
    pub fn propose(
        &self,
        name: &str,
        description: &str,
        instructions: &str,
        reason: &str,
        now: i64,
    ) -> Result<Proposal, String> {
        if !valid_name(name) {
            return Err(format!(
                "{name:?} is not a valid skill name: 1-64 lowercase letters, digits and single hyphens"
            ));
        }
        let description = description.trim();
        if description.is_empty() || description.chars().count() > 1024 {
            return Err("description must be 1-1024 characters".into());
        }
        let instructions = instructions.trim();
        if instructions.is_empty() || instructions.len() > MAX_INSTRUCTIONS_BYTES {
            return Err("instructions must be 1 byte to 32 KiB".into());
        }
        let reason: String = reason.trim().chars().take(MAX_REASON_CHARS).collect();
        let front = serde_norway::to_string(&Frontmatter {
            name,
            description,
            metadata: FrontmatterMeta {
                sail_proposed: true,
            },
        })
        .map_err(|e| e.to_string())?;
        let body = format!(
            "---\n{}---\n{instructions}\n",
            front.trim_start_matches("---\n")
        );
        // The skill must load exactly as the owner will see it.
        let bundle = Bundle {
            files: [("SKILL.md".to_string(), body.clone().into_bytes())].into(),
            revision: String::new(),
        };
        let report = guard::scan(&bundle, Kind::Skill);
        let (decision, decision_reason) = guard::decide(Trust::AgentCreated, &report);

        let mut pending = self.list()?;
        for old in pending.iter().filter(|p| p.name == name) {
            let _ = fs::remove_dir_all(self.folder(&old.id)?);
        }
        pending.retain(|p| p.name != name);
        if pending.len() >= MAX_PENDING {
            return Err(format!(
                "{MAX_PENDING} proposals are already waiting for the owner; wait until they review them"
            ));
        }
        let proposal = Proposal {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: name.to_string(),
            description: description.to_string(),
            reason,
            body,
            proposed_at: now,
            report,
            decision,
            decision_reason,
            conflict: self.skills.join(name).exists(),
        };
        let folder = self.folder(&proposal.id)?;
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        fs::write(folder.join("SKILL.md"), &proposal.body).map_err(|e| e.to_string())?;
        fs::write(
            folder.join("proposal.json"),
            serde_json::to_vec_pretty(&proposal).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(proposal)
    }

    /// Pending proposals, oldest first.
    pub fn list(&self) -> Result<Vec<Proposal>, String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<Proposal> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| fs::read(e.path().join("proposal.json")).ok())
            .filter_map(|b| serde_json::from_slice::<Proposal>(&b).ok())
            .filter(|p| valid_id(&p.id))
            .collect();
        for p in &mut out {
            p.conflict = self.skills.join(&p.name).exists();
        }
        out.sort_by(|a, b| (a.proposed_at, &a.id).cmp(&(b.proposed_at, &b.id)));
        Ok(out)
    }

    fn get(&self, id: &str) -> Result<Proposal, String> {
        let folder = self.folder(id)?;
        let bytes =
            fs::read(folder.join("proposal.json")).map_err(|_| "no such proposal".to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    /// Install a proposal as `$SAIL_DATA/skills/<name>/`. Returns the skill name.
    pub fn approve(&self, id: &str, acknowledge: bool) -> Result<String, String> {
        let proposal = self.get(id)?;
        match proposal.decision {
            Decision::Block => return Err(proposal.decision_reason),
            Decision::NeedsAck if !acknowledge => {
                return Err(format!(
                    "{} needs your confirmation",
                    proposal.decision_reason
                ));
            }
            _ => {}
        }
        let folder = self.folder(id)?;
        let body = fs::read_to_string(folder.join("SKILL.md")).map_err(|e| e.to_string())?;
        if body != proposal.body {
            return Err("the proposal changed on disk; reject it and ask again".into());
        }
        let target = self.skills.join(&proposal.name);
        if target.exists() {
            return Err(format!(
                "a skill named {} already exists; SAIL does not overwrite skills from proposals",
                proposal.name
            ));
        }
        fs::create_dir_all(&self.skills).map_err(|e| e.to_string())?;
        let tmp = self.skills.join(format!(".{}.proposed-new", proposal.name));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        fs::write(tmp.join("SKILL.md"), &body).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
        fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;
        Ok(proposal.name)
    }

    pub fn reject(&self, id: &str) -> Result<(), String> {
        let folder = self.folder(id)?;
        match fs::remove_dir_all(&folder) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("no such proposal".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}
