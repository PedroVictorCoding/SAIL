//! Conversation adapter for the shared typed execution scheduler.
use async_trait::async_trait;
use sail_agents::tools::{Tool, ToolContext, ToolFailure, ToolRegistry, ToolScheduler};
use sail_guardian::{Capability, SystemCapability};
use sail_protocol::execution::{ToolDescriptor, ToolEffect};
use serde_json::{Value, json};
use std::sync::Arc;
struct Kernel;
#[async_trait]
impl Tool for Kernel {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        if tool == "os:status" {
            Capability::System(SystemCapability::OsTelemetry)
        } else {
            ToolScheduler::capability(tool, input)
        }
    }
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        // Registry fixes the tool name; callers cannot select a different adapter.
        let call: crate::KernelToolCall =
            serde_json::from_value(input).map_err(|e| ToolFailure {
                message: e.to_string(),
                retryable: false,
            })?;
        let result = crate::LocalKernelToolBroker::execute_scheduled(&call);
        if result.success {
            Ok(Value::String(result.output.to_string()))
        } else {
            Err(ToolFailure {
                message: result.output.to_string(),
                retryable: false,
            })
        }
    }
}
struct NamedKernel(&'static str);
#[async_trait]
impl Tool for NamedKernel {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        Kernel.capability(tool, input)
    }
    async fn execute(&self, context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        Kernel
            .execute(context, json!({"tool":self.0,"params":input}))
            .await
    }
}
struct PulseTool(&'static str);
#[async_trait]
impl Tool for PulseTool {
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        let pulse = sail_pulse::global().ok_or_else(|| ToolFailure {
            message: "Pulse reminders are not available in this session".into(),
            retryable: false,
        })?;
        execute_pulse_tool(&pulse, self.0, input)
            .map(|value| Value::String(value.to_string()))
            .map_err(|message| ToolFailure {
                message,
                retryable: false,
            })
    }
}

