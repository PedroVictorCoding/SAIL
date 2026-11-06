//! SAIL Reader's library: importing DRM-free EPUB and plain-text books, their canonical text,
//! and reading positions. Entertainment owns the reader (issue #333); Bridge owns the parsing,
//! because book files are untrusted input.
//!
//! An imported book is flattened once into sections of plain-text [`Block`]s, and that text is
//! canonical. A position is a text anchor (section, block, character), never a page number,
//! because pages depend on each device's layout. Nothing here calls a model.
//!
//! On disk, under `<data root>/reader/`:
//! - `library.json`: every book's metadata and position;
//! - `books/<id>/content.json`: its sections and table of contents;
//! - `books/<id>/source.<ext>`: SAIL's copy of the imported bytes, for later re-parsing.
//!
//! The book id is derived from the SHA-256 of the imported bytes, so importing the same file
//! twice finds the existing book. Removing a book deletes only SAIL's copy.
pub mod catalog;
mod epub;
mod zip;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_IMPORT_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_BOOK_CHARS: u64 = 40_000_000;
pub const MAX_SECTION_BLOCKS: usize = 60_000;
pub const MAX_BOOKS: usize = 2_000;
/// Plain-text books are cut into sections of about this many paragraphs when they have no
/// chapter headings, so no section is huge.
const TXT_SECTION_BLOCKS: usize = 150;

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error("{0}")]
    Invalid(String),
    #[error("This book is protected by DRM or encryption; SAIL only opens DRM-free books.")]
    Protected,
    #[error("SAIL Reader opens .epub and .txt files.")]
    Unsupported,
    #[error("This book is too large to import.")]
    TooLarge,
    #[error("The library is full.")]
    Full,
    #[error("No such book.")]
    NotFound,
    #[error("No such place in this book.")]
    BadAnchor,
    #[error("The position changed on another device.")]
    Stale(Box<Position>),
    #[error("The reader library could not be saved.")]
    Storage(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockKind {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    P,
    Quote,
    Li,
    Pre,
    Hr,
}

impl BlockKind {
    pub fn is_heading(self) -> bool {
        matches!(
            self,
            Self::H1 | Self::H2 | Self::H3 | Self::H4 | Self::H5 | Self::H6
        )
    }
}

/// Plain text; `\n` marks a line break inside the block (poetry, addresses, code).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocNode {
    pub title: String,
    pub section: u32,
    /// The block the entry starts at (chapters can share one EPUB file).
    #[serde(default)]
    pub block: u32,
    pub children: Vec<TocNode>,
}

#[derive(Debug)]
pub struct Parsed {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub sections: Vec<Section>,
    pub toc: Vec<TocNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    Epub,
    Txt,
}

impl BookFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Txt => "txt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub section: u32,
    pub block: u32,
    /// UTF-16 offset into the block's text; owned by the client, which lays the text out.
    pub char: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub anchor: Anchor,
    pub percent: f32,
    pub revision: u64,
    pub saved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionInfo {
    pub index: u32,
    pub title: Option<String>,
    pub chars: u64,
    pub blocks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub format: BookFormat,
    pub language: Option<String>,
    pub sections: u32,
    pub chars: u64,
    pub added_at: DateTime<Utc>,
    pub position: Option<Position>,
    /// SHA-256 of the imported bytes.
    pub content_hash: String,
    /// The imported file's name, for provenance.
    pub source_name: String,
    #[serde(default)]
    pub opened_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub section_info: Vec<SectionInfo>,
    #[serde(default)]
    pub metadata: Option<catalog::Metadata>,
    #[serde(default)]
    cover_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub sections: Vec<Section>,
    pub toc: Vec<TocNode>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    books: Vec<Book>,
}

pub struct Imported {
    pub book: Book,
    /// False when the same bytes were already in the library.
    pub created: bool,
}

pub struct Library {
    root: PathBuf,
    index: Mutex<Index>,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ReaderError> {
        let root = root.into();
        std::fs::create_dir_all(root.join("books"))?;
        let index = match std::fs::read(root.join("library.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "library.json is corrupt")
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Index::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            root,
            index: Mutex::new(index),
        })
    }

    /// Books, most recently opened (or added) first.
    pub fn list(&self) -> Vec<Book> {
        let mut books = self.index.lock().unwrap().books.clone();
        books.sort_by_key(|b| std::cmp::Reverse(b.opened_at.unwrap_or(b.added_at)));
        books
    }

