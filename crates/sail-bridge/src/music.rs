//! SAIL Music: the owner's own DRM-free music files (purchases from Bandcamp, iTunes Plus,
//! Qobuz, 7digital and the like), stored on the host and streamed to the owner's browser.
//!
//! Imports are MP3, M4A/AAC (or ALAC), FLAC, Ogg (Vorbis/Opus) and WAV, up to
//! [`MAX_IMPORT_BYTES`]. The container is recognised from its bytes, never the file name.
//! Protected files (iTunes FairPlay `.m4p`, encrypted MP4 sample entries) are refused: SAIL is
//! not a DRM bypass. Tags and duration come from `lofty`; embedded artwork is decoded with
//! size limits and re-encoded as a small JPEG, so no tag-supplied image reaches the browser
//! unchecked.
//!
//! Storage: `<data root>/music/library.json` and `tracks/<id>/{audio.<ext>,cover.jpg}`, all
//! written atomically with mode 0600. The id is derived from the file's SHA-256, so importing
//! the same file twice finds the existing track. Removing a track deletes SAIL's copy only.
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_IMPORT_BYTES: usize = 300 * 1024 * 1024;
pub const MAX_TRACKS: usize = 20_000;
const MAX_TEXT: usize = 300;
const COVER_EDGE: u32 = 600;
const MAX_COVER_SOURCE: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MusicError {
    #[error("{0}")]
    Invalid(String),
    #[error("This file is protected by DRM; SAIL only plays DRM-free music you own.")]
    Protected,
    #[error("SAIL plays MP3, M4A/AAC, FLAC, Ogg and WAV files.")]
    Unsupported,
    #[error("This file is larger than 300 MB.")]
    TooLarge,
    #[error("The music library is full.")]
    Full,
    #[error("That track is not in the library.")]
    NotFound,
    #[error("Storage failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Mp3,
    M4a,
    Flac,
    Ogg,
    Wav,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Mp3 => "mp3",
            Format::M4a => "m4a",
            Format::Flac => "flac",
            Format::Ogg => "ogg",
            Format::Wav => "wav",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Format::Mp3 => "audio/mpeg",
            Format::M4a => "audio/mp4",
            Format::Flac => "audio/flac",
            Format::Ogg => "audio/ogg",
            Format::Wav => "audio/wav",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(default)]
    pub album_artist: String,
    #[serde(default)]
    pub track_number: Option<u32>,
    #[serde(default)]
    pub disc_number: Option<u32>,
    #[serde(default)]
    pub year: Option<u32>,
    pub duration_ms: u64,
    pub format: Format,
    pub bytes: u64,
    pub has_cover: bool,
    pub file_name: String,
    pub added_at: String,
}

pub struct Library {
    root: PathBuf,
    index: Mutex<Vec<Track>>,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, MusicError> {
        let root = root.into();
        std::fs::create_dir_all(root.join("tracks"))?;
        let index = match std::fs::read(root.join("library.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| MusicError::Invalid(format!("library.json is unreadable: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            root,
            index: Mutex::new(index),
        })
    }

    /// Tracks ordered by artist, album, disc, track number, then title.
    pub fn list(&self) -> Vec<Track> {
        let mut tracks = self.index.lock().unwrap().clone();
        tracks.sort_by(|a, b| {
            let key = |t: &Track| {
                (
                    if t.album_artist.is_empty() {
                        t.artist.to_lowercase()
                    } else {
                        t.album_artist.to_lowercase()
                    },
                    t.album.to_lowercase(),
                    t.disc_number.unwrap_or(0),
                    t.track_number.unwrap_or(u32::MAX),
                    t.title.to_lowercase(),
                )
            };
            key(a).cmp(&key(b))
        });
        tracks
    }

