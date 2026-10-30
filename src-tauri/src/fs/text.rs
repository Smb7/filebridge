//! Read and write one remote text file. Bytes stay in this process. Paths are
//! SFTP strings and are never passed to `std::fs` or a shell.

use sha2::{Digest, Sha256};

use crate::session::ops::is_real_directory;
use crate::session::{LinkError, RawMeta, SessionState};

use super::path;

pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const NUL_SCAN: usize = 8 * 1024;
const WRITE_CHUNK: usize = 32 * 1024;
const PARTIAL_PREFIX: &str = ".filebridge-partial-";
const BACKUP_PREFIX: &str = ".filebridge-backup-";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFile {
    pub content: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WrittenFile {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    InvalidPath,
    NotConnected,
    NotFound,
    PermissionDenied,
    NotAFile,
    NotAllowed,
    Symlink,
    TooLarge,
    NotText,
    Conflict,
    AlreadyExists,
    Failed,
}

impl From<LinkError> for TextError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::InvalidPath => Self::InvalidPath,
            LinkError::NotConnected => Self::NotConnected,
            LinkError::NotFound => Self::NotFound,
            LinkError::PermissionDenied => Self::PermissionDenied,
            LinkError::AlreadyExists => Self::AlreadyExists,
            LinkError::NotEmpty | LinkError::NotAllowed => Self::NotAllowed,
            LinkError::Failed => Self::Failed,
        }
    }
}