/// The owner's Spotify account (sail-bridge). The voice turn plays `spotify:find`'s best
/// match straight away; `spotify:play` plays a URI the model chose.
struct SpotifyTool(&'static str);
#[async_trait]
impl Tool for SpotifyTool {
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        execute_spotify_tool(&sail_bridge::spotify::global(), self.0, &input)
            .await
            .map(|value| Value::String(value.to_string()))
            .map_err(|message| ToolFailure {
                message,
                retryable: false,
            })
    }
}
/// Read-only Moodle tools over the local store (`sail-moodle`). They answer from the last
/// sync and never call Moodle.
struct MoodleTool(&'static str);
#[async_trait]
impl Tool for MoodleTool {
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        let service = sail_moodle::service::global().ok_or_else(|| ToolFailure {
            message: "Moodle is not available in this session".into(),
            retryable: false,
        })?;
        crate::moodle_tools::execute(&service, self.0, &input)
            .map(|value| Value::String(value.to_string()))
            .map_err(|message| ToolFailure {
                message,
                retryable: false,
            })
    }
}
/// How many offered alternatives the model sees besides the best match.
const SPOTIFY_ALTERNATIVES: usize = 4;
pub(crate) async fn execute_spotify_tool(
    spotify: &sail_bridge::spotify::Spotify,
    tool: &str,
    input: &Value,
) -> Result<Value, String> {
    use sail_bridge::spotify::{Kind, score};
    let err = |e: sail_bridge::spotify::SpotifyError| e.message;
    match tool {
        "spotify:find" => {
            let query = input["query"].as_str().unwrap_or_default().trim();
            if query.is_empty() {
                return Err("query is required".into());
            }
            let kind = input["kind"]
                .as_str()
                .and_then(Kind::parse)
                .ok_or("kind must be playlist, song, album or artist")?;
            let mine = input["mine"].as_bool() == Some(true) && kind == Kind::Playlist;
            let found = if mine {
                let all = spotify.my_playlists().await.map_err(err)?;
                let mut ranked: Vec<_> = all
                    .iter()
                    .map(|c| (score(query, &c.name), c.clone()))
                    .filter(|(s, _)| *s > 0)
                    .collect();
                ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
                if ranked.is_empty() {
                    if all.is_empty() {
                        return Err("The owner's Spotify account has no playlists.".into());
                    }
                    // Names can be puns or in another language ("Cabeça de Radio" for
                    // Radiohead): let the model choose from the owner's own list.
                    return Ok(json!({
                        "matched": false,
                        "playlists": all,
                        "instruction": "No playlist name contains those words. Choose the one playlist that best fits the request (names can be puns or in another language) and call spotify_play with its uri now.",
                    }));
                }
                ranked.into_iter().map(|(_, c)| c).collect::<Vec<_>>()
            } else {
                spotify.search(query, kind, 5).await.map_err(err)?
            };
            let mut found = found.into_iter();
            let Some(best) = found.next() else {
                return Err(format!(
                    "Spotify found no {} matching \"{query}\".",
                    input["kind"].as_str().unwrap_or("item")
                ));
            };
            let alternatives: Vec<_> = found.take(SPOTIFY_ALTERNATIVES).collect();
            Ok(json!({
                "matched": true,
                "best": best,
                "alternatives": alternatives,
            }))
        }
        "spotify:play" => {
            let uri = input["uri"].as_str().unwrap_or_default();
            let played = spotify.play(uri).await.map_err(err)?;
            Ok(
                json!({"message": format!("Playing on {}.", played["device"].as_str().unwrap_or("Spotify")), "result": played}),
            )
        }
        "spotify:control" => {
            let action = input["action"].as_str().unwrap_or_default();
            if action != "status" {
                spotify.control(action).await.map_err(err)?;
            }
            let now = spotify.now_playing().await.map_err(err)?;
            Ok(json!({"action": action, "now_playing": now}))
        }
        other => Err(format!("unknown Spotify tool {other}")),
    }
}
struct Command;
#[async_trait]
impl Tool for Command {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        let mut reviewed = input.clone();
        reviewed["limits"] = json!(sail_protocol::execution::CommandLimits::default());
        ToolScheduler::capability(tool, &reviewed)
    }
    async fn execute(&self, context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        let socket = std::env::var_os("SAIL_RUNNER_SOCKET").ok_or_else(|| ToolFailure {
            message: "Isolated command runner is not configured".into(),
            retryable: false,
        })?;
        let uid: u32 = std::env::var("SAIL_RUNNER_UID")
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| ToolFailure {
                message: "Runner service UID is not configured".into(),
                retryable: false,
            })?;
        let plan = sail_protocol::execution::CommandPlan {
            version: 1,
            request_id: context.request_id,
            correlation_id: context.correlation_id,
            executable: input["executable"].as_str().unwrap_or_default().into(),
            argv: input["argv"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            limits: sail_protocol::execution::CommandLimits::default(),
        };
        plan.validate().map_err(|message| ToolFailure {
            message,
            retryable: false,
        })?;
        let result = tokio::task::spawn_blocking(move || {
            sail_runner::dispatch_authorized(
                std::path::Path::new(&socket),
                uid,
                &plan,
                context.cancel,
            )
        })
        .await
        .map_err(|e| ToolFailure {
            message: e.to_string(),
            retryable: false,
        })?
        .map_err(|e| ToolFailure {
            message: e.to_string(),
            retryable: false,
        })?;
        if result.status != sail_protocol::execution::CommandStatus::Completed {
            return Err(ToolFailure {
                message: serde_json::to_string(&result).unwrap_or_default(),
                retryable: false,
            });
        }
        Ok(Value::String(
            serde_json::to_string(&result).unwrap_or_default(),
        ))
    }
}
struct Canvas;
#[async_trait]
impl Tool for Canvas {
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        Ok(Value::String(input.to_string()))
    }
}
/// Dialogue's tools as the model sees them, in the order the voice tool list offers them
/// (ties in tool selection keep this order). `vision_frame` and `memory_recall` are not
/// here: they act on the current turn's image and archive, not through the scheduler.
pub(crate) const MODEL_TOOL_ORDER: [&str; 28] = [
    "os:status",
    "proc:list",
    "fs:query",
    "canvas:publish",
    "local:time",
    "command:execute",
    "fs:read",
    "fs:write",
    "github:issues",
    "web:speak",
    "web:read",
    "studio:audio_analyze",
    "pulse:create",
    "pulse:list",
    "pulse:cancel",
    "pulse:snooze",
    "spotify:find",
    "spotify:play",
    "spotify:control",
    "moodle:upcoming",
    "moodle:changes",
    "moodle:course",
    "moodle:search",
    "forge:enqueue",
    "forge:status",
    "forge:list",
    "forge:cancel",
    "forge:clear",
];

