//! Unified interruption-safe conversation controller.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TurnId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Generation(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Connected,
    Reconnecting,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputState {
    Muted,
    Listening,
    SpeechDetected,
    Transcribing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseState {
    Idle,
    Queued,
    Generating,
    Speaking,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActionState {
    AwaitingApproval,
    Running,
    Completed,
    Cancelled,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    UserInterrupt,
    NewInput,
    ConnectionLost,
    ModelFailure,
    PermissionRevoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancellationOutcome {
    pub reason: StopReason,
    pub stopped_generation: Generation,
    pub inference_cancelled: bool,
    pub tts_flushed: bool,
    pub tools_stopped: Vec<String>,
    pub already_completed: Vec<String>,
    pub outcome_uncertain: Vec<String>,
}

#[derive(Debug, Clone)]
struct ActiveResponse {
    pub turn_id: TurnId,
    pub generation: Generation,
    pub state: ResponseState,
    pub cancel_token: CancellationToken,
    pub queued_audio_chunks: usize,
    pub tool_tasks: HashMap<String, ToolActionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryMetadata {
    pub session_id: SessionId,
    pub latest_turn: TurnId,
    pub latest_generation: Generation,
    pub active_tools: HashMap<String, ToolActionState>,
    pub last_tool_receipts: Vec<ToolReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolReceipt {
    pub tool_id: String,
    pub turn_id: TurnId,
    pub generation: Generation,
    pub success: bool,
    pub outcome: serde_json::Value,
}

#[derive(Clone)]
pub struct ConversationController {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    session_id: SessionId,
    session_state: SessionState,
    input_state: InputState,
    active_response: Option<ActiveResponse>,
    recovery: RecoveryMetadata,
    next_turn: u64,
    next_generation: u64,
    outbound_tx: Option<mpsc::Sender<serde_json::Value>>,
}

impl ConversationController {
    pub fn new() -> Self {
        let session_id = SessionId(Uuid::new_v4());
        Self {
            inner: Arc::new(Mutex::new(Inner {
                session_id,
                session_state: SessionState::Connected,
                input_state: InputState::Listening,
                active_response: None,
                recovery: RecoveryMetadata {
                    session_id,
                    latest_turn: TurnId(0),
                    latest_generation: Generation(0),
                    active_tools: HashMap::new(),
                    last_tool_receipts: Vec::new(),
                },
                next_turn: 1,
                next_generation: 1,
                outbound_tx: None,
            })),
        }
    }

    pub async fn set_outbound_channel(&self, tx: mpsc::Sender<serde_json::Value>) {
        let mut inner = self.inner.lock().await;
        inner.outbound_tx = Some(tx);
    }

    pub async fn validate_event(&self, event_generation: Generation) -> bool {
        let inner = self.inner.lock().await;
        inner.recovery.latest_generation == event_generation
    }

    pub async fn start_new_turn(&self) -> (TurnId, Generation, CancellationToken) {
        let mut inner = self.inner.lock().await;

        if let Some(active) = &inner.active_response {
            active.cancel_token.cancel();
        }

        let turn_id = TurnId(inner.next_turn);
        let generation = Generation(inner.next_generation);
        let cancel_token = CancellationToken::new();

        inner.next_turn += 1;
        inner.next_generation += 1;
        inner.input_state = InputState::Listening;
        inner.active_response = Some(ActiveResponse {
            turn_id,
            generation,
            state: ResponseState::Queued,
            cancel_token: cancel_token.clone(),
            queued_audio_chunks: 0,
            tool_tasks: HashMap::new(),
        });

        inner.recovery.latest_turn = turn_id;
        inner.recovery.latest_generation = generation;

        tracing::info!(session_id = ?inner.session_id, turn = ?turn_id, gen = ?generation, "New turn started");
        (turn_id, generation, cancel_token)
    }

    pub async fn stop(&self, reason: StopReason) -> CancellationOutcome {
        let mut inner = self.inner.lock().await;

        let Some(active) = inner.active_response.take() else {
            return CancellationOutcome {
                reason,
                stopped_generation: inner.recovery.latest_generation,
                inference_cancelled: false,
                tts_flushed: false,
                tools_stopped: Vec::new(),
                already_completed: Vec::new(),
                outcome_uncertain: Vec::new(),
            };
        };

        active.cancel_token.cancel();

        let mut tools_stopped = Vec::new();
        let mut already_completed = Vec::new();
        let mut outcome_uncertain = Vec::new();

        for (tool_id, state) in active.tool_tasks {
            match state {
                ToolActionState::Running => tools_stopped.push(tool_id),
                ToolActionState::Completed | ToolActionState::AwaitingApproval => {
                    already_completed.push(tool_id)
                }
                ToolActionState::OutcomeUncertain => outcome_uncertain.push(tool_id),
                ToolActionState::Cancelled => {}
            }
        }

        if let Some(tx) = &inner.outbound_tx {
            let _ = tx
                .send(serde_json::json!({
                    "type": "conversation_stopped",
                    "reason": reason,
                    "generation": active.generation.0,
                }))
                .await;
        }

        inner.input_state = InputState::Listening;

        tracing::info!(reason = ?reason, turn = ?active.turn_id, generation = ?active.generation, "Conversation stopped");

        CancellationOutcome {
            reason,
            stopped_generation: active.generation,
            inference_cancelled: true,
            tts_flushed: active.queued_audio_chunks > 0,
            tools_stopped,
            already_completed,
            outcome_uncertain,
        }
    }

    pub async fn set_response_state(&self, generation: Generation, state: ResponseState) {
        let mut inner = self.inner.lock().await;
        if let Some(active) = &mut inner.active_response
            && active.generation == generation
        {
            active.state = state;
        }
    }

    pub async fn recovery_metadata(&self) -> RecoveryMetadata {
        let inner = self.inner.lock().await;
        inner.recovery.clone()
    }

    pub async fn set_session_reconnecting(&self) {
        let mut inner = self.inner.lock().await;
        inner.session_state = SessionState::Reconnecting;
    }

    pub async fn set_session_connected(&self) {
        let mut inner = self.inner.lock().await;
        inner.session_state = SessionState::Connected;
    }

    pub async fn current_generation(&self) -> Generation {
        let inner = self.inner.lock().await;
        inner.recovery.latest_generation
    }

    pub async fn current_turn(&self) -> TurnId {
        let inner = self.inner.lock().await;
        inner.recovery.latest_turn
    }
}

impl Default for ConversationController {
    fn default() -> Self {
        Self::new()
    }
}
