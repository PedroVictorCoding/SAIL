//! Local speech model access. Workers are persistent, bounded and isolated by engine.
use base64::{Engine, engine::general_purpose::STANDARD};
use sail_common::error::{Result, SailError};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, Semaphore},
};

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
#[derive(Clone)]
struct WorkerClient {
    mode: &'static str,
    worker: Arc<Mutex<Option<Worker>>>,
    capacity: Arc<Semaphore>,
}
impl WorkerClient {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            worker: Arc::new(Mutex::new(None)),
            capacity: Arc::new(Semaphore::new(8)),
        }
    }
    async fn request(&self, request: Value) -> Result<Value> {
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| SailError::Inference(format!("{} worker queue full", self.mode)))?;
        let this = self.clone();
        // The task drains the response even when its caller is cancelled, preserving framing.
        tokio::spawn(async move {
            let _permit = permit;
            let mut slot = this.worker.lock().await;
            if slot.is_none() {
                let python =
                    std::env::var("SAIL_VOICE_PYTHON").unwrap_or(".venv-voice/bin/python".into());
                let script =
                    std::env::var("SAIL_VOICE_WORKER").unwrap_or("scripts/voice/worker.py".into());
                let mut child = Command::new(python)
                    .arg(script)
                    .arg(this.mode)
                    .env("HF_HUB_OFFLINE", "1")
                    .env("TRANSFORMERS_OFFLINE", "1")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::inherit())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| {
                        SailError::Inference(format!(
                            "{} worker unavailable: {e}; run scripts/voice/setup.sh",
                            this.mode
                        ))
                    })?;
                *slot = Some(Worker {
                    input: child.stdin.take().unwrap(),
                    output: BufReader::new(child.stdout.take().unwrap()),
                    child,
                });
            }
            let worker = slot.as_mut().unwrap();
            let operation = async {
                let mut data = serde_json::to_vec(&request)?;
                data.push(b'\n');
                worker.input.write_all(&data).await?;
                worker.input.flush().await?;
                let mut line = String::new();
                if worker.output.read_line(&mut line).await? == 0 {
                    return Err(SailError::Inference("Speech worker exited".into()));
                }
                let result: Value = serde_json::from_str(&line)?;
                if result["ok"] != true {
                    return Err(SailError::Inference(
                        result["error"]
                            .as_str()
                            .unwrap_or("Speech inference failed")
                            .into(),
                    ));
                }
                Ok(result["result"].clone())
            };
            let result = tokio::time::timeout(Duration::from_secs(120), operation).await;
            match result {
                Ok(Ok(value)) => Ok(value),
                error => {
                    if let Some(mut worker) = slot.take() {
                        let _ = worker.child.kill().await;
                    }
                    match error {
                        Ok(Err(e)) => Err(e),
                        _ => Err(SailError::Inference(format!(
                            "{} worker timed out",
                            this.mode
                        ))),
                    }
                }
            }
        })
        .await
        .map_err(|e| SailError::Inference(e.to_string()))?
    }
}

#[derive(Clone)]
pub struct VoiceModelServices {
    vad: WorkerClient,
    stt: WorkerClient,
    tts: WorkerClient,
}
impl Default for VoiceModelServices {
    fn default() -> Self {
        Self::new()
    }
}
impl VoiceModelServices {
    pub fn new() -> Self {
        Self {
            vad: WorkerClient::new("vad"),
            stt: WorkerClient::new("stt"),
            tts: WorkerClient::new("tts"),
        }
    }
    pub async fn health(&self) -> Value {
        let (vad, stt, tts) = tokio::join!(
            self.vad.request(json!({"op":"health"})),
            self.stt.request(json!({"op":"health"})),
            self.tts.request(json!({"op":"health"}))
        );
        fn status(r: Result<Value>) -> Value {
            match r {
                Ok(v) => v,
                Err(e) => json!({"ready":false,"error":e.to_string()}),
            }
        }
        json!({"vad":status(vad),"stt":status(stt),"tts":status(tts)})
    }
    pub async fn vad(&self, session: &str, samples: &[i16]) -> Result<(f32, u64)> {
        let v = self
            .vad
            .request(json!({"session_id":session,"pcm":encode_pcm(samples)}))
            .await?;
        Ok((
            v["confidence"].as_f64().unwrap_or(0.) as f32,
            v["inference_us"].as_u64().unwrap_or(0),
        ))
    }
    pub async fn reset_vad(&self, session: &str) {
        if self.vad.worker.lock().await.is_none() {
            return;
        }
        let _ = self
            .vad
            .request(json!({"op":"reset","session_id":session}))
            .await;
    }
    pub async fn transcribe(
        &self,
        samples: &[i16],
        partial: bool,
        threads: usize,
    ) -> Result<Value> {
        self.stt
            .request(json!({"pcm":encode_pcm(samples),"partial":partial,"threads":threads}))
            .await
    }
    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
        threads: usize,
    ) -> Result<(Vec<i16>, u32)> {
        let v = self.tts.request(json!({"text":text,"voice":voice,"language":language,"speed":speed,"threads":threads})).await?;
        let bytes = STANDARD
            .decode(v["pcm"].as_str().unwrap_or_default())
            .map_err(|e| SailError::Inference(e.to_string()))?;
        Ok((
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect(),
            v["sample_rate"].as_u64().unwrap_or(24000) as u32,
        ))
    }
}
pub fn encode_pcm(samples: &[i16]) -> String {
    STANDARD.encode(
        samples
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}
