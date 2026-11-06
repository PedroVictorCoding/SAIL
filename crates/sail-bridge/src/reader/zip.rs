//! A small, strict ZIP reader for EPUB containers.
//!
//! EPUB files are untrusted, so this reads only what an ordinary book needs and refuses
//! the rest. It accepts stored and deflated entries only and refuses ZIP64, encryption,
//! unsafe names (absolute paths, `..`, backslashes, NUL), duplicate names, too many entries,
//! entries or totals over the size limits, and suspicious compression ratios. Every entry's
//! CRC is checked. Sizes come from the central directory, and inflation is cut off at the
//! declared size, so a lying header cannot make it allocate more.
use std::collections::HashMap;
use std::io::Read;

use super::ReaderError;

pub const MAX_ENTRIES: usize = 4_000;
pub const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// Larger entries may not inflate more than this many times their compressed size.
pub const MAX_RATIO: u64 = 200;

#[derive(Debug, Clone)]
struct Entry {
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    local_offset: u64,
}

pub struct Archive<'a> {
    bytes: &'a [u8],
    entries: HashMap<String, Entry>,
}

fn bad(message: &str) -> ReaderError {
    ReaderError::Invalid(format!("This EPUB is damaged or unsafe: {message}."))
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, ReaderError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| bad("truncated header"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, ReaderError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("truncated header"))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.contains(':')
        && name.split('/').all(|part| part != ".." && part != ".")
}

impl<'a> Archive<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self, ReaderError> {
        // End of central directory: 22 bytes plus a comment of at most 65535.
        if bytes.len() < 22 {
            return Err(bad("too short to be a ZIP file"));
        }
        let floor = bytes.len().saturating_sub(22 + 65_535);
        let eocd = (floor..=bytes.len() - 22)
            .rev()
            .find(|&i| bytes[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
            .ok_or_else(|| bad("no ZIP directory"))?;
        let disk = u16_at(bytes, eocd + 4)?;
        let count = u16_at(bytes, eocd + 10)? as usize;
        let dir_size = u32_at(bytes, eocd + 12)? as usize;
        let dir_offset = u32_at(bytes, eocd + 16)? as usize;
        if disk != 0 {
            return Err(bad("multi-part archives are not supported"));
        }
        if count == 0xffff || dir_offset == 0xffff_ffff {
            return Err(bad("ZIP64 archives are not supported"));
        }
        if count > MAX_ENTRIES {
            return Err(bad("too many files"));
        }
        if dir_offset
            .checked_add(dir_size)
            .is_none_or(|end| end > eocd)
        {
            return Err(bad("directory out of bounds"));
        }
        let mut entries = HashMap::with_capacity(count);
        let mut total: u64 = 0;
        let mut at = dir_offset;
        for _ in 0..count {
            if u32_at(bytes, at)? != 0x0201_4b50 {
                return Err(bad("broken directory entry"));
            }
            let flags = u16_at(bytes, at + 8)?;
            let method = u16_at(bytes, at + 10)?;
            let crc = u32_at(bytes, at + 16)?;
            let compressed = u32_at(bytes, at + 20)? as u64;
            let size = u32_at(bytes, at + 24)? as u64;
            let name_len = u16_at(bytes, at + 28)? as usize;
            let extra_len = u16_at(bytes, at + 30)? as usize;
            let comment_len = u16_at(bytes, at + 32)? as usize;
            let local_offset = u32_at(bytes, at + 42)? as u64;
            let raw_name = bytes
                .get(at + 46..at + 46 + name_len)
                .ok_or_else(|| bad("truncated file name"))?;
            at += 46 + name_len + extra_len + comment_len;
            if flags & 0x1 != 0 || flags & 0x40 != 0 {
                return Err(ReaderError::Protected);
            }
            if compressed == 0xffff_ffff || size == 0xffff_ffff || local_offset == 0xffff_ffff {
                return Err(bad("ZIP64 entries are not supported"));
            }
            let name = std::str::from_utf8(raw_name).map_err(|_| bad("file name not UTF-8"))?;
            if name.ends_with('/') {
                continue; // a directory
            }
            if !safe_name(name) {
                return Err(bad("unsafe file path"));
            }
            if method != 0 && method != 8 {
                return Err(bad("unsupported compression"));
            }
            if method == 0 && compressed != size {
                return Err(bad("stored entry sizes disagree"));
            }
            if size > MAX_ENTRY_BYTES {
                return Err(bad("a file inside is too large"));
            }
            if size > 1024 * 1024 && size > compressed.saturating_mul(MAX_RATIO) {
                return Err(bad("suspicious compression ratio"));
            }
            total += size;
            if total > MAX_TOTAL_BYTES {
                return Err(bad("too large when unpacked"));
            }
            let entry = Entry {
                method,
                crc,
                compressed,
                size,
                local_offset,
            };
            if entries.insert(name.to_string(), entry).is_some() {
                return Err(bad("duplicate file names"));
            }
        }
        Ok(Self { bytes, entries })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, ReaderError> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| bad("a referenced file is missing"))?;
        let at = entry.local_offset as usize;
        if u32_at(self.bytes, at)? != 0x0403_4b50 {
            return Err(bad("broken file header"));
        }
        let name_len = u16_at(self.bytes, at + 26)? as usize;
        let extra_len = u16_at(self.bytes, at + 28)? as usize;
        let start = at + 30 + name_len + extra_len;
        let data = start
            .checked_add(entry.compressed as usize)
            .and_then(|end| self.bytes.get(start..end))
            .ok_or_else(|| bad("file data out of bounds"))?;
        let out = if entry.method == 0 {
            data.to_vec()
        } else {
            let mut out = Vec::with_capacity(entry.size as usize);
            flate2::read::DeflateDecoder::new(data)
                .take(entry.size + 1)
                .read_to_end(&mut out)
                .map_err(|_| bad("corrupt compressed data"))?;
            out
        };
        if out.len() as u64 != entry.size {
            return Err(bad("file size does not match its header"));
        }
        let mut crc = crc32fast::Hasher::new();
        crc.update(&out);
        if crc.finalize() != entry.crc {
            return Err(bad("checksum mismatch"));
        }
        Ok(out)
    }

    pub fn read_text(&self, name: &str) -> Result<String, ReaderError> {
        let bytes = self.read(name)?;
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        String::from_utf8(bytes.to_vec()).map_err(|_| bad("text is not UTF-8"))
    }
}

