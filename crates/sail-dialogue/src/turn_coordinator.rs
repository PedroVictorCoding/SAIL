use crate::{KernelToolResult, speech::SpeechChunker, staged_ack::StagedAckCatalog};
use serde_json::{Value, json};
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const FAST_PATH_LATENCY_THRESHOLD_MS: u64 = 250;

#[derive(Debug, Clone)]
pub struct StagedToolOutcome {
    pub result: KernelToolResult,
    pub ack_spoken: bool,
    pub duration_ms: u64,
}

pub struct TurnCoordinator;

impl TurnCoordinator {
    /// Execute a tool concurrently while orchestrating staged voice acknowledgements.
    ///
    /// 1. Emits `tool_planned` event to telemetry.
    /// 2. Starts tool execution in parallel.
    /// 3. If tool completes within FAST_PATH_LATENCY_THRESHOLD_MS, skips acknowledgment
    ///    to avoid unnecessary conversational filler.
    /// 4. If tool takes longer, immediately emits `ack_started`, queues the localized
    ///    acknowledgement phrase into the TTS speech pipeline, and then awaits tool completion.
    /// 5. Emits `tool_completed` event with duration telemetry.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_staged<F, OutboundEmit>(
        tool_name: &str,
        params: &Value,
        locale: &str,
        tool_fut: F,
        current: u64,
        speech_tx: &mpsc::Sender<String>,
        _chunker: &mut SpeechChunker,
        reply: &mut String,
        emit_fn: OutboundEmit,
        _cancel: Option<CancellationToken>,
    ) -> StagedToolOutcome
    where
        F: Future<Output = KernelToolResult>,
        OutboundEmit: Fn(Value) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>,
    {
        let start = Instant::now();

        // 1. Emit tool_planned
        emit_fn(json!({
            "type": "tool_planned",
            "channel": "voice:vlm",
            "tool": tool_name,
            "turn_id": current,
        }))
        .await;

        tokio::pin!(tool_fut);

        // 2. Race tool future against the fast-path threshold
        let fast_path_timeout = Duration::from_millis(FAST_PATH_LATENCY_THRESHOLD_MS);

        let (result, ack_spoken) = tokio::select! {
            res = &mut tool_fut => {
                // Completed very quickly (<250ms), skip conversational filler!
                (res, false)
            }
            _ = tokio::time::sleep(fast_path_timeout) => {
                // Tool is taking longer: synthesize & stream staged acknowledgement
                let ack_text = StagedAckCatalog::phrase_for(tool_name, params, locale);

                emit_fn(json!({
                    "type": "ack_started",
                    "channel": "voice:vlm",
                    "tool": tool_name,
                    "text": ack_text,
                    "turn_id": current,
                })).await;

                emit_fn(json!({
                    "type": "tool_started",
                    "channel": "voice:vlm",
                    "tool": tool_name,
                    "turn_id": current,
                })).await;

                // Queue ack speech into TTS pipeline so client hears audio immediately
                reply.push_str(ack_text);
                reply.push(' ');
                let _ = speech_tx.send(ack_text.to_string()).await;

                // Await remaining tool execution
                let res = tool_fut.await;
                (res, true)
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        // 3. Emit tool_completed
        emit_fn(json!({
            "type": "tool_completed",
            "channel": "voice:vlm",
            "tool": tool_name,
            "turn_id": current,
            "duration_ms": duration_ms,
            "success": result.success,
        }))
        .await;

        StagedToolOutcome {
            result,
            ack_spoken,
            duration_ms,
        }
    }
}
