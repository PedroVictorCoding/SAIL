//! Local archive of the images that reach a conversation.
//!
//! Camera frames, screen captures and uploaded images were previously only present in the
//! turn that carried them. A follow-up such as "tell me more about that photo" then reached
//! a model that could no longer see it. This module keeps the bytes and a small metadata
//! record under the SAIL data root, so the model can find an image again by time, by this
//! conversation, or by labels derived from its caption and stored description.
//!
//! Storage is private and local: `<data>/vision/archive.sqlite3` indexes the files in
//! `<data>/vision/images/`, and each image has a JSON sidecar with the same metadata and
//! labels. Files are written atomically with owner-only permissions. Nothing here talks to
//! the network or to another subsystem.
//!
//! The tool is deliberately small: `recent` lists by time, `search` matches labels and
//! context, and `get` returns one image. A vision model receives the image itself, exactly
//! like `visual_snapshot`; a text-only model gets the stored local description (generated
//! on demand if it did not exist yet), never a guess.

use crate::vlm::{KernelToolResult, VisionFrameInput, VoiceVlm};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};
use tracing::warn;
use uuid::Uuid;

/// Model-facing tool name.
pub const TOOL: &str = "vision_photos";

/// Context line added to a turn when the conversation already has archived images. The
/// tool selector looks for it so `vision_photos` stays available even when the new message
/// says only "what did I show you?".
pub const ARCHIVE_CONTEXT_NOTE: &str = "This conversation has earlier archived photos";

/// At most this many metadata rows are returned by one query.
const MAX_RESULTS: usize = 20;
/// Recent defaults to the last day; search has no lower time bound by default.
const DEFAULT_RECENT_MINUTES: i64 = 24 * 60;
/// A single stored image may not exceed the same limit the turn accepts.
const MAX_IMAGE_BYTES: usize = 4_000_000;
const MAX_CAPTION_CHARS: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 1200;
const MAX_LABEL_WORDS: usize = 24;
const MAX_IMAGE_SIDE: u32 = 4096;
/// Longest edge read for a list result; `get` returns the full stored description.
const LIST_DESCRIPTION_CHARS: usize = 240;

/// Words that add no search value next to the source/origin labels.
const LABEL_STOPWORDS: &[&str] = &[
    "this", "that", "with", "from", "what", "which", "when", "where", "have", "your", "their",
    "there", "about", "does", "they", "them", "then", "than", "into", "over", "under", "also",
    "show", "shows", "photo", "photos", "image", "images", "picture", "pictures", "snapshot",
    "camera", "screen",
];

const IMAGE_COLUMNS: &str = "id, taken_at_ms, conversation_id, source, origin, format, width, \
                             height, caption, description, labels, bytes";

/// One archived image, without its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImageMeta {
    /// Stable archive id such as `img-20261111T024500Z-1a2b3c4d`.
    pub id: String,
    /// When the server received the image.
    pub taken_at: DateTime<Utc>,
    /// Session the image arrived in.
    pub conversation_id: String,
    /// `camera`, `screen` or `upload`.
    pub source: String,
    /// `snapshot` for a model-requested frame, `attached` for a user-sent image.
    pub origin: String,
    /// Encoded format (`image/jpeg` or `image/png`).
    pub format: String,
    /// Encoded width, read from the file itself.
    pub width: u32,
    /// Encoded height, read from the file itself.
    pub height: u32,
    /// What the user or the model asked when the image was captured.
    pub caption: Option<String>,
    /// Local vision description, when one was produced.
    pub description: Option<String>,
    /// Searchable labels: source, origin, date and context words.
    pub labels: Vec<String>,
    /// Stored size in bytes.
    pub bytes: usize,
}