    pub fn book(&self, id: &str) -> Result<Book, ReaderError> {
        self.index
            .lock()
            .unwrap()
            .books
            .iter()
            .find(|b| b.id == id)
            .cloned()
            .ok_or(ReaderError::NotFound)
    }

    /// The book, marked as opened now.
    pub fn open_book(&self, id: &str) -> Result<(Book, Content), ReaderError> {
        let content = self.content(id)?;
        let mut index = self.index.lock().unwrap();
        let book = index
            .books
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or(ReaderError::NotFound)?;
        book.opened_at = Some(Utc::now());
        let book = book.clone();
        self.save(&index)?;
        Ok((book, content))
    }

    pub fn content(&self, id: &str) -> Result<Content, ReaderError> {
        if !valid_id(id) {
            return Err(ReaderError::NotFound);
        }
        let path = self.root.join("books").join(id).join("content.json");
        let bytes = std::fs::read(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ReaderError::NotFound,
            _ => e.into(),
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|_| ReaderError::Invalid("This book's stored text is damaged.".into()))
    }

    pub fn section(&self, id: &str, index: u32) -> Result<Section, ReaderError> {
        self.content(id)?
            .sections
            .into_iter()
            .nth(index as usize)
            .ok_or(ReaderError::NotFound)
    }

