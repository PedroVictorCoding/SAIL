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

/// [`FileText`] over the runner PDF adapter.
pub struct MoodlePdf(pub Arc<dyn PdfTextExtractor>);

#[async_trait::async_trait]
impl FileText for MoodlePdf {
    async fn pdf_pages(
        &self,
        sha256: &str,
        max_pages: u32,
        cancel: CancellationToken,
    ) -> Result<Vec<(u32, String)>, String> {
        let count = self.0.page_count(sha256).await.map_err(|e| e.to_string())?;
        let last_wanted = count.min(max_pages);
        let mut out = Vec::new();
        let mut first = 1;
        while first <= last_wanted {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let last = (first + PDF_TEXT_PAGES_PER_CALL - 1).min(last_wanted);
            let pages = self
                .0
                .pages(sha256, first, last, cancel.child_token())
                .await
                .map_err(|e| e.to_string())?;
            out.extend(pages.into_iter().map(|p| (p.page, p.text)));
            first = last + 1;
        }
        Ok(out)
    }
}
