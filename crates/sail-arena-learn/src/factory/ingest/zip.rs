//! Minimal read-only ZIP container: bounded stored/deflated entries, never extraction to disk.
use super::{IngestError, MAX_EXTRACTED_BYTES, MAX_FILE_BYTES};
use std::collections::BTreeMap;
use std::io::Read;

/// Limit EPUB directory fan-out before allocating entry metadata.
pub const MAX_ZIP_ENTRIES: usize = 4096;
/// Prevent one compressed member from occupying the whole extraction budget.
pub const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Archive(BTreeMap<String, Vec<u8>>);
fn invalid() -> IngestError {
    IngestError::Invalid("malformed or unsupported ZIP container".into())
}
fn u16_at(bytes: &[u8], at: usize) -> Result<usize, IngestError> {
    let b = bytes.get(at..at + 2).ok_or_else(invalid)?;
    Ok(u16::from_le_bytes([b[0], b[1]]) as usize)
}
fn u32_at(bytes: &[u8], at: usize) -> Result<usize, IngestError> {
    let b = bytes.get(at..at + 4).ok_or_else(invalid)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
impl Archive {
    pub fn open(bytes: &[u8]) -> Result<Self, IngestError> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(IngestError::Limit("ZIP bytes"));
        }
        let end = bytes.len().checked_sub(22).ok_or_else(invalid)?;
        let eocd = (end.saturating_sub(65535)..=end)
            .rev()
            .find(|at| {
                bytes.get(*at..*at + 4) == Some(b"PK\x05\x06")
                    && u16_at(bytes, *at + 20)
                        .ok()
                        .is_some_and(|n| *at + 22 + n == bytes.len())
            })
            .ok_or_else(invalid)?;
        if u16_at(bytes, eocd + 4)? != 0 || u16_at(bytes, eocd + 6)? != 0 {
            return Err(invalid());
        }
        let count = u16_at(bytes, eocd + 10)?;
        if count > MAX_ZIP_ENTRIES {
            return Err(IngestError::Limit("ZIP entries"));
        }
        if u16_at(bytes, eocd + 8)? != count {
            return Err(invalid());
        }
        let mut offset = u32_at(bytes, eocd + 16)?;
        let central_end = offset
            .checked_add(u32_at(bytes, eocd + 12)?)
            .ok_or_else(invalid)?;
        if central_end != eocd {
            return Err(invalid());
        }
        let mut total = 0usize;
        let mut files = BTreeMap::new();
        for _ in 0..count {
            if bytes.get(offset..offset + 4) != Some(b"PK\x01\x02") {
                return Err(invalid());
            }
            let flags = u16_at(bytes, offset + 8)?;
            if flags & 0x41 != 0 {
                return Err(IngestError::Drm("encrypted ZIP entry".into()));
            }
            let method = u16_at(bytes, offset + 10)?;
            let checksum = u32_at(bytes, offset + 16)? as u32;
            let compressed = u32_at(bytes, offset + 20)?;
            let expanded = u32_at(bytes, offset + 24)?;
            if expanded > MAX_ENTRY_BYTES || compressed > MAX_FILE_BYTES {
                return Err(IngestError::Limit("ZIP entry bytes"));
            }
            total = total
                .checked_add(expanded)
                .ok_or(IngestError::Limit("expanded ZIP bytes"))?;
            if total > MAX_EXTRACTED_BYTES {
                return Err(IngestError::Limit("expanded ZIP bytes"));
            }
            let name_len = u16_at(bytes, offset + 28)?;
            let extra_len = u16_at(bytes, offset + 30)?;
            let comment_len = u16_at(bytes, offset + 32)?;
            let name = std::str::from_utf8(
                bytes
                    .get(offset + 46..offset + 46 + name_len)
                    .ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?;
            if name.len() > 1024
                || name.starts_with('/')
                || name.contains(['\\', '\0'])
                || name.split('/').any(|p| p == "..")
            {
                return Err(invalid());
            }
            let local = u32_at(bytes, offset + 42)?;
            if bytes.get(local..local + 4) != Some(b"PK\x03\x04")
                || u16_at(bytes, local + 8)? != method
                || u16_at(bytes, local + 6)? != flags
            {
                return Err(invalid());
            }
            let local_name = u16_at(bytes, local + 26)?;
            if bytes.get(local + 30..local + 30 + local_name) != Some(name.as_bytes()) {
                return Err(invalid());
            }
            let start = local + 30 + local_name + u16_at(bytes, local + 28)?;
            let finish = start.checked_add(compressed).ok_or_else(invalid)?;
            if finish > u32_at(bytes, eocd + 16)? {
                return Err(invalid());
            }
            let input = bytes.get(start..finish).ok_or_else(invalid)?;
            let data = match method {
                0 => input.to_vec(),
                8 => {
                    let mut out = Vec::new();
                    flate2::read::DeflateDecoder::new(input)
                        .take(expanded as u64 + 1)
                        .read_to_end(&mut out)
                        .map_err(|_| invalid())?;
                    out
                }
                _ => return Err(invalid()),
            };
            if data.len() != expanded || crc32(&data) != checksum {
                return Err(invalid());
            }
            if files.insert(name.into(), data).is_some() {
                return Err(invalid());
            }
            offset += 46 + name_len + extra_len + comment_len;
            if offset > central_end {
                return Err(invalid());
            }
        }
        if offset != central_end {
            return Err(invalid());
        }
        Ok(Self(files))
    }
    pub fn text(&self, name: &str) -> Result<&str, IngestError> {
        let bytes = self
            .0
            .get(name)
            .ok_or_else(|| IngestError::Invalid("EPUB member missing".into()))?;
        std::str::from_utf8(bytes)
            .map_err(|_| IngestError::Invalid("EPUB XML must be UTF-8".into()))
    }
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
}