    pub fn get(&self, id: &str) -> Option<Track> {
        self.index
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    /// Import one file. Returns the track and whether it was new (`false` for a re-import).
    pub fn import(&self, file_name: &str, bytes: &[u8]) -> Result<(Track, bool), MusicError> {
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err(MusicError::TooLarge);
        }
        if bytes.is_empty() {
            return Err(MusicError::Invalid("The file is empty.".into()));
        }
        let format = sniff(bytes).ok_or(MusicError::Unsupported)?;
        if format == Format::M4a && mp4_protected(bytes) {
            return Err(MusicError::Protected);
        }
        let id = sha256_hex(bytes)[..24].to_string();
        if let Some(existing) = self.get(&id) {
            return Ok((existing, false));
        }
        if self.index.lock().unwrap().len() >= MAX_TRACKS {
            return Err(MusicError::Full);
        }
        let parsed = read_tags(bytes)?;
        let stem = Path::new(file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled");
        let track = Track {
            id: id.clone(),
            title: clean(parsed.title.as_deref()).unwrap_or_else(|| clip(stem)),
            artist: clean(parsed.artist.as_deref()).unwrap_or_else(|| "Unknown artist".into()),
            album: clean(parsed.album.as_deref()).unwrap_or_default(),
            album_artist: clean(parsed.album_artist.as_deref()).unwrap_or_default(),
            track_number: parsed.track,
            disc_number: parsed.disc,
            year: parsed.year,
            duration_ms: parsed.duration_ms,
            format,
            bytes: bytes.len() as u64,
            has_cover: parsed.cover.is_some(),
            file_name: clip(
                Path::new(file_name)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("track"),
            ),
            added_at: chrono::Utc::now().to_rfc3339(),
        };
        let dir = self.root.join("tracks").join(&id);
        std::fs::create_dir_all(&dir)?;
        write_private(&dir.join(format!("audio.{}", format.extension())), bytes)?;
        if let Some(cover) = &parsed.cover {
            write_private(&dir.join("cover.jpg"), cover)?;
        }
        let mut index = self.index.lock().unwrap();
        index.push(track.clone());
        if let Err(e) = self.save(&index) {
            index.retain(|t| t.id != id);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.into());
        }
        Ok((track, true))
    }

    pub fn remove(&self, id: &str) -> Result<(), MusicError> {
        if !valid_id(id) {
            return Err(MusicError::NotFound);
        }
        let mut index = self.index.lock().unwrap();
        let before = index.len();
        index.retain(|t| t.id != id);
        if index.len() == before {
            return Err(MusicError::NotFound);
        }
        self.save(&index)?;
        let _ = std::fs::remove_dir_all(self.root.join("tracks").join(id));
        Ok(())
    }

    /// The stored audio file, its format and length, for streaming.
    pub fn audio(&self, id: &str) -> Result<(PathBuf, Format, u64), MusicError> {
        if !valid_id(id) {
            return Err(MusicError::NotFound);
        }
        let track = self.get(id).ok_or(MusicError::NotFound)?;
        let path = self
            .root
            .join("tracks")
            .join(id)
            .join(format!("audio.{}", track.format.extension()));
        let len = std::fs::metadata(&path)
            .map_err(|_| MusicError::NotFound)?
            .len();
        Ok((path, track.format, len))
    }

    pub fn cover(&self, id: &str) -> Result<Vec<u8>, MusicError> {
        if !valid_id(id) || !self.get(id).map(|t| t.has_cover).unwrap_or(false) {
            return Err(MusicError::NotFound);
        }
        std::fs::read(self.root.join("tracks").join(id).join("cover.jpg"))
            .map_err(|_| MusicError::NotFound)
    }

    fn save(&self, index: &[Track]) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(index).map_err(std::io::Error::other)?;
        write_private(&self.root.join("library.json"), &bytes)
    }
}

