use async_trait::async_trait;
use sail_protocol::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait InferenceBackend: Send + Sync {
    fn name(&self) -> &str;
    /// External providers must never become the implicit/default inference route.
    fn is_external(&self) -> bool {
        false
    }
    /// Peak configured generation/prefill thread demand; not observed utilization.
    fn cpu_slots(&self) -> usize {
        1
    }
    fn aliases(&self) -> Vec<String>;
    async fn start(&self) -> Result<ModelStatus>;
    async fn status(&self) -> ModelStatus;
    async fn infer(
        &self,
        request: &InferenceRequest,
        events: Option<&mpsc::Sender<InferenceEvent>>,
        cancel: &CancellationToken,
        limits: &ResourceLimits,
    ) -> Result<InferenceResponse>;
    async fn shutdown(&self) -> Result<()>;
}