#[cfg(test)]
pub(crate) fn fixture(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let mut h = vec![0u8; 30];
        h[..4].copy_from_slice(b"PK\x03\x04");
        h[4..6].copy_from_slice(&20u16.to_le_bytes());
        h[14..18].copy_from_slice(&crc32(data).to_le_bytes());
        h[18..22].copy_from_slice(&(data.len() as u32).to_le_bytes());
        h[22..26].copy_from_slice(&(data.len() as u32).to_le_bytes());
        h[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        let mut c = vec![0u8; 46];
        c[..4].copy_from_slice(b"PK\x01\x02");
        c[4..6].copy_from_slice(&20u16.to_le_bytes());
        c[6..8].copy_from_slice(&h[4..6]);
        c[16..28].copy_from_slice(&h[14..26]);
        c[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        c[42..46].copy_from_slice(&offset.to_le_bytes());
        central.extend(c);
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    let size = central.len() as u32;
    out.extend(central);
    let mut end = vec![0u8; 22];
    end[..4].copy_from_slice(b"PK\x05\x06");
    end[8..10].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    end[10..12].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    end[12..16].copy_from_slice(&size.to_le_bytes());
    end[16..20].copy_from_slice(&start.to_le_bytes());
    out.extend(end);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_duplicate_and_oversized_archives_are_errors() {
        let valid = fixture(&[("x", b"ok")]);
        assert_eq!(Archive::open(&valid).unwrap().text("x").unwrap(), "ok");
        for end in 0..valid.len() {
            assert!(Archive::open(&valid[..end]).is_err());
        }
        assert!(Archive::open(&fixture(&[("x", b"a"), ("x", b"b")])).is_err());
        let mut bomb = valid;
        let c = bomb.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        bomb[c + 24..c + 28].copy_from_slice(&((MAX_ENTRY_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(Archive::open(&bomb), Err(IngestError::Limit(_))));
    }
    #[test]
    fn lying_deflate_sizes_and_crc_corruption_are_rejected() {
        use std::io::Write;
        let data = b"a bounded compressed member";
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();
        let original = fixture(&[("x", data)]);
        let central = original
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .unwrap();
        let mut zip = original[..31].to_vec();
        zip[8..10].copy_from_slice(&8u16.to_le_bytes());
        zip[18..22].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        zip.extend_from_slice(&compressed);
        let new_central = zip.len();
        zip.extend_from_slice(&original[central..]);
        zip[new_central + 10..new_central + 12].copy_from_slice(&8u16.to_le_bytes());
        zip[new_central + 20..new_central + 24]
            .copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        let end = zip.len() - 22;
        zip[end + 16..end + 20].copy_from_slice(&(new_central as u32).to_le_bytes());
        assert_eq!(
            Archive::open(&zip).unwrap().text("x").unwrap().as_bytes(),
            data
        );
        // The decoder reads at most declared expanded size + 1, even when the directory lies.
        zip[new_central + 24..new_central + 28].copy_from_slice(&1u32.to_le_bytes());
        assert!(Archive::open(&zip).is_err());
        let mut corrupt = fixture(&[("x", b"data")]);
        corrupt[31] ^= 1;
        assert!(Archive::open(&corrupt).is_err());
    }
}