/// The host's music library under the SAIL data root.
pub fn global() -> Result<Arc<Library>, String> {
    static LIBRARY: OnceLock<Result<Arc<Library>, String>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            Library::open(sail_common::storage::resolve().path.join("music"))
                .map(Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone()
}

/// Recognise the container from its first bytes.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    let b = bytes;
    if b.starts_with(b"fLaC") {
        return Some(Format::Flac);
    }
    if b.starts_with(b"OggS") {
        return Some(Format::Ogg);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        return Some(Format::Wav);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        // Includes FairPlay .m4p; `mp4_protected` refuses those before anything is stored.
        return Some(Format::M4a);
    }
    if b.starts_with(b"ID3") {
        // ID3v2 may also front FLAC or AAC streams; look past it.
        if b.len() >= 10 {
            let size = ((b[6] as usize & 0x7f) << 21)
                | ((b[7] as usize & 0x7f) << 14)
                | ((b[8] as usize & 0x7f) << 7)
                | (b[9] as usize & 0x7f);
            let rest = b.get(10 + size..).unwrap_or(&[]);
            if rest.starts_with(b"fLaC") {
                return Some(Format::Flac);
            }
        }
        return Some(Format::Mp3);
    }
    // A bare MPEG audio frame: 11 sync bits, layer III.
    if b.len() >= 2 && b[0] == 0xff && (b[1] & 0xe0) == 0xe0 && (b[1] & 0x06) == 0x02 {
        return Some(Format::Mp3);
    }
    None
}

/// MP4 files whose audio sample entries are protected (`drms` FairPlay, `enca` encrypted), or
/// whose brand is M4P. Walks the box tree to `stsd`; never trusts the file extension.
pub fn mp4_protected(bytes: &[u8]) -> bool {
    if bytes.len() >= 12 && &bytes[8..12] == b"M4P " {
        return true;
    }
    fn walk(data: &[u8], depth: u8) -> bool {
        let mut at = 0usize;
        while at + 8 <= data.len() && depth < 10 {
            let size32 = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            let kind = &data[at + 4..at + 8];
            let (header, size) = match size32 {
                0 => (8, data.len() - at),
                1 if at + 16 <= data.len() => (
                    16,
                    u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize,
                ),
                n => (8, n),
            };
            if size < header || at.checked_add(size).is_none_or(|end| end > data.len()) {
                return false;
            }
            let body = &data[at + header..at + size];
            match kind {
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                    if walk(body, depth + 1) {
                        return true;
                    }
                }
                b"stsd" if body.len() >= 8 => {
                    // version/flags (4) + entry count (4), then sample entries.
                    let mut e = 8usize;
                    while e + 8 <= body.len() {
                        let esize = u32::from_be_bytes(body[e..e + 4].try_into().unwrap()) as usize;
                        let ekind = &body[e + 4..e + 8];
                        if matches!(ekind, b"drms" | b"drmi" | b"enca" | b"encv" | b"p608") {
                            return true;
                        }
                        if esize < 8 {
                            break;
                        }
                        e += esize;
                    }
                }
                _ => {}
            }
            at += size;
        }
        false
    }
    walk(bytes, 0)
}

struct Parsed {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    track: Option<u32>,
    disc: Option<u32>,
    year: Option<u32>,
    duration_ms: u64,
    cover: Option<Vec<u8>>,
}

fn read_tags(bytes: &[u8]) -> Result<Parsed, MusicError> {
    use lofty::prelude::*;
    let tagged = lofty::probe::Probe::new(Cursor::new(bytes))
        .guess_file_type()
        .map_err(|_| MusicError::Unsupported)?
        .read()
        .map_err(|e| MusicError::Invalid(format!("This audio file could not be read ({e}).")))?;
    let duration_ms = tagged.properties().duration().as_millis() as u64;
    if duration_ms == 0 {
        return Err(MusicError::Invalid(
            "This audio file has no playable audio.".into(),
        ));
    }
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |v: Option<std::borrow::Cow<'_, str>>| v.map(|s| s.into_owned());
    let cover = tag.and_then(|t| {
        let pictures = t.pictures();
        pictures
            .iter()
            .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
            .or_else(|| pictures.first())
            .and_then(|p| small_jpeg(p.data()))
    });
    Ok(Parsed {
        title: tag.and_then(|t| text(t.title())),
        artist: tag.and_then(|t| text(t.artist())),
        album: tag.and_then(|t| text(t.album())),
        album_artist: tag.and_then(|t| t.get_string(&ItemKey::AlbumArtist).map(str::to_string)),
        track: tag.and_then(|t| t.track()).filter(|n| *n > 0 && *n < 1000),
        disc: tag.and_then(|t| t.disk()).filter(|n| *n > 0 && *n < 100),
        year: tag
            .and_then(|t| t.year())
            .filter(|y| (1000..3000).contains(y)),
        duration_ms,
        cover,
    })
}