    pub fn import(&self, file_name: &str, bytes: &[u8]) -> Result<Imported, ReaderError> {
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err(ReaderError::TooLarge);
        }
        let lower = file_name.to_ascii_lowercase();
        let format = if lower.ends_with(".epub") {
            BookFormat::Epub
        } else if lower.ends_with(".txt") {
            BookFormat::Txt
        } else {
            return Err(ReaderError::Unsupported);
        };
        let hash = sha256_hex(bytes);
        let id = hash[..24].to_string();
        if let Ok(book) = self.book(&id) {
            return Ok(Imported {
                book,
                created: false,
            });
        }
        let parsed = match format {
            BookFormat::Epub => epub::parse(bytes)?,
            BookFormat::Txt => parse_txt(bytes)?,
        };
        let stem = Path::new(file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .replace(['_', '-'], " ");
        let section_info: Vec<SectionInfo> = parsed
            .sections
            .iter()
            .enumerate()
            .map(|(i, s)| SectionInfo {
                index: i as u32,
                title: s.title.clone(),
                chars: s.blocks.iter().map(|b| b.text.chars().count() as u64).sum(),
                blocks: s.blocks.len() as u32,
            })
            .collect();
        let book = Book {
            id: id.clone(),
            title: parsed
                .title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| stem.trim().to_string()),
            authors: parsed.authors,
            format,
            language: parsed.language,
            sections: parsed.sections.len() as u32,
            chars: section_info.iter().map(|s| s.chars).sum(),
            added_at: Utc::now(),
            position: None,
            content_hash: hash,
            source_name: Path::new(file_name)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("book")
                .chars()
                .take(200)
                .collect(),
            opened_at: None,
            section_info,
            metadata: None,
            cover_file: None,
        };
        let content = Content {
            sections: parsed.sections,
            toc: parsed.toc,
        };
        let dir = self.root.join("books").join(&id);
        std::fs::create_dir_all(&dir)?;
        write_private(&dir.join(format!("source.{}", format.extension())), bytes)?;
        write_private(
            &dir.join("content.json"),
            &serde_json::to_vec(&content).map_err(std::io::Error::other)?,
        )?;
        let mut index = self.index.lock().unwrap();
        if let Some(existing) = index.books.iter().find(|b| b.id == id) {
            return Ok(Imported {
                book: existing.clone(),
                created: false,
            });
        }
        if index.books.len() >= MAX_BOOKS {
            drop(index);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(ReaderError::Full);
        }
        index.books.push(book.clone());
        self.save(&index)?;
        tracing::info!(book = %id, sections = book.sections, chars = book.chars, "Reader imported a book");
        Ok(Imported {
            book,
            created: true,
        })
    }

    /// Save a position if `expected_revision` is the current one (0 when there is none).
    pub fn set_position(
        &self,
        id: &str,
        anchor: Anchor,
        percent: f32,
        expected_revision: u64,
    ) -> Result<Position, ReaderError> {
        let mut index = self.index.lock().unwrap();
        let book = index
            .books
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or(ReaderError::NotFound)?;
        let section = book
            .section_info
            .get(anchor.section as usize)
            .ok_or(ReaderError::BadAnchor)?;
        if anchor.block >= section.blocks.max(1) {
            return Err(ReaderError::BadAnchor);
        }
        let current = book.position.as_ref().map_or(0, |p| p.revision);
        if current != expected_revision {
            return Err(ReaderError::Stale(Box::new(
                book.position
                    .clone()
                    .expect("a revision above 0 has a position"),
            )));
        }
        let position = Position {
            anchor,
            percent: if percent.is_finite() {
                percent.clamp(0.0, 100.0)
            } else {
                0.0
            },
            revision: current + 1,
            saved_at: Utc::now(),
        };
        book.position = Some(position.clone());
        self.save(&index)?;
        Ok(position)
    }

    /// Persist owner-selected online details without changing imported text or reading anchors.
    pub fn set_metadata(
        &self,
        id: &str,
        metadata: Option<catalog::Metadata>,
        cover: Option<&[u8]>,
    ) -> Result<Book, ReaderError> {
        let mut index = self.index.lock().unwrap();
        let position = index
            .books
            .iter()
            .position(|b| b.id == id)
            .ok_or(ReaderError::NotFound)?;
        let old = index.books[position].metadata.clone();
        let old_file = index.books[position].cover_file.clone();
        let dir = self.root.join("books").join(id);
        let new_file = cover.map(|_| format!("cover-{}.jpg", uuid::Uuid::new_v4()));
        if let Some(ref m) = metadata {
            if !catalog::valid_work_key(&m.work_key)
                || m.cover_cached != cover.is_some()
                || cover.is_some_and(|b| b.len() > catalog::MAX_COVER)
            {
                return Err(ReaderError::Invalid("Invalid book details.".into()));
            }
        } else if cover.is_some() {
            return Err(ReaderError::Invalid("A cover needs book details.".into()));
        }
        if let (Some(bytes), Some(name)) = (cover, &new_file) {
            write_private(&dir.join(name), bytes)?;
        }
        index.books[position].metadata = metadata;
        index.books[position].cover_file = new_file.clone();
        if let Err(error) = self.save(&index) {
            index.books[position].metadata = old;
            index.books[position].cover_file = old_file;
            if let Some(name) = new_file {
                let _ = std::fs::remove_file(dir.join(name));
            }
            return Err(error);
        }
        if let Some(name) = old_file {
            let _ = std::fs::remove_file(dir.join(name));
        }
        Ok(index.books[position].clone())
    }

    pub fn cover(&self, id: &str) -> Result<Vec<u8>, ReaderError> {
        let index = self.index.lock().unwrap();
        let book = index
            .books
            .iter()
            .find(|b| b.id == id)
            .ok_or(ReaderError::NotFound)?;
        if !book.metadata.as_ref().is_some_and(|m| m.cover_cached) {
            return Err(ReaderError::NotFound);
        }
        let name = book.cover_file.as_ref().ok_or(ReaderError::NotFound)?;
        // The filename is created here, never from a query or remote metadata field.
        if !name.starts_with("cover-") || !name.ends_with(".jpg") || name.contains(['/', '\\']) {
            return Err(ReaderError::NotFound);
        }
        Ok(std::fs::read(self.root.join("books").join(id).join(name))?)
    }

    /// Remove SAIL's copy of a book. The owner's original file is never touched.
    pub fn remove(&self, id: &str) -> Result<(), ReaderError> {
        let mut index = self.index.lock().unwrap();
        let before = index.books.len();
        index.books.retain(|b| b.id != id);
        if index.books.len() == before {
            return Err(ReaderError::NotFound);
        }
        self.save(&index)?;
        drop(index);
        if valid_id(id) {
            let _ = std::fs::remove_dir_all(self.root.join("books").join(id));
        }
        tracing::info!(book = %id, "Reader removed a book from the library");
        Ok(())
    }

    fn save(&self, index: &Index) -> Result<(), ReaderError> {
        let bytes = serde_json::to_vec_pretty(index).map_err(std::io::Error::other)?;
        write_private(&self.root.join("library.json"), &bytes)?;
        Ok(())
    }
}