/// A minimal ZIP writer for tests: stored or deflated entries, no extras.
#[cfg(test)]
pub(crate) fn build(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
    use std::io::Write;
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in files {
        let mut crc = crc32fast::Hasher::new();
        crc.update(data);
        let crc = crc.finalize();
        let body = if *deflate {
            let mut e =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let method: u16 = if *deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(20u16.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(method.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend(crc.to_le_bytes());
        out.extend((body.len() as u32).to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(name.as_bytes());
        out.extend(&body);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(0u16.to_le_bytes());
        central.extend(method.to_le_bytes());
        central.extend([0u8; 4]);
        central.extend(crc.to_le_bytes());
        central.extend((body.len() as u32).to_le_bytes());
        central.extend((data.len() as u32).to_le_bytes());
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0u8; 12]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let dir_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(dir_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stored_and_deflated_entries_with_checked_crc() {
        let zip = build(&[
            ("mimetype", b"application/epub+zip", false),
            ("a/b.txt", b"hello hello hello", true),
        ]);
        let archive = Archive::open(&zip).unwrap();
        assert_eq!(archive.read("a/b.txt").unwrap(), b"hello hello hello");
        let mut broken = zip.clone();
        // Flip a byte inside the stored mimetype data: the CRC must catch it.
        let at = broken
            .windows(11)
            .position(|w| w == b"application")
            .unwrap();
        broken[at] = b'A';
        assert!(Archive::open(&broken).unwrap().read("mimetype").is_err());
    }

    #[test]
    fn refuses_unsafe_names_duplicates_and_bombs() {
        for name in ["../evil", "/abs", "a\\b", "a/../../b", "C:x"] {
            let zip = build(&[(name, b"x", false)]);
            assert!(Archive::open(&zip).is_err(), "{name} accepted");
        }
        let zip = build(&[("a", b"x", false), ("a", b"y", false)]);
        assert!(Archive::open(&zip).is_err());
        let bomb = vec![0u8; 8 * 1024 * 1024];
        let zip = build(&[("zeros", &bomb, true)]);
        let error = Archive::open(&zip).err().unwrap().to_string();
        assert!(error.contains("ratio"), "{error}");
        assert!(Archive::open(b"not a zip at all, not even close").is_err());
    }
}
