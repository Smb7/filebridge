pub(crate) mod path;

use serde::Serialize;

use crate::session::{LinkError, RawFile, RawMeta, SessionState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

impl From<LinkError> for CommandError {
    fn from(error: LinkError) -> Self {
        let (code, message) = match error {
            LinkError::InvalidPath => ("invalid_path", "Remote path must be absolute."),
            LinkError::NotConnected => ("not_connected", "That server is not connected."),
            LinkError::NotFound => ("not_found", "That path does not exist."),
            LinkError::PermissionDenied => ("permission_denied", "Permission denied."),
            LinkError::NotEmpty => ("not_empty", "That folder is not empty."),
            LinkError::AlreadyExists => ("already_exists", "That name already exists."),
            LinkError::NotAllowed => ("not_allowed", "That path cannot be changed."),
            LinkError::Failed => ("sftp_failed", "Could not read that path."),
        };
        Self {
            code,
            message: message.to_string(),
        }
    }
}

fn mutation_error(error: LinkError) -> CommandError {
    match error {
        LinkError::Failed => CommandError {
            code: "sftp_failed",
            message: "Could not change that path.".to_string(),
        },
        other => CommandError::from(other),
    }
}

impl From<path::PathError> for CommandError {
    fn from(error: path::PathError) -> Self {
        Self {
            code: "invalid_path",
            message: error.message.to_string(),
        }
    }
}

pub fn kind_from_flags(is_dir: bool, is_file: bool, is_symlink: bool) -> EntryKind {
    if is_symlink {
        EntryKind::Symlink
    } else if is_dir {
        EntryKind::Dir
    } else if is_file {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

pub fn prepare_listing(parent: &str, raw: Vec<RawFile>) -> Vec<FsEntry> {
    let mut entries = Vec::new();
    for file in raw {
        let Some(name) = path::listable_name(&file.name) else {
            continue;
        };
        let Some(full) = path::join_remote(parent, name) else {
            continue;
        };
        entries.push(FsEntry {
            name: name.to_string(),
            path: full,
            kind: kind_from_flags(file.meta.is_dir, file.meta.is_file, file.meta.is_symlink),
            size: file.meta.size,
            modified: file.meta.modified,
        });
    }
    entries.sort_by(cmp_entries);
    entries
}

pub fn entry_from_meta(path: &str, meta: RawMeta) -> FsEntry {
    FsEntry {
        name: name_of(path),
        path: path.to_string(),
        kind: kind_from_flags(meta.is_dir, meta.is_file, meta.is_symlink),
        size: meta.size,
        modified: meta.modified,
    }
}

fn name_of(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn cmp_entries(left: &FsEntry, right: &FsEntry) -> std::cmp::Ordering {
    dir_rank(left.kind)
        .cmp(&dir_rank(right.kind))
        .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        .then_with(|| left.name.cmp(&right.name))
}

fn dir_rank(kind: EntryKind) -> u8 {
    if kind == EntryKind::Dir { 0 } else { 1 }
}

#[tauri::command]
pub async fn fs_list(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    path: String,
) -> Result<Vec<FsEntry>, CommandError> {
    let path = path::normalize_remote_path(&path)?;
    let raw = sessions.read_dir(&session_id, &path).await?;
    Ok(prepare_listing(&path, raw))
}

#[tauri::command]
pub async fn fs_stat(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    path: String,
) -> Result<FsEntry, CommandError> {
    let path = path::normalize_remote_path(&path)?;
    let meta = sessions.stat_path(&session_id, &path).await?;
    Ok(entry_from_meta(&path, meta))
}

#[tauri::command]
pub async fn fs_mkdir(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    path: String,
) -> Result<(), CommandError> {
    let path = path::normalize_remote_path(&path)?;
    sessions
        .create_dir(&session_id, &path)
        .await
        .map_err(mutation_error)
}

#[tauri::command]
pub async fn fs_rename(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    from: String,
    to: String,
) -> Result<(), CommandError> {
    let from = path::normalize_remote_path(&from)?;
    let to = path::normalize_remote_path(&to)?;
    sessions
        .rename_path(&session_id, &from, &to)
        .await
        .map_err(mutation_error)
}

#[tauri::command]
pub async fn fs_delete(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
    path: String,
) -> Result<(), CommandError> {
    let path = path::normalize_remote_path(&path)?;
    sessions
        .remove_path(&session_id, &path)
        .await
        .map_err(mutation_error)
}

#[cfg(test)]
mod tests {
    use super::{
        CommandError, EntryKind, entry_from_meta, kind_from_flags, mutation_error, prepare_listing,
    };
    use crate::session::{LinkError, RawFile, RawMeta};

    fn file(name: &str, is_dir: bool) -> RawFile {
        RawFile {
            name: name.to_string(),
            meta: RawMeta {
                is_dir,
                is_file: !is_dir,
                is_symlink: false,
                size: if is_dir { 0 } else { 12 },
                modified: None,
            },
        }
    }

    #[test]
    fn listing_skips_unsafe_names_and_sorts_directories_first() {
        let entries = prepare_listing(
            "/etc/telegraf",
            vec![
                file("zeta.conf", false),
                file("conf.d", true),
                file(".", false),
                file("..", false),
                file("Alpha.conf", false),
                file("bad\0name", false),
                file("../../etc/passwd", false),
                RawFile {
                    name: "link".to_string(),
                    meta: RawMeta {
                        is_dir: true,
                        is_file: false,
                        is_symlink: true,
                        size: 0,
                        modified: Some(10),
                    },
                },
            ],
        );
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["conf.d", "Alpha.conf", "link", "passwd", "zeta.conf"]
        );
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert_eq!(entries[0].path, "/etc/telegraf/conf.d");
        assert_eq!(entries[3].path, "/etc/telegraf/passwd");
        assert_eq!(entries[2].kind, EntryKind::Symlink);
        assert!(kind_from_flags(true, false, true) == EntryKind::Symlink);
    }

    #[test]
    fn errors_are_safe_to_show() {
        for error in [
            LinkError::InvalidPath,
            LinkError::NotConnected,
            LinkError::NotFound,
            LinkError::PermissionDenied,
            LinkError::NotEmpty,
            LinkError::AlreadyExists,
            LinkError::NotAllowed,
            LinkError::Failed,
        ] {
            let command = CommandError::from(error);
            let rendered = format!("{} {}", command.code, command.message);
            assert!(!rendered.contains("hunter2-secret"));
            assert!(!rendered.to_lowercase().contains("password"));
            assert!(!rendered.to_lowercase().contains("passphrase"));
            assert!(!rendered.to_lowercase().contains("private"));
            assert!(!rendered.to_lowercase().contains("sudo"));
        }
    }

    #[test]
    fn a_change_is_shown_as_returned_and_does_not_offer_sudo() {
        let denied = mutation_error(LinkError::PermissionDenied);
        assert_eq!(denied.code, "permission_denied");
        assert_eq!(denied.message, "Permission denied.");
        assert!(!denied.message.to_lowercase().contains("sudo"));

        let failed = mutation_error(LinkError::Failed);
        assert_eq!(failed.code, "sftp_failed");
        assert_eq!(failed.message, "Could not change that path.");
        assert!(!failed.message.contains("hunter2-secret"));

        let occupied = mutation_error(LinkError::NotEmpty);
        assert_eq!(occupied.code, "not_empty");
        assert_eq!(occupied.message, "That folder is not empty.");
    }

    #[test]
    fn stat_uses_the_path_name_and_does_not_read_file_bytes() {
        let entry = entry_from_meta(
            "/etc/telegraf/telegraf.conf",
            RawMeta {
                is_dir: false,
                is_file: true,
                is_symlink: false,
                size: 42,
                modified: Some(9),
            },
        );
        assert_eq!(entry.name, "telegraf.conf");
        assert_eq!(entry.kind, EntryKind::File);
        assert_eq!(entry.size, 42);
        assert_eq!(
            entry_from_meta(
                "/",
                RawMeta {
                    is_dir: true,
                    is_file: false,
                    is_symlink: false,
                    size: 0,
                    modified: None,
                }
            )
            .name,
            "/"
        );
    }
}