impl ImageMeta {
    /// Bounded JSON for a tool result. The image itself is returned separately.
    pub fn summary_json(&self) -> Value {
        json!({
            "id": &self.id,
            "taken_at": self.taken_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "conversation_id": &self.conversation_id,
            "source": &self.source,
            "origin": &self.origin,
            "format": &self.format,
            "width": self.width,
            "height": self.height,
            "caption": self.caption.as_deref().map(|text| clip(text, 300)),
            "description": self.description.as_deref().map(|text| clip(text, LIST_DESCRIPTION_CHARS)),
            "labels": &self.labels,
        })
    }
}

/// A stored image and its bytes.
pub struct StoredImage {
    /// Metadata row.
    pub meta: ImageMeta,
    /// Encoded image bytes.
    pub bytes: Vec<u8>,
}

/// Fields needed to store one image.
pub struct NewImage<'a> {
    /// Encoded PNG or JPEG bytes.
    pub bytes: &'a [u8],
    /// `image/png` or `image/jpeg`.
    pub format: &'a str,
    /// `camera`, `screen` or `upload`.
    pub source: &'a str,
    /// `snapshot` or `attached`.
    pub origin: &'a str,
    /// Voice session the image belongs to.
    pub conversation_id: &'a str,
    /// The user's text or the tool's question.
    pub caption: Option<&'a str>,
    /// A local description, when one already exists.
    pub description: Option<&'a str>,
}

/// The on-disk archive. Cheap to construct; each call uses a short-lived SQLite connection.
#[derive(Clone, Debug)]
pub struct Archive {
    root: PathBuf,
}

impl Archive {
    /// Open the owner's archive under `<data>/vision`.
    pub fn open() -> Result<Self, String> {
        Self::at(default_root())
    }

    /// Open an archive at an explicit root, for tests and recovery.
    pub fn at(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        let images = root.join("images");
        fs::create_dir_all(&images).map_err(|error| {
            format!("The photo archive directory could not be created: {error}")
        })?;
        private_dir(&root)?;
        private_dir(&images)?;
        let archive = Self { root };
        // Several turns and tests may open the archive at once. Serialize the schema
        // creation inside this process so the first open cannot race another one.
        static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = INIT.lock().unwrap_or_else(|poison| poison.into_inner());
        archive.with_conn(|connection| {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS images(
                    id TEXT PRIMARY KEY,
                    taken_at_ms INTEGER NOT NULL,
                    conversation_id TEXT NOT NULL,
                    source TEXT NOT NULL,
                    origin TEXT NOT NULL,
                    format TEXT NOT NULL,
                    width INTEGER NOT NULL,
                    height INTEGER NOT NULL,
                    caption TEXT,
                    description TEXT,
                    labels TEXT NOT NULL,
                    bytes INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS images_taken_at ON images(taken_at_ms DESC);
                CREATE INDEX IF NOT EXISTS images_conversation
                    ON images(conversation_id, taken_at_ms DESC);",
            )
        })?;
        Ok(archive)
    }

