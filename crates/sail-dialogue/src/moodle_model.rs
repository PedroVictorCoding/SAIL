//! The Moodle connector's model and runner adapters.
//!
//! Extraction reuses the Course Factory's [`MindAuthor`] and [`MindGate`]. That gives it
//! the same background priority, voice preemption and error sorting, so Moodle reading can
//! never compete with the owner for the model. PDF text comes from the same `sail-runner`
//! sandbox adapter, pointed at the Moodle mirror (whose blobs are also named by SHA-256).
use sail_arena_learn::factory::contracts::{
    AuthorModel, AuthorModelError, AuthorPrompt, PDF_TEXT_PAGES_PER_CALL, PdfTextExtractor,
};
use sail_arena_learn::factory::jobs::ModelGate;
use sail_arena_learn::factory::mind_author::{MindAuthor, MindGate};
use sail_moodle::service::{AdmitModel, FileText};
use sail_moodle::{ExtractModel, ExtractModelError, ExtractPrompt};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// [`ExtractModel`] over Mind at background priority.
pub struct MoodleMind(pub MindAuthor);

#[async_trait::async_trait]
impl ExtractModel for MoodleMind {
    async fn complete(
        &self,
        prompt: &ExtractPrompt,
        cancel: CancellationToken,
    ) -> Result<String, ExtractModelError> {
        let prompt = AuthorPrompt {
            system: prompt.system.clone(),
            user: prompt.user.clone(),
            schema: "moodle_extraction",
            max_output_tokens: prompt.max_output_tokens,
            reasoning_budget: sail_common::request_body::Job::MoodleExtraction.budget(),
        };
        self.0.complete(&prompt, cancel).await.map_err(|e| match e {
            AuthorModelError::Preempted(m) => ExtractModelError::Preempted(m),
            AuthorModelError::NotAdmitted(m) => ExtractModelError::NotAdmitted(m),
            AuthorModelError::Unavailable(m) => ExtractModelError::Unavailable(m),
            AuthorModelError::Failed(m) => ExtractModelError::Failed(m),
        })
    }
}

/// [`AdmitModel`] over the factory's gate: voice, foreground use and a busy model all wait.
pub struct MoodleGate(pub MindGate);

impl AdmitModel for MoodleGate {
    fn admit(&self) -> Result<(), String> {
        self.0.admit_background()
    }
}

/// The sandbox operation an owner allows once for all PDFs (see Guardian's
/// `STANDING_SANDBOX_PROTOCOLS`).
pub const PDF_PROTOCOL: &str = "pdf-text-v1";

/// PDF jobs authorize as Arena (`sail_runner::pdf::dispatch`), so the standing rule does too.
/// One permission therefore covers Moodle files and Course Factory sources.
pub const PDF_REQUESTER: sail_common::types::SubsystemId = sail_common::types::SubsystemId::Arena;

/// [`FileText`] over the runner PDF adapter. It reads only while the owner's standing
/// "read PDF text in the sandbox" rule exists, so background syncs never raise a prompt.
pub struct MoodlePdf {
    pub reader: Arc<dyn PdfTextExtractor>,
    pub broker: Arc<sail_guardian::GuardianExecutionBroker>,
}

#[async_trait::async_trait]
impl FileText for MoodlePdf {
    fn ready(&self) -> Result<(), String> {
        if self
            .broker
            .policy()
            .allows_sandbox_operation(PDF_REQUESTER, PDF_PROTOCOL)
        {
            Ok(())
        } else {
            Err("PDF reading hasn't been allowed yet".into())
        }
    }

    async fn pdf_pages(
        &self,
        sha256: &str,
        max_pages: u32,
        cancel: CancellationToken,
    ) -> Result<Vec<(u32, String)>, String> {
        let count = self
            .reader
            .page_count(sha256)
            .await
            .map_err(|e| e.to_string())?;
        let last_wanted = count.min(max_pages);
        let mut out = Vec::new();
        let mut first = 1;
        while first <= last_wanted {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let last = (first + PDF_TEXT_PAGES_PER_CALL - 1).min(last_wanted);
            let pages = self
                .reader
                .pages(sha256, first, last, cancel.child_token())
                .await
                .map_err(|e| e.to_string())?;
            out.extend(pages.into_iter().map(|p| (p.page, p.text)));
            first = last + 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use sail_moodle::{DocSource, ExtractionDoc};

    /// Mind rejects an answer budget over its output limit as an invalid request. Until
    /// 2026-11-08 the limit was 1024 and this prompt asked for 2048, so every model
    /// extraction on a real Moodle failed with `InvalidRequest`.
    #[test]
    fn the_extraction_prompt_fits_minds_output_limit() {
        let doc = ExtractionDoc {
            id: "doc".into(),
            course_id: 1,
            course_name: "Compiladores".into(),
            source: DocSource::AssignmentBrief { assignment_id: 2 },
            label: "Trabalho".into(),
            text: "Entrega do compilador até 2026-12-01.".into(),
            observed_at: chrono::Utc::now(),
            term: None,
        };
        let prompt = sail_moodle::extract::prompt::build_prompt(&doc, &[]);
        let limit = sail_protocol::ResourceLimits::default().max_output_tokens;
        assert!(prompt.max_output_tokens <= limit);
    }
}