/// The host's reader library under the SAIL data root.
pub fn global() -> Result<Arc<Library>, String> {
    static LIBRARY: OnceLock<Result<Arc<Library>, String>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            Library::open(sail_common::storage::resolve().path.join("reader"))
                .map(Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone()
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

/// UTF-8 text: paragraphs are separated by blank lines; short lines such as "Chapter 3" or
/// "CHAPTER III" start a new section. Without chapters, sections are cut by length.
fn parse_txt(bytes: &[u8]) -> Result<Parsed, ReaderError> {
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
        .map_err(|_| ReaderError::Invalid("This text file is not UTF-8.".into()))?;
    if text.chars().count() as u64 > MAX_BOOK_CHARS {
        return Err(ReaderError::TooLarge);
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut sections: Vec<Section> = Vec::new();
    let mut current = Section {
        title: None,
        blocks: Vec::new(),
    };
    for paragraph in text.split("\n\n") {
        let lines: Vec<&str> = paragraph
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }
        // Hard-wrapped prose (Gutenberg wraps near 70 columns) becomes one line; three or more
        // lines all under 60 characters (verse, addresses, contents lists) keep their breaks.
        let verse = lines.len() > 2 && lines.iter().all(|l| l.trim().chars().count() < 60);
        let joined = if verse {
            lines
                .iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ")
        };
        // "CHAPTER I." alone, or followed by the chapter's title on the next line.
        let heading = lines.len() <= 2 && is_chapter_heading(lines[0]);
        let joined = if heading {
            lines
                .iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            joined
        };
        if (heading || current.blocks.len() >= TXT_SECTION_BLOCKS) && !current.blocks.is_empty() {
            sections.push(std::mem::replace(
                &mut current,
                Section {
                    title: None,
                    blocks: Vec::new(),
                },
            ));
        }
        if heading {
            current.title = Some(joined.clone());
            current.blocks.push(Block {
                kind: BlockKind::H2,
                text: joined,
            });
        } else {
            current.blocks.push(Block {
                kind: BlockKind::P,
                text: joined,
            });
        }
    }
    if !current.blocks.is_empty() {
        sections.push(current);
    }
    if sections.is_empty() {
        return Err(ReaderError::Invalid("This text file is empty.".into()));
    }
    let toc = sections
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            s.title.clone().map(|title| TocNode {
                title: title.replace('\n', " "),
                section: i as u32,
                block: 0,
                children: Vec::new(),
            })
        })
        .collect();
    Ok(Parsed {
        title: None,
        authors: Vec::new(),
        language: None,
        sections,
        toc,
    })
}

