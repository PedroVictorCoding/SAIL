//! Telegram voice notes (OGG/Opus) to 16 kHz mono PCM for SAIL's local Whisper.
//!
//! The OGG container is parsed here (pages → packets, RFC 3533). Opus packets are decoded
//! by the system's `libopus.so.0`, loaded at run time with `dlopen`, so no new crate or
//! build dependency is needed; when the library is missing, voice notes are refused with
//! a clear message and text keeps working. Nothing is sent anywhere: transcription is local.
use std::ffi::{CStr, c_char, c_int, c_void};

/// Voice notes longer than this are refused (Telegram's own limit for notes is about 1 h).
pub const MAX_SECONDS: usize = 600;
const RATE: i32 = 16_000;

/// Split an OGG stream into the packets of its first logical stream.
pub fn packets(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    let mut partial: Vec<u8> = Vec::new();
    let mut pos = 0;
    let mut serial = None;
    while pos + 27 <= data.len() {
        if &data[pos..pos + 4] != b"OggS" {
            return Err("not an OGG file".into());
        }
        let page_serial = u32::from_le_bytes([
            data[pos + 14],
            data[pos + 15],
            data[pos + 16],
            data[pos + 17],
        ]);
        let segments = data[pos + 26] as usize;
        let table_end = pos + 27 + segments;
        if table_end > data.len() {
            return Err("truncated OGG page".into());
        }
        let lacing = &data[pos + 27..table_end];
        let body_len: usize = lacing.iter().map(|&b| b as usize).sum();
        if table_end + body_len > data.len() {
            return Err("truncated OGG page".into());
        }
        let first = *serial.get_or_insert(page_serial);
        let mut body = table_end;
        for &len in lacing {
            if page_serial == first {
                partial.extend_from_slice(&data[body..body + len as usize]);
                if len < 255 {
                    out.push(std::mem::take(&mut partial));
                }
            }
            body += len as usize;
        }
        pos = table_end + body_len;
    }
    if out.is_empty() {
        return Err("the voice note has no audio".into());
    }
    Ok(out)
}

type CreateFn = unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut c_void;
type DecodeFn = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16, c_int, c_int) -> c_int;
type DestroyFn = unsafe extern "C" fn(*mut c_void);

struct Opus {
    create: CreateFn,
    decode: DecodeFn,
    destroy: DestroyFn,
}

fn libopus() -> Result<Opus, String> {
    // SAFETY: dlopen/dlsym with NUL-terminated names; the library handle stays loaded for
    // the life of the process (never dlclosed), so the function pointers stay valid.
    unsafe {
        let handle = libc::dlopen(c"libopus.so.0".as_ptr(), libc::RTLD_NOW);
        if handle.is_null() {
            return Err("voice notes need libopus (package libopus0)".into());
        }
        let symbol = |name: &CStr| {
            let ptr = libc::dlsym(handle, name.as_ptr() as *const c_char);
            (!ptr.is_null()).then_some(ptr)
        };
        match (
            symbol(c"opus_decoder_create"),
            symbol(c"opus_decode"),
            symbol(c"opus_decoder_destroy"),
        ) {
            (Some(c), Some(d), Some(x)) => Ok(Opus {
                create: std::mem::transmute::<*mut c_void, CreateFn>(c),
                decode: std::mem::transmute::<*mut c_void, DecodeFn>(d),
                destroy: std::mem::transmute::<*mut c_void, DestroyFn>(x),
            }),
            _ => Err("libopus is missing its decoder functions".into()),
        }
    }
}