/// Decode embedded artwork with limits and re-encode it as a JPEG of at most 600px.
fn small_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || data.len() > MAX_COVER_SOURCE {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width() > COVER_EDGE || img.height() > COVER_EDGE {
        img.thumbnail(COVER_EDGE, COVER_EDGE)
    } else {
        img
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&img.to_rgb8())
        .ok()?;
    Some(out)
}

fn clean(value: Option<&str>) -> Option<String> {
    let v: String = value?.chars().filter(|c| !c.is_control()).collect();
    let v = v.trim();
    (!v.is_empty()).then(|| clip(v))
}

fn clip(v: &str) -> String {
    v.chars().take(MAX_TEXT).collect()
}

fn valid_id(id: &str) -> bool {
    id.len() == 24
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Write through a temporary file and rename, so a crash never leaves half a file; owner-only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 16-bit mono PCM WAV of `ms` milliseconds of silence, with an optional
    /// RIFF INFO title/artist.
    pub(crate) fn wav(ms: u32, title: Option<&str>) -> Vec<u8> {
        let rate = 8000u32;
        let samples = rate * ms / 1000;
        let data_len = samples * 2;
        let mut info = Vec::new();
        if let Some(t) = title {
            let mut chunk = |id: &[u8; 4], text: &str| {
                let mut v = text.as_bytes().to_vec();
                v.push(0);
                if v.len() % 2 == 1 {
                    v.push(0);
                }
                info.extend_from_slice(id);
                info.extend_from_slice(&(v.len() as u32).to_le_bytes());
                info.extend_from_slice(&v);
            };
            chunk(b"INAM", t);
            chunk(b"IART", "Test Artist");
        }
        let list_len = if info.is_empty() {
            0
        } else {
            12 + info.len() as u32
        };
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len + list_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        if !info.is_empty() {
            out.extend_from_slice(b"LIST");
            out.extend_from_slice(&(4 + info.len() as u32).to_le_bytes());
            out.extend_from_slice(b"INFO");
            out.extend_from_slice(&info);
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.resize(out.len() + data_len as usize, 0);
        out
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sail-music-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mp4_with_entry(entry: &[u8; 4], brand: &[u8; 4]) -> Vec<u8> {
        fn bx(kind: &[u8], body: &[u8]) -> Vec<u8> {
            let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(kind);
            v.extend_from_slice(body);
            v
        }
        let mut stsd_body = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd_body.extend(bx(entry, &[0u8; 28]));
        let stsd = bx(b"stsd", &stsd_body);
        let moov = bx(
            b"moov",
            &bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stsd)))),
        );
        let mut ftyp_body = brand.to_vec();
        ftyp_body.extend_from_slice(&[0, 0, 0, 0]);
        ftyp_body.extend_from_slice(b"M4A mp42");
        let mut file = bx(b"ftyp", &ftyp_body);
        file.extend(moov);
        file
    }

    #[test]
    fn sniffs_containers_from_bytes() {
        assert_eq!(sniff(b"fLaC\0\0\0\x22"), Some(Format::Flac));
        assert_eq!(sniff(b"OggS\0\x02"), Some(Format::Ogg));
        assert_eq!(sniff(&wav(10, None)), Some(Format::Wav));
        assert_eq!(sniff(b"ID3\x04\0\0\0\0\0\0rest"), Some(Format::Mp3));
        assert_eq!(sniff(&[0xff, 0xfb, 0x90, 0x00]), Some(Format::Mp3));
        assert_eq!(sniff(&mp4_with_entry(b"mp4a", b"M4A ")), Some(Format::M4a));
        assert_eq!(sniff(b"%PDF-1.7"), None);
        assert_eq!(sniff(b"<html>"), None);
    }

    #[test]
    fn refuses_protected_mp4() {
        assert!(!mp4_protected(&mp4_with_entry(b"mp4a", b"M4A ")));
        assert!(!mp4_protected(&mp4_with_entry(b"alac", b"M4A ")));
        assert!(mp4_protected(&mp4_with_entry(b"drms", b"M4A ")));
        assert!(mp4_protected(&mp4_with_entry(b"enca", b"M4A ")));
        assert!(mp4_protected(&mp4_with_entry(b"mp4a", b"M4P ")));
        let dir = temp();
        let lib = Library::open(&dir).unwrap();
        assert!(matches!(
            lib.import("song.m4a", &mp4_with_entry(b"drms", b"M4A ")),
            Err(MusicError::Protected)
        ));
        assert!(lib.list().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imports_lists_streams_dedupes_and_removes() {
        let dir = temp();
        let lib = Library::open(&dir).unwrap();
        let file = wav(1500, Some("Quiet Song"));
        let (track, new) = lib.import("01 quiet.wav", &file).unwrap();
        assert!(new);
        assert_eq!(track.title, "Quiet Song");
        assert_eq!(track.artist, "Test Artist");
        assert_eq!(track.format, Format::Wav);
        assert!(
            (1400..=1600).contains(&track.duration_ms),
            "{}",
            track.duration_ms
        );
        let (again, new) = lib.import("copy.wav", &file).unwrap();
        assert!(!new);
        assert_eq!(again.id, track.id);
        let untagged = lib
            .import("Field Recording.wav", &wav(500, None))
            .unwrap()
            .0;
        assert_eq!(untagged.title, "Field Recording");
        assert_eq!(untagged.artist, "Unknown artist");
        assert_eq!(lib.list().len(), 2);
        let (path, format, len) = lib.audio(&track.id).unwrap();
        assert_eq!(format, Format::Wav);
        assert_eq!(len, file.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Reopen from disk.
        let reopened = Library::open(&dir).unwrap();
        assert_eq!(reopened.list().len(), 2);
        reopened.remove(&track.id).unwrap();
        assert!(matches!(
            reopened.audio(&track.id),
            Err(MusicError::NotFound)
        ));
        assert!(!dir.join("tracks").join(&track.id).exists());
        assert!(matches!(
            reopened.remove("../etc"),
            Err(MusicError::NotFound)
        ));
        assert!(matches!(
            reopened.cover(&untagged.id),
            Err(MusicError::NotFound)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// ID3v2.3 text frame.
    fn id3_text(id: &[u8; 4], text: &str) -> Vec<u8> {
        let mut body = vec![0u8]; // ISO-8859-1
        body.extend_from_slice(text.as_bytes());
        let mut f = id.to_vec();
        f.extend_from_slice(&(body.len() as u32).to_be_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend(body);
        f
    }

    /// An MP3: ID3v2.3 (title, artist, album, track, a PNG front cover) and ~1 s of CBR
    /// 128 kbit/s 44.1 kHz frames.
    fn mp3() -> Vec<u8> {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            900,
            900,
            image::Rgb([200, 40, 90]),
        ))
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let mut apic = vec![0u8];
        apic.extend_from_slice(b"image/png\0");
        apic.push(3); // front cover
        apic.push(0); // empty description
        apic.extend(&png);
        let mut frames = Vec::new();
        frames.extend(id3_text(b"TIT2", "Harbour Lights"));
        frames.extend(id3_text(b"TPE1", "Mara Vale"));
        frames.extend(id3_text(b"TALB", "Low Tide"));
        frames.extend(id3_text(b"TRCK", "3/10"));
        frames.extend_from_slice(b"APIC");
        frames.extend_from_slice(&(apic.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend(apic);
        let n = frames.len();
        let size = [
            (n >> 21) as u8 & 0x7f,
            (n >> 14) as u8 & 0x7f,
            (n >> 7) as u8 & 0x7f,
            n as u8 & 0x7f,
        ];
        let mut out = b"ID3\x03\x00\x00".to_vec();
        out.extend_from_slice(&size);
        out.extend(frames);
        for _ in 0..40 {
            let mut frame = vec![0xff, 0xfb, 0x90, 0x64];
            frame.resize(417, 0);
            out.extend(frame);
        }
        out
    }

    /// A FLAC with STREAMINFO (2 s at 44.1 kHz) and a Vorbis comment; no audio frames are
    /// needed for tags and duration.
    fn flac() -> Vec<u8> {
        let mut out = b"fLaC".to_vec();
        // STREAMINFO: block type 0, length 34.
        out.extend_from_slice(&[0x00, 0, 0, 34]);
        let mut info = vec![0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0];
        let rate: u64 = 44100;
        let samples: u64 = 88200;
        // 20 bits rate, 3 bits channels-1 (1), 5 bits bps-1 (15), 36 bits total samples.
        let packed: u64 = (rate << 44) | (1 << 41) | (15 << 36) | samples;
        info.extend_from_slice(&packed.to_be_bytes());
        info.extend_from_slice(&[0u8; 16]);
        out.extend(info);
        // VORBIS_COMMENT, last block.
        let mut vc = Vec::new();
        let vendor = b"sail-test";
        vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        vc.extend_from_slice(vendor);
        let comments = [
            "TITLE=Paper Moons",
            "ARTIST=Mara Vale",
            "ALBUM=Low Tide",
            "TRACKNUMBER=4",
            "DATE=2024",
        ];
        vc.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for c in comments {
            vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
            vc.extend_from_slice(c.as_bytes());
        }
        out.push(0x80 | 4);
        out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
        out.extend(vc);
        out
    }

    #[test]
    fn reads_mp3_and_flac_tags_duration_and_cover() {
        let dir = temp();
        let lib = Library::open(&dir).unwrap();
        let (m, _) = lib.import("x.mp3", &mp3()).unwrap();
        assert_eq!(
            (m.title.as_str(), m.artist.as_str(), m.album.as_str()),
            ("Harbour Lights", "Mara Vale", "Low Tide")
        );
        assert_eq!(m.format, Format::Mp3);
        assert_eq!(m.track_number, Some(3));
        assert!(
            (900..=1200).contains(&m.duration_ms),
            "mp3 duration {}",
            m.duration_ms
        );
        assert!(m.has_cover);
        let cover = lib.cover(&m.id).unwrap();
        let img = image::load_from_memory(&cover).unwrap();
        assert_eq!(
            (img.width(), img.height()),
            (600, 600),
            "cover is re-encoded and capped at 600px"
        );
        assert_eq!(
            image::guess_format(&cover).unwrap(),
            image::ImageFormat::Jpeg
        );
        let (f, _) = lib.import("y.flac", &flac()).unwrap();
        assert_eq!(
            (f.title.as_str(), f.artist.as_str()),
            ("Paper Moons", "Mara Vale")
        );
        assert_eq!(f.format, Format::Flac);
        assert_eq!(f.track_number, Some(4));
        assert_eq!(f.year, Some(2024));
        assert_eq!(f.duration_ms, 2000);
        assert!(!f.has_cover);
        let order: Vec<_> = lib.list().iter().map(|t| t.title.clone()).collect();
        assert_eq!(
            order,
            ["Harbour Lights", "Paper Moons"],
            "album order by track number"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_non_audio_and_empty() {
        let dir = temp();
        let lib = Library::open(&dir).unwrap();
        assert!(matches!(
            lib.import("x.mp3", b"not audio at all"),
            Err(MusicError::Unsupported)
        ));
        assert!(matches!(
            lib.import("x.wav", b""),
            Err(MusicError::Invalid(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