fn is_chapter_heading(line: &str) -> bool {
    let line = line.trim();
    if line.chars().count() > 60 {
        return false;
    }
    let lower = line.to_lowercase();
    ["chapter ", "part ", "book ", "prologue", "epilogue"]
        .iter()
        .any(|p| lower.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_library() -> (Library, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sail-reader-{}", uuid::Uuid::new_v4()));
        (Library::open(&dir).unwrap(), dir)
    }

    const TXT: &str = "Chapter 1\nThe Clocks\n\nIt was a bright cold day in April,\nand the clocks were striking.\n\nShort line\nanother short\nthird line\n\nChapter 2\n\nThe end.\n";

    #[test]
    fn selected_metadata_and_cover_survive_reload_without_changing_text_or_position() {
        let (library, dir) = temp_library();
        let book = library.import("original.txt", TXT.as_bytes()).unwrap().book;
        let anchor = Anchor {
            section: 0,
            block: 0,
            char: 0,
        };
        let position = library.set_position(&book.id, anchor, 35.0, 0).unwrap();
        let metadata = catalog::Metadata {
            work_key: "/works/OL123W".into(),
            title: "Online title".into(),
            authors: vec!["An author".into()],
            first_publish_year: Some(1900),
            edition_count: Some(2),
            subjects: vec![],
            description: None,
            cover_id: Some(123),
            cover_cached: true,
            retrieved_at: Utc::now(),
            user_edited: false,
        };
        library
            .set_metadata(&book.id, Some(metadata), Some(b"cached raster"))
            .unwrap();
        let reopened = Library::open(&dir).unwrap();
        let cached = reopened.book(&book.id).unwrap();
        assert_eq!(cached.title, book.title);
        assert_eq!(cached.position, Some(position));
        assert_eq!(cached.metadata.as_ref().unwrap().title, "Online title");
        assert_eq!(reopened.cover(&book.id).unwrap(), b"cached raster");
        assert_eq!(reopened.content(&book.id).unwrap().sections.len(), 2);
        // Older library files without the new optional field still deserialize.
        let mut old = serde_json::to_value(&cached).unwrap();
        old.as_object_mut().unwrap().remove("metadata");
        assert!(
            serde_json::from_value::<Book>(old)
                .unwrap()
                .metadata
                .is_none()
        );
        reopened.set_metadata(&book.id, None, None).unwrap();
        assert!(reopened.book(&book.id).unwrap().metadata.is_none());
        assert!(matches!(
            reopened.cover(&book.id),
            Err(ReaderError::NotFound)
        ));
        assert!(
            !std::fs::read_dir(dir.join("books").join(&book.id))
                .unwrap()
                .any(|f| f
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("cover-"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imports_txt_with_chapters_dedupes_and_persists() {
        let (library, dir) = temp_library();
        let imported = library.import("my_book.txt", TXT.as_bytes()).unwrap();
        assert!(imported.created);
        let book = imported.book;
        assert_eq!(book.title, "my book");
        assert_eq!(book.format, BookFormat::Txt);
        assert_eq!(book.sections, 2);
        let s0 = library.section(&book.id, 0).unwrap();
        assert_eq!(s0.title.as_deref(), Some("Chapter 1\nThe Clocks"));
        assert_eq!(
            s0.blocks[1].text,
            "It was a bright cold day in April, and the clocks were striking."
        );
        assert_eq!(s0.blocks[2].text, "Short line\nanother short\nthird line");
        assert_eq!(library.content(&book.id).unwrap().toc.len(), 2);

        let again = library.import("renamed.txt", TXT.as_bytes()).unwrap();
        assert!(!again.created);
        assert_eq!(again.book.id, book.id);

        drop(library);
        let reopened = Library::open(&dir).unwrap();
        assert_eq!(reopened.list().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("library.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn positions_use_revisions_and_validate_anchors() {
        let (library, dir) = temp_library();
        let id = library.import("b.txt", TXT.as_bytes()).unwrap().book.id;
        let anchor = Anchor {
            section: 0,
            block: 1,
            char: 5,
        };
        let first = library.set_position(&id, anchor, 12.5, 0).unwrap();
        assert_eq!(first.revision, 1);
        // A second device still at revision 0 is told where the book really is.
        match library.set_position(&id, anchor, 50.0, 0) {
            Err(ReaderError::Stale(current)) => assert_eq!(current.revision, 1),
            other => panic!("expected stale, got {other:?}"),
        }
        let second = library
            .set_position(
                &id,
                Anchor {
                    section: 1,
                    block: 0,
                    char: 0,
                },
                400.0,
                1,
            )
            .unwrap();
        assert_eq!((second.revision, second.percent), (2, 100.0));
        assert!(matches!(
            library.set_position(
                &id,
                Anchor {
                    section: 9,
                    block: 0,
                    char: 0
                },
                1.0,
                2
            ),
            Err(ReaderError::BadAnchor)
        ));
        assert!(matches!(
            library.set_position(
                &id,
                Anchor {
                    section: 0,
                    block: 99,
                    char: 0
                },
                1.0,
                2
            ),
            Err(ReaderError::BadAnchor)
        ));
        assert_eq!(library.book(&id).unwrap().position.unwrap().revision, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_other_formats_and_removes_only_its_copy() {
        let (library, dir) = temp_library();
        assert!(matches!(
            library.import("x.pdf", b"%PDF"),
            Err(ReaderError::Unsupported)
        ));
        assert!(matches!(
            library.import("x.txt", b"\xff\xfe"),
            Err(ReaderError::Invalid(_))
        ));
        assert!(matches!(
            library.import("x.epub", b"nope"),
            Err(ReaderError::Invalid(_))
        ));
        let original = dir.join("original.txt");
        std::fs::write(&original, TXT).unwrap();
        let id = library
            .import("original.txt", &std::fs::read(&original).unwrap())
            .unwrap()
            .book
            .id;
        library.remove(&id).unwrap();
        assert!(original.exists());
        assert!(!dir.join("books").join(&id).exists());
        assert!(matches!(library.remove(&id), Err(ReaderError::NotFound)));
        assert!(matches!(
            library.content("../../etc"),
            Err(ReaderError::NotFound)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