/// Id, description, input properties, required inputs, effect, concurrency limit.
type KernelTool = (
    &'static str,
    &'static str,
    Value,
    &'static [&'static str],
    ToolEffect,
    usize,
);

fn string(max: u64, description: &str) -> Value {
    json!({"type":"string","maxLength":max,"description":description})
}

/// Every Dialogue tool: one registry is the source of descriptions, schemas, effects and
/// limits for voice turns, tool plans and agent task plans alike.
pub fn registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::default());
    sail_agents::tools::register_builtin_time(&registry).expect("time descriptor");
    let path = || {
        string(
            4096,
            "Path relative to the workspace root; dot (.) is the root",
        )
    };
    let job = || {
        string(
            128,
            "The job id or issue number (e.g. 5, #5, job-5, or job-5-61c992ad)",
        )
    };
    let kernel: [KernelTool; 15] = [
        (
            "os:status",
            "Read local OS memory, uptime and load",
            json!({}),
            &[],
            ToolEffect::ReadOnly,
            4,
        ),
        (
            "proc:list",
            "List local processes",
            json!({"limit":{"type":"integer","minimum":1,"maximum":100}}),
            &[],
            ToolEffect::ReadOnly,
            4,
        ),
        (
            "fs:query",
            "Inspect a file or directory inside the authorized workspace. Use relative paths; dot (.) means the workspace root. Never probe outside paths or retry a denied query.",
            json!({"path":path()}),
            &[],
            ToolEffect::ReadOnly,
            4,
        ),
        (
            "fs:read",
            "Read text contents of a file inside the authorized workspace.",
            json!({"path":path()}),
            &["path"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "fs:write",
            "Create or write a file inside the authorized workspace. Requires explicit user approval.",
            json!({"path":path(),"content":string(32700, "File content to write")}),
            &["path", "content"],
            ToolEffect::Mutating,
            2,
        ),
        (
            "github:issues",
            "Retrieve open GitHub issues ordered by priority, or fetch full details of a specific issue by number or URL",
            json!({
                "issue_number":{"type":"integer","description":"Specific GitHub issue number to fetch full title, body, state and labels for"},
                "url":string(1024, "Specific GitHub issue URL (e.g. https://github.com/owner/repo/issues/125)"),
                "limit":{"type":"integer","minimum":1,"maximum":50},
                "priority":{"type":"string","maxLength":2,"enum":["P0","P1","P2","P3","P4","p0","p1","p2","p3","p4"]}
            }),
            &[],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "web:speak",
            "Read a website or web page to the user (e.g. 'Read me this website: https://...', 'Read this page for me: https://...'). Fetches the page, extracts its important readable content, and streams it directly to voice synthesis with a short introductory context_phrase so the user hears it immediately without waiting for full model generation.",
            json!({
                "url":string(2048, "The HTTP or HTTPS URL of the page to read aloud"),
                "context_phrase":string(256, "A short introductory phrase to speak before reading the content, e.g. 'Reading Roko's Basilisk from LessWrong:'"),
                "max_paragraphs":{"type":"integer","minimum":1,"maximum":20,"description":"Maximum number of paragraphs to read aloud (defaults to 6)"}
            }),
            &["url"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "web:read",
            "Fetch the text contents of a public web page or documentation for silent inspection/analysis when the user does not ask to have the page read to them",
            json!({"url":string(2048, "The HTTP or HTTPS URL to fetch and read")}),
            &["url"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "studio:audio_analyze",
            "Measure a local audio file for Studio: integrated loudness (LUFS, ITU-R BS.1770-4 gated), true-peak estimate, loudness range, frequency balance, arrangement energy and tempo, with concrete mixing recommendations and a chart ready to display.",
            json!({
                "file_path":string(1024, "Path to the audio file inside the authorized workspace"),
                "genre_hint":string(64, "Optional genre such as electronic, hip-hop, pop or rock"),
                "target_lufs":{"type":"number","minimum":-30,"maximum":-3,"description":"Optional loudness target; defaults to the genre or -14 LUFS"}
            }),
            &["file_path"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "forge:enqueue",
            "Enqueue an autonomous coding task or GitHub issue for Forge to solve in an isolated sandbox with automated testing, review, and PR drafting.",
            json!({
                "issue_number":{"type":"integer","minimum":1,"description":"GitHub issue number to solve"},
                "repo":string(512, "Repository name, defaults to PedroVictorCoding/SAIL"),
                "task":string(12000, "Optional description or instructions for the task"),
                "body":string(12000, "Optional issue body or extra context")
            }),
            &[],
            ToolEffect::Mutating,
            2,
        ),
        (
            "forge:status",
            "Check the status, current step, test outcome, and diff of a Forge autonomous coding job. Accepts exact job IDs (e.g. job-5-61c992ad), prefix (e.g. job-5), or issue numbers (e.g. 5 or #5).",
            json!({"job_id":job()}),
            &["job_id"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "forge:list",
            "List all active, recent, and stored Forge autonomous coding jobs and tasks.",
            json!({}),
            &[],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "forge:cancel",
            "Cancel or stop a running or in-progress Forge autonomous coding task.",
            json!({"job_id":job(),"reason":string(2048, "Optional cancellation reason")}),
            &["job_id"],
            ToolEffect::Mutating,
            2,
        ),
        (
            "forge:clear",
            "Remove, clear, or archive blocked, cancelled, or completed Forge coding jobs from the active queue and delete their local checkpoints. Pass 'all' to clear all finished/blocked jobs, or specify a job_id.",
            json!({"job_id":string(128, "Specific job ID to remove, or 'all' to clear all finished and blocked jobs (defaults to 'all')")}),
            &[],
            ToolEffect::Mutating,
            2,
        ),
        (
            "canvas:publish",
            "Display a diagram or structured visual beside the conversation",
            json!({
                "artifact_type":{"type":"string","enum":["mermaid","json","svg"],"maxLength":16},
                "content":{"type":"string","maxLength":32700}
            }),
            &["artifact_type", "content"],
            ToolEffect::ReadOnly,
            4,
        ),
    ];
    for (id, description, properties, required, effect, concurrency_limit) in kernel {
        let canvas = id == "canvas:publish";
        let backend: Arc<dyn Tool> = if canvas {
            Arc::new(Canvas)
        } else {
            Arc::new(NamedKernel(id))
        };
        registry
            .register(
                ToolDescriptor {
                    id: id.into(),
                    version: 1,
                    description: description.into(),
                    input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
                    output_schema: json!({"type":"string","maxLength":if canvas {65536} else {1048576}}),
                    security_effect: effect,
                    required_capabilities: vec![id.into()],
                    timeout_ms: if canvas { 1000 } else { 120000 },
                    resource_class: if canvas { "telemetry" } else { "io" }.into(),
                    concurrency_limit,
                },
                backend,
            )
            .unwrap_or_else(|e| panic!("{id} descriptor: {e}"));
    }
    let pulse_tools: [KernelTool; 4] = [
        (
            "pulse:create",
            "Create a local one-time reminder. Convert the requested time to a concrete RFC 3339 due_at. If the time is ambiguous, call with preview=true first and show the interpreted local time before saving.",
            json!({
                "text": string(2000, "What to remind the user about"),
                "due_at": string(64, "Due time as an RFC 3339 timestamp, e.g. 2026-11-04T18:30:00Z"),
                "timezone": string(64, "Optional IANA time zone; defaults to the user's saved zone"),
                "utc_offset_minutes": {"type":"integer","minimum":-840,"maximum":840,"description":"Optional local offset from UTC in minutes"},
                "preview": {"type":"boolean","description":"When true, show the interpreted time without saving"}
            }),
            &["text", "due_at"],
            ToolEffect::Mutating,
            2,
        ),
        (
            "pulse:list",
            "List the user's active local reminders with their ids, due times, status and snooze count.",
            json!({"include_finished": {"type":"boolean","description":"Also include recently delivered and cancelled reminders"}}),
            &[],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "pulse:cancel",
            "Cancel a pending or due local reminder by id.",
            json!({"id": string(64, "Reminder id from pulse:list")}),
            &["id"],
            ToolEffect::Mutating,
            2,
        ),
        (
            "pulse:snooze",
            "Move a local reminder to a later time. Provide either minutes or an RFC 3339 until timestamp.",
            json!({
                "id": string(64, "Reminder id from pulse:list"),
                "minutes": {"type":"integer","minimum":1,"maximum":10080,"description":"Minutes from now"},
                "until": string(64, "New due time as an RFC 3339 timestamp")
            }),
            &["id"],
            ToolEffect::Mutating,
            2,
        ),
    ];
    for (id, description, properties, required, effect, concurrency_limit) in pulse_tools {
        registry
            .register(
                ToolDescriptor {
                    id: id.into(),
                    version: 1,
                    description: description.into(),
                    input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
                    output_schema: json!({"type":"string","maxLength":65536}),
                    security_effect: effect,
                    required_capabilities: vec![id.into()],
                    timeout_ms: 5000,
                    resource_class: "io".into(),
                    concurrency_limit,
                },
                Arc::new(PulseTool(id)),
            )
            .unwrap_or_else(|e| panic!("{id} descriptor: {e}"));
    }

    let spotify_tools: [KernelTool; 3] = [
        (
            "spotify:find",
            "Find music to play on the user's Spotify: a playlist (set mine=true for the user's own or followed playlists, e.g. 'my radiohead playlist'), song, album or artist. The best match starts playing at once; returns its full name and creator.",
            json!({
                "query": string(200, "What to look for, e.g. 'radiohead' or 'how to disappear completely'"),
                "kind": {"type":"string","maxLength":16,"enum":["playlist","song","album","artist"],"description":"What the user asked to play"},
                "mine": {"type":"boolean","description":"Search only the user's own and followed playlists"}
            }),
            &["query", "kind"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "spotify:play",
            "Play music on Spotify now: a uri from spotify_find, e.g. the playlist you chose from the owner's list or an alternative the user named.",
            json!({"uri": string(128, "spotify:playlist:…, spotify:track:…, spotify:album:… or spotify:artist:… from spotify_find")}),
            &["uri"],
            ToolEffect::Mutating,
            1,
        ),
        (
            "spotify:control",
            "Control Spotify music playback: pause, resume, skip to the next or previous song, or report what is playing now (status).",
            json!({"action": {"type":"string","maxLength":16,"enum":["pause","resume","next","previous","status"]}}),
            &["action"],
            ToolEffect::Mutating,
            2,
        ),
    ];
    for (id, description, properties, required, effect, concurrency_limit) in spotify_tools {
        registry
            .register(
                ToolDescriptor {
                    id: id.into(),
                    version: 1,
                    description: description.into(),
                    input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
                    output_schema: json!({"type":"string","maxLength":65536}),
                    security_effect: effect,
                    required_capabilities: vec![id.into()],
                    timeout_ms: 25_000,
                    resource_class: "io".into(),
                    concurrency_limit,
                },
                Arc::new(SpotifyTool(id)),
            )
            .unwrap_or_else(|e| panic!("{id} descriptor: {e}"));
    }

    let course = || {
        string(
            120,
            "Optional course: its name, a fragment of it (e.g. 'cálculo') or its short name",
        )
    };
    let moodle_tools: [KernelTool; 4] = [
        (
            "moodle:upcoming",
            "List the user's upcoming school exams, tests, deadlines and other dated items from their Moodle course pages (last sync), with how sure SAIL is and the quote each came from. Portuguese: exame, teste, frequência, entrega, prazo, quando, próximo, escola, cadeira, disciplina.",
            json!({
                "days": {"type":"integer","minimum":1,"maximum":366,"description":"How many days ahead; default 30"},
                "course": course()
            }),
            &[],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "moodle:changes",
            "What changed on the user's school Moodle recently: new or updated files, moved deadlines, announcements, released grades. Portuguese: novidades, avisos, ficheiros, notas, mudou.",
            json!({
                "since_days": {"type":"integer","minimum":1,"maximum":180,"description":"How many days back; default 7"},
                "course": course()
            }),
            &[],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "moodle:course",
            "Everything SAIL knows about one school Moodle course: how it is graded (weights, minimum grade), dated items, released grades and recent changes. Portuguese: avaliação, nota, cadeira, disciplina, UC.",
            json!({"course": course()}),
            &["course"],
            ToolEffect::ReadOnly,
            2,
        ),
        (
            "moodle:search",
            "Search the user's school Moodle items and changes (titles and source quotes) for a word or phrase. Portuguese: procurar.",
            json!({"query": string(100, "What to look for, e.g. 'projeto' or 'recurso'"), "course": course()}),
            &["query"],
            ToolEffect::ReadOnly,
            2,
        ),
    ];
    for (id, description, properties, required, effect, concurrency_limit) in moodle_tools {
        registry
            .register(
                ToolDescriptor {
                    id: id.into(),
                    version: 1,
                    description: description.into(),
                    input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
                    output_schema: json!({"type":"string","maxLength":65536}),
                    security_effect: effect,
                    required_capabilities: vec![id.into()],
                    timeout_ms: 5000,
                    resource_class: "io".into(),
                    concurrency_limit,
                },
                Arc::new(MoodleTool(id)),
            )
            .unwrap_or_else(|e| panic!("{id} descriptor: {e}"));
    }

    registry.register(ToolDescriptor {
        id:"command:execute".into(),version:1,description:"Execute a command in the isolated no-network runner after exact owner approval. Never host shell execution. Runner may be unavailable.".into(),
        input_schema:json!({"type":"object","properties":{"executable":{"type":"string","maxLength":4096},"argv":{"type":"array","maxItems":128,"items":{"type":"string","maxLength":8192}}},"required":["executable","argv"],"additionalProperties":false}),
        output_schema:json!({"type":"string","maxLength":1048576}),security_effect:ToolEffect::Sandbox,
        required_capabilities:vec!["command:execute".into()],timeout_ms:120000,resource_class:"sandbox".into(),concurrency_limit:1,
    },Arc::new(Command)).expect("command descriptor");
    registry
}

/// The registry's tools as OpenAI-style function definitions (`fs:read` becomes `fs_read`).
pub(crate) fn model_tool_definitions() -> Vec<Value> {
    static DEFINITIONS: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    DEFINITIONS
        .get_or_init(|| {
            let registry = registry();
            MODEL_TOOL_ORDER
                .iter()
                .map(|id| {
                    let tool = registry.descriptor(id).expect("ordered tool is registered");
                    json!({"type":"function","function":{"name":id.replace(':', "_"),
                        "description":tool.description,"parameters":tool.input_schema}})
                })
                .collect()
        })
        .clone()
}

pub fn scheduler(broker: Arc<sail_guardian::GuardianExecutionBroker>) -> Arc<ToolScheduler> {
    Arc::new(ToolScheduler::new(registry(), broker, 4))
}

pub fn shared() -> Result<Arc<ToolScheduler>, sail_guardian::ExecutionDenied> {
    static SCHEDULER: std::sync::OnceLock<Arc<ToolScheduler>> = std::sync::OnceLock::new();
    if let Some(scheduler) = SCHEDULER.get() {
        return Ok(scheduler.clone());
    }
    let instance = scheduler(sail_guardian::global_broker()?);
    Ok(SCHEDULER.get_or_init(|| instance).clone())
}

/// Execute one Pulse tool against the process-wide store and return a JSON observation.
fn execute_pulse_tool(
    pulse: &sail_pulse::Pulse,
    tool: &str,
    input: Value,
) -> Result<Value, String> {
    let settings = pulse.settings().map_err(|error| error.to_string())?;
    match tool {
        "pulse:create" => {
            let text = input["text"].as_str().unwrap_or_default();
            let due_text = input["due_at"].as_str().unwrap_or_default();
            let due_at = chrono::DateTime::parse_from_rfc3339(due_text)
                .map_err(|_| "due_at must be an RFC 3339 timestamp with an offset".to_string())?
                .with_timezone(&chrono::Utc);
            let timezone = input["timezone"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&settings.timezone)
                .to_owned();
            let utc_offset_minutes = input["utc_offset_minutes"]
                .as_i64()
                .map(|value| value as i32)
                .unwrap_or(settings.utc_offset_minutes);
            let new = sail_pulse::NewReminder::new(text, due_at, timezone, utc_offset_minutes)
                .map_err(|error| error.to_string())?;
            let local = due_at.with_timezone(
                &chrono::FixedOffset::east_opt(utc_offset_minutes * 60)
                    .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC offset")),
            );
            let interpreted = json!({
                "text": new.text,
                "due_at": due_at,
                "due_local": local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
                "timezone": new.timezone,
            });
            if input["preview"].as_bool() == Some(true) {
                return Ok(json!({
                    "preview": true,
                    "saved": false,
                    "interpreted": interpreted,
                    "message": format!("This reminder would be set for {}. Confirm to save it.", interpreted["due_local"]),
                }));
            }
            let reminder = pulse.create(new).map_err(|error| error.to_string())?;
            Ok(json!({
                "saved": true,
                "reminder": reminder,
                "message": format!("Reminder saved for {}.", interpreted["due_local"]),
            }))
        }
        "pulse:list" => {
            let reminders = pulse
                .list_active(sail_pulse::DEFAULT_LIST_LIMIT)
                .map_err(|error| error.to_string())?;
            let mut output = json!({"reminders": reminders, "count": reminders.len()});
            if input["include_finished"].as_bool() == Some(true) {
                output["history"] = json!(
                    pulse
                        .list_history(sail_pulse::DEFAULT_HISTORY_LIMIT)
                        .map_err(|error| error.to_string())?
                );
            }
            Ok(output)
        }
        "pulse:cancel" => {
            let id = input["id"]
                .as_str()
                .ok_or_else(|| "id is required".to_string())?;
            let reminder = pulse.cancel(id).map_err(|error| error.to_string())?;
            Ok(
                json!({"reminder": reminder, "message": format!("Reminder {} cancelled.", reminder.id)}),
            )
        }
        "pulse:snooze" => {
            let id = input["id"]
                .as_str()
                .ok_or_else(|| "id is required".to_string())?;
            let until = match (
                input["minutes"].as_u64(),
                input["until"]
                    .as_str()
                    .filter(|value| !value.trim().is_empty()),
            ) {
                (Some(minutes), None) if (1..=10_080).contains(&minutes) => {
                    chrono::Utc::now() + chrono::Duration::minutes(minutes as i64)
                }
                (Some(_), None) => return Err("minutes must be between 1 and 10080".into()),
                (None, Some(until)) => chrono::DateTime::parse_from_rfc3339(until)
                    .map_err(|_| "until must be an RFC 3339 timestamp".to_string())?
                    .with_timezone(&chrono::Utc),
                _ => return Err("provide either minutes or until, not both".into()),
            };
            let reminder = pulse.snooze(id, until).map_err(|error| error.to_string())?;
            Ok(
                json!({"reminder": reminder, "message": format!("Reminder snoozed until {}.", reminder.due_at)}),
            )
        }
        other => Err(format!("Unknown Pulse tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use sail_common::types::SubsystemId;
    use sail_guardian::{
        ApprovalBroker, AuditLedger, GuardianExecutionBroker, PolicyEngine, SandboxPolicy,
    };
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;
    #[tokio::test]
    async fn owner_http_approval_resumes_real_registered_tool_and_caches_its_result() {
        let broker = Arc::new(GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::new(AuditLedger::new()),
            Arc::new(ApprovalBroker::new()),
            Arc::new(SandboxPolicy::new()),
        ));
        let scheduler = scheduler(broker.clone());
        let mut events = scheduler.subscribe();
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            actor: SubsystemId::Dialogue,
            cancel: CancellationToken::new(),
        };
        let scheduled = scheduler.clone();
        let next = context.clone();
        let operation = tokio::spawn(async move {
            scheduled
                .execute(next, "proc:list", json!({"limit":1}), None, true)
                .await
        });
        let session = loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let Some(session) = event.approval_session {
                assert_eq!(event.request_id, context.request_id);
                break session;
            }
        };
        assert!(!operation.is_finished());
        let secret = "test-owner-token-with-more-than-32-bytes";
        let app = crate::approval_http::router(
            Some(sail_guardian::authority::ApprovalAuthority::new(secret).unwrap()),
            Some(broker.clone()),
        );
        let body = json!({"session_id":session,"approved":true}).to_string();
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resolve")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert!(!operation.is_finished());
        let accepted = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resolve")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let observed = tokio::time::timeout(std::time::Duration::from_secs(2), operation)
            .await
            .unwrap()
            .unwrap();
        assert!(observed.success, "{:?}", observed.error);
        let data: Value = serde_json::from_str(observed.data.as_str().unwrap()).unwrap();
        assert!(data["count"].as_u64().unwrap() <= 1);
        let replay = scheduler
            .execute(context, "proc:list", json!({"limit":1}), None, true)
            .await;
        assert_eq!(replay.id, observed.id);
        assert_eq!(
            broker
                .audit()
                .list(100)
                .iter()
                .filter(|a| a.outcome == sail_guardian::DecisionOutcome::Granted)
                .count(),
            1
        );
    }
    /// Spotify tools against a local stand-in for the Spotify Web API.
    #[tokio::test]
    async fn spotify_find_offers_the_owner_playlists_and_play_starts_at_once() {
        use axum::{Json, Router, routing::get};
        let played = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let sink = played.clone();
        let api = Router::new()
            .route("/v1/me/playlists", get(|| async { Json(json!({"next":null,"items":[
                {"name":"Cabeça de Radio","uri":"spotify:playlist:aaa","owner":{"display_name":"Pedro Victor"},"description":"OK Computer to In Rainbows","tracks":{"total":42}},
                {"name":"Lo-fi beats","uri":"spotify:playlist:bbb","owner":{"display_name":"Spotify"},"description":"","tracks":{"total":90}}]})) }))
            .route("/v1/me/player/devices", get(|| async { Json(json!({"devices":[{"id":"phone1","name":"Pedro's phone","is_active":false,"is_restricted":false}]})) }))
            .route("/v1/me/player/play", axum::routing::put(move |uri: axum::extract::RawQuery, Json(body): Json<Value>| {
                let sink = sink.clone();
                async move { sink.lock().unwrap().push(json!({"query":uri.0,"body":body})); axum::http::StatusCode::NO_CONTENT }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("sail-spotify-tool-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("token.json"),
            json!({"access_token":"a","refresh_token":"r","expires_at":u64::MAX/2}).to_string(),
        )
        .unwrap();
        let spotify = sail_bridge::spotify::Spotify::with_endpoints(dir.clone(), &base, &base);

        // "my radiohead playlist": no name contains the word, so SAIL sees the owner's list.
        let found = execute_spotify_tool(
            &spotify,
            "spotify:find",
            &json!({"query":"radiohead","kind":"playlist","mine":true}),
        )
        .await
        .unwrap();
        assert_eq!(found["matched"], false);
        assert_eq!(found["playlists"][0]["name"], "Cabeça de Radio");
        assert_eq!(found["playlists"][0]["by"], "Pedro Victor");
        // A word that is in a name gives a best match with its full name and creator.
        let found = execute_spotify_tool(
            &spotify,
            "spotify:find",
            &json!({"query":"cabeça de radio","kind":"playlist","mine":true}),
        )
        .await
        .unwrap();
        assert_eq!(found["best"]["name"], "Cabeça de Radio");
        assert_eq!(found["best"]["by"], "Pedro Victor");
        assert_eq!(found["best"]["uri"], "spotify:playlist:aaa");

        // Nothing plays until spotify:play is called; then it plays on the available device.
        assert!(played.lock().unwrap().is_empty());
        let ok = execute_spotify_tool(
            &spotify,
            "spotify:play",
            &json!({"uri":"spotify:playlist:aaa"}),
        )
        .await
        .unwrap();
        assert_eq!(ok["message"], "Playing on Pedro's phone.");
        assert_eq!(played.lock().unwrap()[0]["query"], "device_id=phone1");
        assert_eq!(
            played.lock().unwrap()[0]["body"]["context_uri"],
            "spotify:playlist:aaa"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn the_voice_model_is_offered_exactly_the_registered_tools() {
        let registered: std::collections::BTreeSet<String> =
            registry().descriptors().into_iter().map(|d| d.id).collect();
        let offered: std::collections::BTreeSet<String> =
            MODEL_TOOL_ORDER.iter().map(|id| id.to_string()).collect();
        assert_eq!(registered, offered);
        for tool in model_tool_definitions() {
            let name = tool["function"]["name"].as_str().unwrap();
            let id = MODEL_TOOL_ORDER
                .iter()
                .find(|id| id.replace(':', "_") == name)
                .unwrap();
            assert_eq!(
                tool["function"]["parameters"],
                registry().descriptor(id).unwrap().input_schema,
                "{id}: the model sees the schema the scheduler enforces"
            );
            assert!(
                !tool["function"]["description"]
                    .as_str()
                    .unwrap()
                    .starts_with("Local ")
            );
        }
    }
    #[tokio::test]
    async fn actual_clock_and_canvas_use_registered_bounded_tools() {
        let broker = Arc::new(GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::new(AuditLedger::new()),
            Arc::new(ApprovalBroker::new()),
            Arc::new(SandboxPolicy::new()),
        ));
        let scheduler = scheduler(broker);
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            actor: SubsystemId::Dialogue,
            cancel: CancellationToken::new(),
        };
        let clock = scheduler
            .execute(context.clone(), "local:time", json!({}), None, true)
            .await;
        assert!(clock.success);
        assert!(clock.data["unix_ms"].as_u64().unwrap() > 0);
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            ..context
        };
        let canvas = scheduler
            .execute(
                context,
                "canvas:publish",
                json!({"artifact_type":"mermaid","content":"graph LR; A-->B"}),
                None,
                true,
            )
            .await;
        assert!(canvas.success);
    }
}