impl From<path::PathError> for TextError {
    fn from(_: path::PathError) -> Self {
        Self::InvalidPath
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteVersion {
    Missing,
    Present { sha256: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteDecision {
    Create,
    Replace,
    Conflict,
    AlreadyExists,
}

/// A matching hash is the only silent replace. `overwrite` without that hash
/// is the user's confirm. A mismatch never writes, even when overwrite is set.
pub fn decide_write(
    remote: &RemoteVersion,
    overwrite: bool,
    expected: Option<&str>,
) -> WriteDecision {
    if let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) {
        if !is_sha256(expected) {
            return WriteDecision::Conflict;
        }
        return match remote {
            RemoteVersion::Present { sha256: Some(sha) } if sha.eq_ignore_ascii_case(expected) => {
                WriteDecision::Replace
            }
            RemoteVersion::Present { .. } | RemoteVersion::Missing => WriteDecision::Conflict,
        };
    }
    match remote {
        RemoteVersion::Missing => WriteDecision::Create,
        RemoteVersion::Present { .. } if overwrite => WriteDecision::Replace,
        RemoteVersion::Present { .. } => WriteDecision::AlreadyExists,
    }
}

pub fn reject_content(content: &str) -> Result<(), TextError> {
    if content.len() as u64 > MAX_TEXT_BYTES {
        return Err(TextError::TooLarge);
    }
    if content.contains('\0') {
        return Err(TextError::NotText);
    }
    Ok(())
}

/// Refuse a NUL in the first 8 KB, and any later NUL in a file we fully loaded.
/// A too-large buffer is refused whole. Nothing here returns a prefix.
pub fn inspect_owned(bytes: Vec<u8>) -> Result<String, TextError> {
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(TextError::TooLarge);
    }
    if has_nul(&bytes) {
        return Err(TextError::NotText);
    }
    String::from_utf8(bytes).map_err(|_| TextError::NotText)
}

fn has_nul(bytes: &[u8]) -> bool {
    let scan = bytes.len().min(NUL_SCAN);
    if bytes[..scan].contains(&0) {
        return true;
    }
    bytes[scan..].contains(&0)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavePaths {
    pub partial: String,
    pub backup: String,
}

pub fn plan_save_paths(path: &str, id: &str) -> Result<SavePaths, TextError> {
    if path == "/" || !safe_id(id) {
        return Err(TextError::NotAllowed);
    }
    let parent = parent_of(path);
    let partial = join(&parent, &format!("{PARTIAL_PREFIX}{id}"));
    let backup = join(&parent, &format!("{BACKUP_PREFIX}{id}"));
    if partial == path || backup == path || partial == backup {
        return Err(TextError::NotAllowed);
    }
    Ok(SavePaths { partial, backup })
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || char == '-')
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn is_writable_file(meta: &RawMeta) -> bool {
    meta.is_file && !meta.is_dir && !meta.is_symlink
}

pub(crate) async fn read_text(
    sessions: &SessionState,
    session_id: &str,
    path: &str,
) -> Result<TextFile, TextError> {
    let bytes = read_bytes(sessions, session_id, path).await?;
    let sha256 = sha256_hex(&bytes);
    let size = bytes.len() as u64;
    let content = inspect_owned(bytes)?;
    Ok(TextFile {
        content,
        sha256,
        size,
    })
}

async fn read_bytes(
    sessions: &SessionState,
    session_id: &str,
    path: &str,
) -> Result<Vec<u8>, TextError> {
    let meta = sessions.stat_path(session_id, path).await?;
    if meta.is_dir || !meta.is_file {
        return Err(TextError::NotAFile);
    }
    if meta.size > MAX_TEXT_BYTES {
        return Err(TextError::TooLarge);
    }
    let token = sessions.transfer_open_read(session_id, path).await?;
    let mut bytes = Vec::new();
    let outcome = read_capped(sessions, session_id, &token, &mut bytes).await;
    let _ = sessions.transfer_close(session_id, &token).await;
    outcome?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(TextError::TooLarge);
    }
    Ok(bytes)
}

async fn read_capped(
    sessions: &SessionState,
    session_id: &str,
    token: &str,
    bytes: &mut Vec<u8>,
) -> Result<(), TextError> {
    loop {
        let chunk = sessions
            .transfer_read(session_id, token, WRITE_CHUNK as u32)
            .await?;
        if chunk.is_empty() {
            return Ok(());
        }
        if bytes.len() + chunk.len() > MAX_TEXT_BYTES as usize {
            bytes.clear();
            return Err(TextError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
}

pub(crate) async fn write_text(
    sessions: &SessionState,
    session_id: &str,
    path: &str,
    content: &str,
    overwrite: bool,
    expected_sha256: Option<&str>,
) -> Result<WrittenFile, TextError> {
    if path == "/" {
        return Err(TextError::NotAllowed);
    }
    reject_content(content)?;
    let remote = classify_remote(sessions, session_id, path, expected_sha256.is_some()).await?;
    match decide_write(&remote, overwrite, expected_sha256) {
        WriteDecision::Conflict => return Err(TextError::Conflict),
        WriteDecision::AlreadyExists => return Err(TextError::AlreadyExists),
        WriteDecision::Create | WriteDecision::Replace => {}
    }
    let replacing = matches!(remote, RemoteVersion::Present { .. });
    commit_content(sessions, session_id, path, content.as_bytes(), replacing).await?;
    Ok(WrittenFile {
        sha256: sha256_hex(content.as_bytes()),
        size: content.len() as u64,
    })
}

async fn classify_remote(
    sessions: &SessionState,
    session_id: &str,
    path: &str,
    need_hash: bool,
) -> Result<RemoteVersion, TextError> {
    match sessions.transfer_lstat(session_id, path).await {
        Err(LinkError::NotFound) => Ok(RemoteVersion::Missing),
        Err(error) => Err(error.into()),
        Ok(meta) if meta.is_symlink => Err(TextError::Symlink),
        Ok(meta) if is_real_directory(&meta) => Err(TextError::NotAllowed),
        Ok(meta) if !is_writable_file(&meta) => Err(TextError::NotAFile),
        Ok(meta) if need_hash && meta.size > MAX_TEXT_BYTES => Err(TextError::Conflict),
        Ok(_) if need_hash => match read_bytes(sessions, session_id, path).await {
            Ok(bytes) => Ok(RemoteVersion::Present {
                sha256: Some(sha256_hex(&bytes)),
            }),
            Err(TextError::NotFound | TextError::TooLarge) => Err(TextError::Conflict),
            Err(error) => Err(error),
        },
        Ok(_) => Ok(RemoteVersion::Present { sha256: None }),
    }
}

async fn commit_content(
    sessions: &SessionState,
    session_id: &str,
    path: &str,
    bytes: &[u8],
    replacing: bool,
) -> Result<(), TextError> {
    let id = uuid::Uuid::new_v4().to_string();
    let paths = plan_save_paths(path, &id)?;
    let token = match sessions
        .transfer_open_write(session_id, &paths.partial)
        .await
    {
        Ok(token) => token,
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = write_chunks(sessions, session_id, &token, bytes).await {
        let _ = sessions.transfer_close(session_id, &token).await;
        let _ = sessions.transfer_remove(session_id, &paths.partial).await;
        return Err(error);
    }
    if let Err(error) = sessions.transfer_close(session_id, &token).await {
        let _ = sessions.transfer_remove(session_id, &paths.partial).await;
        return Err(error.into());
    }
    if let Err(error) = commit_rename(sessions, session_id, path, &paths, replacing).await {
        let _ = sessions.transfer_remove(session_id, &paths.partial).await;
        return Err(error);
    }
    Ok(())
}

async fn write_chunks(
    sessions: &SessionState,
    session_id: &str,
    token: &str,
    bytes: &[u8],
) -> Result<(), TextError> {
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + WRITE_CHUNK).min(bytes.len());
        sessions
            .transfer_write(session_id, token, &bytes[offset..end])
            .await?;
        offset = end;
    }
    Ok(())
}

async fn commit_rename(
    sessions: &SessionState,
    session_id: &str,
    path: &str,
    paths: &SavePaths,
    replacing: bool,
) -> Result<(), TextError> {
    if !replacing {
        return sessions
            .transfer_rename(session_id, &paths.partial, path)
            .await
            .map_err(TextError::from);
    }
    sessions
        .transfer_rename(session_id, path, &paths.backup)
        .await?;
    if let Err(error) = sessions
        .transfer_rename(session_id, &paths.partial, path)
        .await
    {
        let _ = sessions
            .transfer_rename(session_id, &paths.backup, path)
            .await;
        return Err(error.into());
    }
    let _ = sessions.transfer_remove(session_id, &paths.backup).await;
    Ok(())
}

pub fn command_error(error: TextError, writing: bool) -> super::CommandError {
    let (code, message) = match error {
        TextError::InvalidPath => ("invalid_path", "Remote path must be absolute."),
        TextError::NotConnected => ("not_connected", "That server is not connected."),
        TextError::NotFound => ("not_found", "That path does not exist."),
        TextError::PermissionDenied => ("permission_denied", "Permission denied."),
        TextError::NotAFile => ("not_a_file", "That path is not a file."),
        TextError::NotAllowed => ("not_allowed", "That path cannot be changed."),
        TextError::Symlink => (
            "not_allowed",
            "That path is a symlink. It was not replaced.",
        ),
        TextError::TooLarge => ("too_large", "That file is larger than 1 MiB."),
        TextError::NotText => ("not_text", "That file is not text."),
        TextError::Conflict => (
            "conflict",
            "The file changed on the server. It was not replaced.",
        ),
        TextError::AlreadyExists => ("already_exists", "That file already exists."),
        TextError::Failed if writing => ("sftp_failed", "Could not write that file."),
        TextError::Failed => ("sftp_failed", "Could not read that file."),
    };
    super::CommandError {
        code,
        message: message.to_string(),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOptions {
    pub overwrite: bool,
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

#[tauri::command]
pub async fn fs_read_text(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    path: String,
) -> Result<TextFile, super::CommandError> {
    let path = path::normalize_remote_path(&path)?;
    read_text(&sessions, &session_id, &path)
        .await
        .map_err(|error| command_error(error, false))
}

#[tauri::command]
pub async fn fs_write(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    path: String,
    content: String,
    options: WriteOptions,
) -> Result<WrittenFile, super::CommandError> {
    let path = path::normalize_remote_path(&path)?;
    write_text(
        &sessions,
        &session_id,
        &path,
        &content,
        options.overwrite,
        options.expected_sha256.as_deref(),
    )
    .await
    .map_err(|error| command_error(error, true))
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_TEXT_BYTES, NUL_SCAN, RemoteVersion, TextError, WriteDecision, command_error,
        decide_write, inspect_owned, plan_save_paths, reject_content, sha256_hex,
    };

    fn present(sha: &str) -> RemoteVersion {
        RemoteVersion::Present {
            sha256: Some(sha.to_string()),
        }
    }

    #[test]
    fn a_mismatch_does_not_authorize_a_write() {
        let sha = "ab".repeat(32);
        let other = "cd".repeat(32);
        assert_eq!(
            decide_write(&present(&sha), true, Some(&sha)),
            WriteDecision::Replace
        );
        assert_eq!(
            decide_write(&present(&sha), true, Some(&other)),
            WriteDecision::Conflict
        );
        assert_eq!(
            decide_write(&present(&sha), false, Some(&sha)),
            WriteDecision::Replace
        );
        assert_eq!(
            decide_write(&RemoteVersion::Missing, true, Some(&sha)),
            WriteDecision::Conflict
        );
        assert_eq!(
            decide_write(&present(&sha), true, Some("not-a-hash")),
            WriteDecision::Conflict
        );
        assert_eq!(
            decide_write(&RemoteVersion::Missing, false, None),
            WriteDecision::Create
        );
        assert_eq!(
            decide_write(&present(&sha), false, None),
            WriteDecision::AlreadyExists
        );
        assert_eq!(
            decide_write(&present(&sha), true, None),
            WriteDecision::Replace
        );
        assert_eq!(
            decide_write(&RemoteVersion::Present { sha256: None }, true, Some(&sha)),
            WriteDecision::Conflict
        );
    }

    #[test]
    fn text_is_refused_whole_when_it_is_binary_or_too_large() {
        let ok = inspect_owned(b"interval = \"10s\"\n".to_vec()).expect("text");
        assert_eq!(ok, "interval = \"10s\"\n");
        assert!(inspect_owned(Vec::new()).is_ok());

        let mut early = vec![b'a'; 16];
        early[0] = 0;
        assert_eq!(inspect_owned(early), Err(TextError::NotText));

        let mut at_window = vec![b'a'; NUL_SCAN];
        at_window[NUL_SCAN - 1] = 0;
        assert_eq!(inspect_owned(at_window), Err(TextError::NotText));

        let mut after_window = vec![b'a'; NUL_SCAN + 1];
        after_window[NUL_SCAN] = 0;
        assert_eq!(inspect_owned(after_window), Err(TextError::NotText));

        assert_eq!(inspect_owned(vec![0xff, 0xfe]), Err(TextError::NotText));
        assert_eq!(
            inspect_owned(vec![b'a'; MAX_TEXT_BYTES as usize + 1]),
            Err(TextError::TooLarge)
        );
        assert!(inspect_owned(vec![b'a'; 32]).is_ok());
        assert!(reject_content("ok").is_ok());
        assert_eq!(reject_content("a\0b"), Err(TextError::NotText));
        assert_eq!(
            reject_content(&"a".repeat(MAX_TEXT_BYTES as usize + 1)),
            Err(TextError::TooLarge)
        );
    }

    #[test]
    fn a_refused_file_is_not_returned_as_a_prefix_and_errors_stay_safe() {
        let secret = format!("password={}\0", "hunter2-secret");
        let refused = inspect_owned(secret.into_bytes()).expect_err("binary");
        let command = command_error(refused, false);
        let rendered = format!("{} {}", command.code, command.message);
        assert!(!rendered.contains("hunter2-secret"));
        assert!(!rendered.to_lowercase().contains("password"));
        assert!(!rendered.to_lowercase().contains("sudo"));
        assert_ne!(command.message, "a");

        let denied = command_error(TextError::PermissionDenied, true);
        assert_eq!(denied.code, "permission_denied");
        assert_eq!(denied.message, "Permission denied.");
        assert!(!denied.message.to_lowercase().contains("sudo"));

        let conflict = command_error(TextError::Conflict, true);
        assert_eq!(conflict.code, "conflict");
        assert!(conflict.message.contains("not replaced"));
        assert!(!conflict.message.to_lowercase().contains("sudo"));
    }

    #[test]
    fn save_paths_are_generated_names_in_the_parent_directory() {
        let paths = plan_save_paths("/etc/telegraf/cpu.conf", "abc-1").expect("paths");
        assert_eq!(paths.partial, "/etc/telegraf/.filebridge-partial-abc-1");
        assert_eq!(paths.backup, "/etc/telegraf/.filebridge-backup-abc-1");
        assert!(!paths.partial.contains("cpu.conf"));
        assert!(plan_save_paths("/", "abc-1").is_err());
        assert!(plan_save_paths("/etc/telegraf/cpu.conf", "../x").is_err());
        assert!(plan_save_paths("/etc/telegraf/cpu.conf", "").is_err());
        let root = plan_save_paths("/cpu.conf", "abc-1").expect("root parent");
        assert_eq!(root.partial, "/.filebridge-partial-abc-1");
    }

    #[test]
    fn sha256_is_hex_of_the_bytes() {
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("password"));
    }
}