    /// Store one image and return its metadata. A bad format, an unreadable image or an
    /// oversized payload is rejected, never stored partially.
    pub fn store(&self, image: NewImage<'_>) -> Result<ImageMeta, String> {
        if image.bytes.is_empty() || image.bytes.len() > MAX_IMAGE_BYTES {
            return Err("The image is empty or larger than 4MB".into());
        }
        if !matches!(image.source, "camera" | "screen" | "upload") {
            return Err("The image source must be camera, screen or upload".into());
        }
        if !matches!(image.origin, "snapshot" | "attached") {
            return Err("The image origin must be snapshot or attached".into());
        }
        if image.conversation_id.is_empty() {
            return Err("The image has no conversation".into());
        }
        let (width, height) = image_dimensions(image.bytes, image.format)?;
        let taken_at = Utc::now();
        let caption = image
            .caption
            .and_then(|text| bounded(text, MAX_CAPTION_CHARS));
        let description = image
            .description
            .and_then(|text| bounded(text, MAX_DESCRIPTION_CHARS));
        let labels = labels_for(
            image.source,
            image.origin,
            taken_at,
            caption.as_deref(),
            description.as_deref(),
        );
        let id = new_id(taken_at);
        let path = self.image_path(&id, image.format);
        write_atomic(&path, image.bytes)?;
        let meta = ImageMeta {
            id: id.clone(),
            taken_at,
            conversation_id: image.conversation_id.to_string(),
            source: image.source.to_string(),
            origin: image.origin.to_string(),
            format: image.format.to_string(),
            width,
            height,
            caption,
            description,
            labels: labels.split_whitespace().map(str::to_string).collect(),
            bytes: image.bytes.len(),
        };
        let inserted = self.with_conn(|connection| {
            connection.execute(
                "INSERT INTO images(
                    id, taken_at_ms, conversation_id, source, origin, format, width, height,
                    caption, description, labels, bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    &meta.id,
                    meta.taken_at.timestamp_millis(),
                    &meta.conversation_id,
                    &meta.source,
                    &meta.origin,
                    &meta.format,
                    meta.width,
                    meta.height,
                    meta.caption,
                    meta.description,
                    labels,
                    meta.bytes as i64,
                ],
            )
        });
        if let Err(error) = inserted {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        if let Err(error) = write_sidecar(&self.root, &meta) {
            warn!(%error, id = %meta.id, "Archived photo sidecar was not written");
        }
        Ok(meta)
    }

    /// Newest images first, optionally limited to one conversation and a lower time bound.
    pub fn recent(
        &self,
        conversation: Option<&str>,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ImageMeta>, String> {
        let limit = limit.clamp(1, MAX_RESULTS) as i64;
        let since = since.timestamp_millis();
        self.query(
            &format!(
                "SELECT {IMAGE_COLUMNS} FROM images
                 WHERE (?1 IS NULL OR conversation_id = ?1) AND taken_at_ms >= ?2
                 ORDER BY taken_at_ms DESC, rowid DESC LIMIT ?3"
            ),
            &[&conversation, &since, &limit],
        )
    }

    /// Search labels, caption and stored description. `since` absent means all time.
    pub fn search(
        &self,
        conversation: Option<&str>,
        query: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<ImageMeta>, String> {
        let limit = limit.clamp(1, MAX_RESULTS) as i64;
        let since = since.map(|at| at.timestamp_millis()).unwrap_or(i64::MIN);
        self.query(
            &format!(
                "SELECT {IMAGE_COLUMNS} FROM images
                 WHERE (?1 IS NULL OR conversation_id = ?1) AND taken_at_ms >= ?2
                   AND (instr(lower(labels), lower(?3)) > 0
                     OR instr(lower(coalesce(caption, '')), lower(?3)) > 0
                     OR instr(lower(coalesce(description, '')), lower(?3)) > 0)
                 ORDER BY taken_at_ms DESC, rowid DESC LIMIT ?4"
            ),
            &[&conversation, &since, &query, &limit],
        )
    }

    /// Whether this conversation has any archived image. Used only to tell the model that
    /// a later turn can look again; it never returns image data.
    pub fn has_images(&self, conversation: &str) -> Result<bool, String> {
        self.with_conn(|connection| {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM images WHERE conversation_id = ?1)",
                [conversation],
                |row| row.get::<_, bool>(0),
            )
        })
    }

    /// Read one image and its metadata by id.
    pub fn get(&self, id: &str) -> Result<StoredImage, String> {
        let meta = self
            .load_meta(id)?
            .ok_or_else(|| "No archived photo has that id".to_string())?;
        let path = self.image_path(&meta.id, &meta.format);
        let bytes = fs::read(&path)
            .map_err(|error| format!("The archived photo file could not be read: {error}"))?;
        Ok(StoredImage { meta, bytes })
    }