/// Decode an OGG/Opus voice note to 16 kHz mono samples.
pub fn decode(data: &[u8]) -> Result<Vec<i16>, String> {
    let packets = packets(data)?;
    let head = packets.first().ok_or("the voice note has no header")?;
    if !head.starts_with(b"OpusHead") || head.len() < 19 {
        return Err("the voice note is not Opus audio".into());
    }
    let channels = head[9].clamp(1, 2) as c_int;
    let pre_skip = u16::from_le_bytes([head[10], head[11]]) as usize / 3; // 48 kHz → 16 kHz
    let opus = libopus()?;
    let mut error: c_int = 0;
    // SAFETY: valid rate/channels; `error` is a valid out pointer.
    let decoder = unsafe { (opus.create)(RATE, channels, &mut error) };
    if decoder.is_null() || error != 0 {
        return Err(format!("libopus could not start (error {error})"));
    }
    // 120 ms is the longest Opus frame.
    let frame_max = (RATE as usize * 120 / 1000) as c_int;
    let mut buffer = vec![0i16; frame_max as usize * channels as usize];
    let mut pcm: Vec<i16> = Vec::new();
    let mut failure = None;
    // Packet 0 is OpusHead, packet 1 OpusTags.
    for packet in packets.iter().skip(2) {
        if packet.is_empty() {
            continue;
        }
        // SAFETY: the decoder is live; `packet` and `buffer` are valid for their lengths,
        // and `frame_max` fits `buffer` for `channels`.
        let n = unsafe {
            (opus.decode)(
                decoder,
                packet.as_ptr(),
                packet.len() as i32,
                buffer.as_mut_ptr(),
                frame_max,
                0,
            )
        };
        if n < 0 {
            failure = Some(format!("the voice note could not be decoded (error {n})"));
            break;
        }
        let n = n as usize;
        if channels == 1 {
            pcm.extend_from_slice(&buffer[..n]);
        } else {
            pcm.extend(
                buffer[..n * 2]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|[l, r]| ((*l as i32 + *r as i32) / 2) as i16),
            );
        }
        if pcm.len() > MAX_SECONDS * RATE as usize {
            failure = Some(format!(
                "voice notes longer than {MAX_SECONDS} s are not transcribed"
            ));
            break;
        }
    }
    // SAFETY: created above, destroyed once.
    unsafe { (opus.destroy)(decoder) };
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(pcm.split_off(pre_skip.min(pcm.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One OGG page with the given packets (each < 255 bytes) and serial.
    fn page(serial: u32, packets: &[&[u8]]) -> Vec<u8> {
        let mut out = b"OggS".to_vec();
        out.extend([0, 0]);
        out.extend([0u8; 8]);
        out.extend(serial.to_le_bytes());
        out.extend([0u8; 8]);
        out.push(packets.len() as u8);
        for p in packets {
            out.push(p.len() as u8);
        }
        for p in packets {
            out.extend_from_slice(p);
        }
        out
    }

    #[test]
    fn packets_follow_lacing_and_ignore_other_streams() {
        let mut data = page(7, &[b"OpusHead1234567890123", b"OpusTags"]);
        data.extend(page(9, &[b"other stream"]));
        data.extend(page(7, &[b"audio"]));
        let p = packets(&data).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[2], b"audio");
        assert!(packets(b"RIFF....").is_err());
        assert!(packets(&data[..30]).is_err());
        assert!(decode(&page(1, &[b"NotOpus"])).is_err());
    }

    #[test]
    fn a_packet_split_across_255_byte_segments_is_joined() {
        let long = vec![7u8; 300];
        let mut data = b"OggS".to_vec();
        data.extend([0u8; 10]);
        data.extend(1u32.to_le_bytes());
        data.extend([0u8; 8]);
        data.push(2);
        data.extend([255, 45]);
        data.extend(&long);
        assert_eq!(packets(&data).unwrap(), vec![long]);
    }

    /// Decodes a real voice note through libopus when `SAIL_TEST_OGG` names one (e.g. one
    /// second of a 440 Hz tone); without it there is nothing to decode and it returns.
    #[test]
    fn a_real_voice_note_decodes_to_16khz() {
        let Some(path) = std::env::var_os("SAIL_TEST_OGG") else {
            return;
        };
        let pcm = decode(&std::fs::read(path).unwrap()).unwrap();
        assert!(
            (15_000..=17_000).contains(&pcm.len()),
            "{} samples",
            pcm.len()
        );
        let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 5_000, "audible: peak {peak}");
    }
}
