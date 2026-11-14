//! Transport and bookkeeping for the Telegram gateway.
//!
//! The modules here never contact Telegram themselves: [`telegram::BotApi`] is the only
//! way out, and tests use [`telegram::FakeBot`]. Persisted gateway files hold chat ids,
//! pairing codes or the bot token, so they are created atomically with mode 0600.
/// Parsing chat input into commands, skill calls and text.
pub mod commands;
/// The durable, de-duplicating reply queue.
pub mod outbox;
/// Pairing codes, the unknown-sender policy and approval lockout.
pub mod pairing;
/// Telegram Bot API types, pure JSON parsing and the test bot.
pub mod telegram;
/// Telegram-safe plain text and chunking.
pub mod text;

pub use commands::{Command, help_text, matches, parse, short_id};
pub use outbox::{OutMsg, Outbox};
pub use pairing::{
    Admission, CODE_ALPHABET, CODE_LEN, CODE_TTL_SECS, MAX_FAILED_APPROVALS, MAX_PENDING, Paired,
    PairingStore, PairingView, PendingPair, REQUEST_COOLDOWN_SECS,
};
pub use telegram::{
    BotApi, BotToken, FakeBot, Incoming, IncomingKind, Outgoing, parse_sent, parse_updates, request,
};
pub use text::{approval_card, chunks, plain};

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
/// Read one JSON file. A missing file is `Ok(None)`; anything unreadable or corrupt is `Err`.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", display(path))),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("corrupt {}: {error}", display(path))),
    }
}

/// Write one JSON file atomically: a 0600 temp file, fsync, rename, then fsync the directory.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    let _ = fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options
        .open(&tmp)
        .map_err(|error| format!("cannot write {}: {error}", display(path)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    fs::rename(&tmp, path).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn display(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gateway file")
        .to_string()
}
