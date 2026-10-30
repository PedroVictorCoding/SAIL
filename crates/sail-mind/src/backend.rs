use async_trait::async_trait;
use sail_protocol::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait InferenceBackend: Send + Sync {
    fn name(&self) -> &str;
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