    /// Replace the stored description and refresh the searchable labels.
    pub fn set_description(&self, id: &str, description: &str) -> Result<(), String> {
        let Some(meta) = self.load_meta(id)? else {
            return Err("No archived photo has that id".into());
        };
        let description = bounded(description, MAX_DESCRIPTION_CHARS)
            .ok_or_else(|| "The description is empty".to_string())?;
        let labels = labels_for(
            &meta.source,
            &meta.origin,
            meta.taken_at,
            meta.caption.as_deref(),
            Some(&description),
        );
        self.with_conn(|connection| {
            connection.execute(
                "UPDATE images SET description = ?1, labels = ?2 WHERE id = ?3",
                params![description, labels, id],
            )
        })?;
        let updated = ImageMeta {
            description: Some(description),
            labels: labels.split_whitespace().map(str::to_string).collect(),
            ..meta
        };
        if let Err(error) = write_sidecar(&self.root, &updated) {
            warn!(%error, id = %updated.id, "Updated archived photo sidecar was not written");
        }
        Ok(())
    }

    fn query(&self, sql: &str, values: &[&dyn rusqlite::ToSql]) -> Result<Vec<ImageMeta>, String> {
        self.with_conn(|connection| {
            let mut statement = connection.prepare(sql)?;
            statement
                .query_map(values, from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    fn load_meta(&self, id: &str) -> Result<Option<ImageMeta>, String> {
        if !valid_id(id) {
            return Err("That is not an archived photo id".into());
        }
        self.with_conn(|connection| {
            connection
                .query_row(
                    &format!("SELECT {IMAGE_COLUMNS} FROM images WHERE id = ?1"),
                    [id],
                    from_row,
                )
                .optional()
        })
    }

    fn image_path(&self, id: &str, format: &str) -> PathBuf {
        let extension = match format {
            "image/png" => "png",
            _ => "jpg",
        };
        self.root.join("images").join(format!("{id}.{extension}"))
    }

    fn with_conn<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        let mut connection = Connection::open(self.root.join("archive.sqlite3"))
            .map_err(|error| format!("The photo archive could not be opened: {error}"))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("The photo archive is unavailable: {error}"))?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(|error| format!("The photo archive is unavailable: {error}"))?;
        operation(&mut connection)
            .map_err(|error| format!("The photo archive query failed: {error}"))
    }
}

/// Store an image that arrived attached to a user turn.
pub fn store_attached(
    frame: &VisionFrameInput,
    source: &str,
    conversation: &str,
    prompt: &str,
) -> Result<ImageMeta, String> {
    Archive::open()?.store(NewImage {
        bytes: &frame.data,
        format: &frame.format,
        source,
        origin: "attached",
        conversation_id: conversation,
        caption: Some(prompt),
        description: None,
    })
}

/// Store a frame the model requested with `visual_snapshot`.
pub fn store_snapshot(
    frame: &VisionFrameInput,
    source: &str,
    conversation: &str,
    question: Option<&str>,
) -> Result<ImageMeta, String> {
    Archive::open()?.store(NewImage {
        bytes: &frame.data,
        format: &frame.format,
        source,
        origin: "snapshot",
        conversation_id: conversation,
        caption: question,
        description: None,
    })
}

/// Record a local description for a stored image, best effort.
pub fn describe_stored(id: &str, description: &str) {
    match Archive::open() {
        Ok(archive) => {
            if let Err(error) = archive.set_description(id, description) {
                warn!(%error, id, "Archived photo description was not stored");
            }
        }
        Err(error) => warn!(%error, id, "Photo archive is unavailable while saving a description"),
    }
}

/// The model-facing tool schema.
pub fn tool_definition() -> Value {
    json!({"type":"function","function":{
        "name": TOOL,
        "description": "Look through the local archive of photos and screenshots from this and earlier conversations. Use it when the user refers to an image that is not attached to the current message, including a photo taken earlier in this conversation. mode=recent lists recent images (default: this conversation, last 24 hours); mode=search finds older images by their labels, caption or stored description (set scope=all to search every conversation); mode=get returns one image by id: you see the image itself when your model supports vision, otherwise you receive its stored description.",
        "parameters": {"type":"object","properties":{
            "mode": {"type":"string","enum":["recent","search","get"],"description":"recent lists by time, search matches labels/context, get fetches one by id"},
            "id": {"type":"string","maxLength":80,"description":"Photo id from a recent or search result"},
            "query": {"type":"string","maxLength":300,"description":"Words describing the photo's context or content (search only)"},
            "since_minutes": {"type":"integer","minimum":1,"maximum":525600,"description":"How far back to look; recent defaults to 1440, search defaults to all time"},
            "limit": {"type":"integer","minimum":1,"maximum":20,"description":"Maximum results; default 10"},
            "scope": {"type":"string","enum":["conversation","all"],"description":"conversation (default) limits to this conversation; all searches the whole archive"}
        },"required":["mode"],"additionalProperties":false}
    }})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Recent,
    Search,
    Get,
}

struct PhotoQuery {
    mode: Mode,
    id: Option<String>,
    query: Option<String>,
    since_minutes: Option<i64>,
    limit: usize,
    all_conversations: bool,
}

/// Validate the model's arguments. Errors are owner-readable and become a failed tool result.
fn parse_params(value: &Value) -> Result<PhotoQuery, String> {
    let mode = match value["mode"].as_str() {
        Some("recent") => Mode::Recent,
        Some("search") => Mode::Search,
        Some("get") => Mode::Get,
        _ => return Err("mode must be recent, search or get".into()),
    };
    let id = value["id"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let query = value["query"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(300).collect::<String>());
    let since_minutes = value["since_minutes"]
        .as_i64()
        .filter(|minutes| *minutes >= 1)
        .map(|minutes| minutes.min(525_600));
    let limit = value["limit"]
        .as_u64()
        .map(|value| value as usize)
        .unwrap_or(10)
        .clamp(1, MAX_RESULTS);
    let all_conversations = match value["scope"].as_str() {
        None | Some("conversation") => false,
        Some("all") => true,
        Some(other) => return Err(format!("scope must be conversation or all, not {other}")),
    };
    match mode {
        Mode::Get if id.is_none() => return Err("id is required for get".into()),
        Mode::Search if query.is_none() => return Err("query is required for search".into()),
        _ => {}
    }
    Ok(PhotoQuery {
        mode,
        id,
        query,
        since_minutes,
        limit,
        all_conversations,
    })
}

/// Run one `vision_photos` call. Returns the tool result and, for a vision model, the user
/// message that carries the retrieved image into the next model round.
pub async fn run_tool(
    vlm: &VoiceVlm,
    conversation: Option<&str>,
    params: &Value,
) -> (KernelToolResult, Option<Value>) {
    let failed = |error: String| {
        (
            KernelToolResult {
                tool: TOOL.into(),
                success: false,
                output: json!({"error": error}),
            },
            None,
        )
    };
    let query = match parse_params(params) {
        Ok(query) => query,
        Err(error) => return failed(error),
    };
    let archive = match Archive::open() {
        Ok(archive) => archive,
        Err(error) => return failed(error),
    };
    let scope = if query.all_conversations {
        None
    } else {
        conversation
    };
    match query.mode {
        Mode::Recent => {
            let minutes = query.since_minutes.unwrap_or(DEFAULT_RECENT_MINUTES);
            let since = Utc::now() - ChronoDuration::minutes(minutes);
            match archive.recent(scope, since, query.limit) {
                Ok(images) => list_result(images),
                Err(error) => failed(error),
            }
        }
        Mode::Search => {
            let since = query
                .since_minutes
                .map(|minutes| Utc::now() - ChronoDuration::minutes(minutes));
            match archive.search(
                scope,
                query.query.as_deref().unwrap_or_default(),
                since,
                query.limit,
            ) {
                Ok(images) => list_result(images),
                Err(error) => failed(error),
            }
        }
        Mode::Get => match archive.get(query.id.as_deref().unwrap_or_default()) {
            Ok(stored) => present(vlm, &archive, stored).await,
            Err(error) => failed(error),
        },
    }
}

fn list_result(images: Vec<ImageMeta>) -> (KernelToolResult, Option<Value>) {
    let note = if images.is_empty() {
        "No archived photos match. Use scope=all or a larger since_minutes to look further back."
    } else {
        "Use mode=get with an id to see one photo. The image itself is attached only for get."
    };
    (
        KernelToolResult {
            tool: TOOL.into(),
            success: true,
            output: json!({
                "count": images.len(),
                "images": images.iter().map(ImageMeta::summary_json).collect::<Vec<_>>(),
                "note": note,
            }),
        },
        None,
    )
}

async fn present(
    vlm: &VoiceVlm,
    archive: &Archive,
    stored: StoredImage,
) -> (KernelToolResult, Option<Value>) {
    let StoredImage { meta, bytes } = stored;
    let frame = VisionFrameInput {
        width: meta.width,
        height: meta.height,
        format: meta.format.clone(),
        data: bytes,
    };
    if crate::visual_snapshot::main_model_has_vision(vlm).await {
        return attach(&meta, frame);
    }
    let description = match meta
        .description
        .clone()
        .filter(|text| !text.trim().is_empty())
    {
        Some(description) => Some(description),
        None => {
            match crate::visual_snapshot::observe(
                vlm,
                &frame,
                &meta.source,
                meta.caption.as_deref(),
            )
            .await
            {
                Ok(crate::visual_snapshot::Observation::Text(text)) => {
                    if let Err(error) = archive.set_description(&meta.id, &text) {
                        warn!(%error, id = %meta.id, "Archived photo description was not stored");
                    }
                    Some(text)
                }
                Ok(crate::visual_snapshot::Observation::Native) => return attach(&meta, frame),
                Err(error) => {
                    return (
                        KernelToolResult {
                            tool: TOOL.into(),
                            success: false,
                            output: json!({
                                "photo": meta.summary_json(),
                                "error": format!("The photo is archived, but it could not be described: {error}"),
                            }),
                        },
                        None,
                    );
                }
            }
        }
    };
    let description = description.unwrap_or_default();
    (
        KernelToolResult {
            tool: TOOL.into(),
            success: true,
            output: json!({
                "photo": meta.summary_json(),
                "description": clip(&description, MAX_DESCRIPTION_CHARS),
                "note": "This is the locally stored description of the photo. Answer from it; do not claim details it does not mention.",
            }),
        },
        None,
    )
}

fn attach(meta: &ImageMeta, frame: VisionFrameInput) -> (KernelToolResult, Option<Value>) {
    let label = format!(
        "[Archived {} photo from {}]",
        meta.source,
        meta.taken_at.to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    match crate::vlm::multimodal_content(&label, Some(frame)) {
        Ok(content) => (
            KernelToolResult {
                tool: TOOL.into(),
                success: true,
                output: json!({
                    "photo": meta.summary_json(),
                    "attached": true,
                    "note": "The photo is attached in the next message.",
                }),
            },
            Some(json!({"role":"user","content":content})),
        ),
        Err(error) => (
            KernelToolResult {
                tool: TOOL.into(),
                success: false,
                output: json!({"error": error.to_string()}),
            },
            None,
        ),
    }
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageMeta> {
    let taken_at_ms: i64 = row.get(1)?;
    let taken_at = DateTime::<Utc>::from_timestamp_millis(taken_at_ms).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(1, "taken_at_ms".into(), rusqlite::types::Type::Integer)
    })?;
    let labels: String = row.get(10)?;
    Ok(ImageMeta {
        id: row.get(0)?,
        taken_at,
        conversation_id: row.get(2)?,
        source: row.get(3)?,
        origin: row.get(4)?,
        format: row.get(5)?,
        width: row.get(6)?,
        height: row.get(7)?,
        caption: row.get(8)?,
        description: row.get(9)?,
        labels: labels.split_whitespace().map(str::to_string).collect(),
        bytes: row.get::<_, i64>(11)? as usize,
    })
}

fn image_dimensions(bytes: &[u8], format: &str) -> Result<(u32, u32), String> {
    let format = match format {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return Err("Upload PNG or JPEG".into()),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    reader.limits(limits);
    reader
        .into_dimensions()
        .map_err(|error| format!("The image could not be read: {error}"))
}

fn labels_for(
    source: &str,
    origin: &str,
    taken_at: DateTime<Utc>,
    caption: Option<&str>,
    description: Option<&str>,
) -> String {
    let mut labels = vec![
        format!("source:{source}"),
        format!("origin:{origin}"),
        format!("date:{}", taken_at.format("%Y-%m-%d")),
    ];
    let mut seen: HashSet<String> = HashSet::new();
    for word in format!(
        "{} {}",
        caption.unwrap_or_default(),
        description.unwrap_or_default()
    )
    .split(|character: char| !character.is_alphanumeric())
    {
        let word = word.to_lowercase();
        if word.chars().count() < 4
            || LABEL_STOPWORDS.contains(&word.as_str())
            || !seen.insert(word.clone())
        {
            continue;
        }
        labels.push(word);
        if labels.len() >= MAX_LABEL_WORDS {
            break;
        }
    }
    labels.join(" ")
}

fn new_id(taken_at: DateTime<Utc>) -> String {
    let random = Uuid::new_v4().simple().to_string();
    format!("img-{}-{}", taken_at.format("%Y%m%dT%H%M%SZ"), &random[..8])
}

fn valid_id(id: &str) -> bool {
    id.len() <= 80
        && id.starts_with("img-")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn bounded(text: &str, max: usize) -> Option<String> {
    let text: String = text
        .trim()
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(max)
        .collect();
    (!text.trim().is_empty()).then_some(text.trim().to_string())
}

fn clip(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
    fs::write(&temporary, bytes)
        .map_err(|error| format!("The archived photo could not be written: {error}"))?;
    private_file(&temporary)?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("The archived photo could not be stored: {error}"))
}

fn write_sidecar(root: &Path, meta: &ImageMeta) -> Result<(), String> {
    let path = root.join("images").join(format!("{}.json", meta.id));
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|error| format!("The photo labels could not be encoded: {error}"))?;
    write_atomic(&path, &bytes)
}

#[cfg(unix)]
fn private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o770))
        .map_err(|error| format!("The photo archive directory permissions failed: {error}"))
}

