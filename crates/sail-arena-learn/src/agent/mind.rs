//! [`Model`] over SAIL Mind's resident local model. Interactive priority: the learner is
//! waiting. Mind's own admission still protects live voice.
use crate::core::ports::Model;
use sail_protocol::resources::WorkPriority;
use sail_protocol::{InferenceRequest, InferenceResponse};
use tokio_util::sync::CancellationToken;

pub struct MindModel {
    mind: sail_mind::MindService,
}

impl MindModel {
    pub fn new(mind: sail_mind::MindService) -> Self {
        Self { mind }
    }
}

#[async_trait::async_trait]
impl Model for MindModel {
    async fn complete(
        &self,
        request: InferenceRequest,
        cancel: CancellationToken,
    ) -> Result<InferenceResponse, String> {
        self.mind
            .infer_with_priority(request, cancel, None, WorkPriority::Interactive)
            .await
            .map_err(|e| e.message)
    }
}
