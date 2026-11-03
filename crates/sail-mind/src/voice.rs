//! Local speech model access. Workers are persistent, bounded and isolated by engine.
use base64::{Engine, engine::general_purpose::STANDARD};
use sail_common::error::{Result, SailError};
use serde_json::{Value, json};
use std::time::Instant;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, Semaphore},
};
use tracing::{Instrument, debug, error, info, trace, warn};

struct Worker {
    child: Child,
    cancel: tokio_util::sync::CancellationToken,
    lease: sail_common::resources::ResourceLease,
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
            capacity: Arc::new(Semaphore::new(if mode == "speaker" { 1 } else { 8 })),
        }
    }
    async fn request(&self, request: Value) -> Result<Value> {
        let permit = self.capacity.clone().try_acquire_owned().map_err(|_| {
            warn!(engine = self.mode, "Speech worker queue is full");
            SailError::Inference(format!("{} worker queue full", self.mode))
        })?;
        let this = self.clone();
        let span = tracing::Span::current();
        let queued = Instant::now();
        if self.mode == "vad" {
            trace!(engine = self.mode, "Queued VAD worker request");
        } else {
            debug!(engine = self.mode, "Queued speech worker request");
        }
        // The task drains the response even when its caller is cancelled, preserving framing.
        tokio::spawn(async move {
            let _permit = permit;
            // Draining an accepted speech request retains voice priority even if its
            // original turn is aborted; the worker still consumes compute.
            let _voice = (!matches!(this.mode,"vad"|"speaker")).then(|| sail_common::resources::global().voice_turn());
            let mut slot = this.worker.lock().await;
            let started=Instant::now();
            if this.mode!="vad"{debug!(engine=this.mode,queue_wait_ms=queued.elapsed().as_millis(),"Acquired speech worker; sending request");}
            if slot.is_none() {
                let python =
                    std::env::var(if this.mode=="speaker" {"SAIL_SPEAKER_PYTHON"} else {"SAIL_VOICE_PYTHON"}).unwrap_or_else(|_|if this.mode=="speaker" {".venv-speaker/bin/python".into()} else {".venv-voice/bin/python".into()});
                let script =
                    if this.mode=="speaker" {"scripts/voice/speaker_worker.py".into()} else {std::env::var("SAIL_VOICE_WORKER").unwrap_or("scripts/voice/worker.py".into())};
                info!(engine=this.mode,%python,%script,"Starting persistent offline speech worker");
                let cancel=tokio_util::sync::CancellationToken::new();
                let lease = sail_common::resources::global().try_admit(
                    sail_common::resources::ResourceClaim {
                        kind: sail_protocol::resources::WorkKind::Speech,
                        priority: if this.mode=="speaker" {sail_protocol::resources::WorkPriority::Background} else {sail_protocol::resources::WorkPriority::Voice},
                        ram_bytes: match this.mode { "stt" => 1024, "tts" => 512, "speaker" => 3072, _ => 128 } * sail_common::resources::MIB,
                        cpu_slots: if this.mode=="speaker" {2} else {0},
                        heavy_model: false,
                    }, cancel.clone(),
                ).map_err(|e| SailError::Inference(e.to_string()))?;
                let mut child = Command::new(python)
                    .arg(script)
                    .arg(this.mode)
                    .env("HF_HUB_OFFLINE", "1")
                    .env("TRANSFORMERS_OFFLINE", "1")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| {
                        SailError::Inference(format!(
                            "{} worker unavailable: {e}; run scripts/voice/setup.sh",
                            this.mode
                        ))
                    })?;
                let pid=child.id();
                info!(engine=this.mode,?pid,"Speech worker process spawned; loading model on first request");
                if let Some(stderr)=child.stderr.take(){
                    let mode=this.mode;
                    tokio::spawn(async move{
                        let mut lines=BufReader::new(stderr).lines();
                        loop{match lines.next_line().await{
                            Ok(Some(line))=>debug!(target:"sail_mind::worker",engine=mode,?pid,%line,"Speech worker output"),
                            Ok(None)=>{debug!(engine=mode,?pid,"Speech worker output closed");break;},
                            Err(error)=>{warn!(engine=mode,?pid,%error,"Failed to read speech worker diagnostics");break;}
                        }}
                    }.instrument(tracing::info_span!(parent:None,"speech_worker",engine=mode,?pid)));
                }
                *slot = Some(Worker {
                    lease,
                    cancel,
                    input: child.stdin.take().unwrap(),
                    output: BufReader::new(child.stdout.take().unwrap()),
                    child,
                });
            }
            let worker = slot.as_mut().unwrap();
            let cancelled=worker.cancel.clone();
            let operation = async {
                let mut data = serde_json::to_vec(&request)?;
                data.push(b'\n');
                worker.input.write_all(&data).await?;
                worker.input.flush().await?;
                let mut line = String::new();
                if (&mut worker.output).take(if this.mode=="speaker" {1_048_576} else {u64::MAX}).read_line(&mut line).await? == 0 || !line.ends_with('\n') {
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
            let result = tokio::select! {
                biased;
                _=cancelled.cancelled()=>Ok(Err(SailError::Inference("Speaker analysis preempted for live voice".into()))),
                result=tokio::time::timeout(Duration::from_secs(120),operation)=>result,
            };
            match result {
                Ok(Ok(value)) => {
                    worker.lease.settled();
                    if this.mode=="vad"{trace!(engine=this.mode,elapsed_us=started.elapsed().as_micros(),"VAD worker response received");}
                    else{debug!(engine=this.mode,elapsed_ms=started.elapsed().as_millis(),"Speech worker response received");}
                    if this.mode=="speaker" {if let Some(mut done)=slot.take(){let _=done.child.kill().await;}}
                    Ok(value)
                },
                error => {
                    error!(engine=this.mode,elapsed_ms=started.elapsed().as_millis(),failure=?error,"Speech worker request failed; discarding process before next request");
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
        }.instrument(span))
        .await
        .map_err(|e| SailError::Inference(e.to_string()))?
    }
}

#[derive(Clone)]
pub struct VoiceModelServices {
    vad: WorkerClient,
    stt: WorkerClient,
    tts: WorkerClient,
    speaker: WorkerClient,
    inference: Option<crate::MindService>,
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
            speaker: WorkerClient::new("speaker"),
            inference: None,
        }
    }
    pub async fn analyze_speakers(
        &self,
        samples: &[i16],
    ) -> Result<sail_protocol::speaker::SpeakerOutput> {
        if samples.len() < 16000 || samples.len() > sail_protocol::speaker::MAX_SAMPLES {
            return Err(SailError::Inference(
                "Speaker audio must be 1–60 seconds".into(),
            ));
        }
        let value = self
            .speaker
            .request(json!({"pcm":encode_pcm(samples)}))
            .await?;
        let output: sail_protocol::speaker::SpeakerOutput = serde_json::from_value(value)?;
        output
            .validate(samples.len() as u64 / 16)
            .map_err(SailError::Inference)?;
        Ok(output)
    }

    pub async fn warm_tts(&self, threads: usize) -> Result<()> {
        self.tts
            .request(json!({"op":"health","threads":threads}))
            .await?;
        Ok(())
    }
    pub fn with_inference(mut self, service: crate::MindService) -> Self {
        self.inference = Some(service);
        self
    }
    pub fn inference(&self) -> Option<crate::MindService> {
        self.inference.clone()
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
        let started = Instant::now();
        info!(
            samples = samples.len(),
            audio_ms = samples.len() / 16,
            partial,
            threads,
            "Running local Whisper transcription"
        );
        let result = self
            .stt
            .request(json!({"pcm":encode_pcm(samples),"partial":partial,"threads":threads}))
            .await?;
        info!(elapsed_ms=started.elapsed().as_millis(),partial,language=?result["language"],words=result["tokens"].as_array().map_or(0,Vec::len),"Whisper transcription completed");
        debug!(transcript=?result["text"],"Recognized speech hypothesis");
        Ok(result)
    }
    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
        threads: usize,
    ) -> Result<(Vec<i16>, u32)> {
        info!(
            voice,
            language,
            speed,
            threads,
            text_chars = text.chars().count(),
            "Requesting local Kokoro synthesis"
        );
        debug!(text, "Text submitted to speech synthesis");
        let started = Instant::now();
        let v = self.tts.request(json!({"text":text,"voice":voice,"language":language,"speed":speed,"threads":threads})).await?;
        let bytes = STANDARD
            .decode(v["pcm"].as_str().unwrap_or_default())
            .map_err(|e| SailError::Inference(e.to_string()))?;
        info!(elapsed_ms=started.elapsed().as_millis(),pcm_bytes=bytes.len(),sample_rate=?v["sample_rate"],"Kokoro synthesis completed");
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