#[cfg(not(unix))]
fn private_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("The archived photo permissions failed: {error}"))
}

#[cfg(not(unix))]
fn private_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// The real data root, or a per-test temporary root so unit tests never touch owner data.
fn default_root() -> PathBuf {
    #[cfg(test)]
    {
        use std::sync::OnceLock;
        static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();
        TEST_ROOT
            .get_or_init(|| {
                std::env::temp_dir().join(format!(
                    "sail-vision-test-{}-{}",
                    std::process::id(),
                    Uuid::new_v4().simple()
                ))
            })
            .clone()
    }
    #[cfg(not(test))]
    {
        sail_common::storage::data_dir("vision")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive() -> (Archive, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "sail-vision-archive-test-{}",
            Uuid::new_v4().simple()
        ));
        (Archive::at(&root).expect("archive"), root)
    }

    fn jpeg() -> Vec<u8> {
        // 1x1 JPEG, small enough to store and real enough to read dimensions from.
        let mut bytes = Vec::new();
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 0]));
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .expect("test jpeg");
        bytes
    }

    fn store(archive: &Archive, conversation: &str, caption: Option<&str>) -> ImageMeta {
        let bytes = jpeg();
        archive
            .store(NewImage {
                bytes: &bytes,
                format: "image/jpeg",
                source: "camera",
                origin: "snapshot",
                conversation_id: conversation,
                caption,
                description: None,
            })
            .expect("store")
    }

    #[test]
    fn stores_bytes_labels_and_reads_them_back() {
        let (archive, root) = archive();
        let meta = store(&archive, "session-a", Some("Read the whiteboard error"));
        assert_eq!(meta.conversation_id, "session-a");
        assert_eq!(meta.source, "camera");
        assert_eq!(meta.origin, "snapshot");
        assert_eq!((meta.width, meta.height), (1, 1));
        assert!(meta.labels.iter().any(|label| label == "source:camera"));
        assert!(meta.labels.iter().any(|label| label == "whiteboard"));
        assert!(
            root.join("images")
                .join(format!("{}.jpg", meta.id))
                .is_file()
        );
        assert!(
            root.join("images")
                .join(format!("{}.json", meta.id))
                .is_file()
        );

        assert!(archive.has_images("session-a").expect("has images"));
        assert!(!archive.has_images("session-b").expect("no images"));

        let stored = archive.get(&meta.id).expect("get");
        assert_eq!(stored.meta.id, meta.id);
        assert_eq!(stored.bytes, jpeg());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recent_is_newest_first_and_scoped_to_one_conversation() {
        let (archive, root) = archive();
        let first = store(&archive, "session-a", Some("first"));
        let second = store(&archive, "session-b", Some("second"));

        let all = archive
            .recent(None, Utc::now() - ChronoDuration::hours(1), 10)
            .expect("recent all");
        assert_eq!(
            all.iter().map(|meta| &meta.id).collect::<Vec<_>>(),
            vec![&second.id, &first.id]
        );
        let scoped = archive
            .recent(Some("session-a"), Utc::now() - ChronoDuration::hours(1), 10)
            .expect("recent scoped");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, first.id);
        assert!(
            archive
                .recent(
                    Some("session-a"),
                    Utc::now() + ChronoDuration::seconds(1),
                    10
                )
                .expect("recent since")
                .is_empty()
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn search_matches_caption_description_and_labels() {
        let (archive, root) = archive();
        let meta = store(
            &archive,
            "session-a",
            Some("The terminal shows a build error"),
        );
        archive
            .set_description(&meta.id, "A dark screen with white compiler output")
            .expect("description");

        assert_eq!(
            archive
                .search(None, "compiler", None, 10)
                .expect("search description")[0]
                .id,
            meta.id
        );
        assert_eq!(
            archive
                .search(None, "terminal", None, 10)
                .expect("search caption")[0]
                .id,
            meta.id
        );
        assert_eq!(
            archive
                .search(None, "source:camera", None, 10)
                .expect("search label")[0]
                .id,
            meta.id
        );
        assert!(
            archive
                .search(Some("another-session"), "compiler", None, 10)
                .expect("search scoped")
                .is_empty()
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_an_unreadable_image_and_a_bad_source() {
        let (archive, root) = archive();
        let bytes = b"not an image";
        assert!(
            archive
                .store(NewImage {
                    bytes,
                    format: "image/jpeg",
                    source: "camera",
                    origin: "snapshot",
                    conversation_id: "session-a",
                    caption: None,
                    description: None,
                })
                .is_err()
        );
        assert!(
            archive
                .store(NewImage {
                    bytes: &jpeg(),
                    format: "image/jpeg",
                    source: "microphone",
                    origin: "snapshot",
                    conversation_id: "session-a",
                    caption: None,
                    description: None,
                })
                .is_err()
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn params_require_the_fields_each_mode_uses() {
        assert_eq!(
            parse_params(&json!({"mode":"recent"}))
                .expect("recent")
                .limit,
            10
        );
        assert!(parse_params(&json!({"mode":"search"})).is_err());
        assert!(parse_params(&json!({"mode":"get"})).is_err());
        assert!(parse_params(&json!({"mode":"label"})).is_err());
        assert!(parse_params(&json!({"mode":"recent","scope":"team"})).is_err());
        assert!(
            parse_params(&json!({"mode":"search","query":"whiteboard","scope":"all"}))
                .expect("search all")
                .all_conversations
        );
    }
}
