//! Remote paths are SFTP strings. They are never passed to local `std::fs`.
//! `..` is legal and resolved lexically so a pane does not depend on a
//! server-side working directory.

const MAX_REMOTE_PATH: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub struct PathError {
    pub message: &'static str,
}

pub fn normalize_remote_path(input: &str) -> Result<String, PathError> {
    let trimmed = input.trim();
    if trimmed.is_empty() || !trimmed.starts_with('/') {
        return Err(PathError {
            message: "Remote path must be absolute.",
        });
    }
    if trimmed.len() > MAX_REMOTE_PATH {
        return Err(PathError {
            message: "Remote path is too long.",
        });
    }
    if trimmed.chars().any(char::is_control) {
        return Err(PathError {
            message: "Remote path is not valid.",
        });
    }

    let mut parts = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            parts.pop();
            continue;
        }
        parts.push(segment);
    }
    if parts.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Last path segment of a directory listing name. A server that sends a full
/// path, or a name containing `..`, cannot escape the directory being listed.
pub fn listable_name(raw: &str) -> Option<&str> {
    let trimmed = raw.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if !is_listable_name(name) {
        return None;
    }
    Some(name)
}

pub fn join_remote(parent: &str, name: &str) -> Option<String> {
    let name = listable_name(name)?;
    if parent == "/" {
        Some(format!("/{name}"))
    } else {
        Some(format!("{parent}/{name}"))
    }
}

fn is_listable_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.len() > 255 {
        return false;
    }
    !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::{join_remote, listable_name, normalize_remote_path};

    #[test]
    fn normalizes_absolute_paths_and_rejects_relative_or_nul() {
        assert_eq!(
            normalize_remote_path(" /etc/telegraf/ ").expect("path"),
            "/etc/telegraf"
        );
        assert_eq!(
            normalize_remote_path("/etc/foo/../bar").expect("dotdot"),
            "/etc/bar"
        );
        assert_eq!(
            normalize_remote_path("/etc//./telegraf/").expect("cleanup"),
            "/etc/telegraf"
        );
        assert_eq!(normalize_remote_path("/../etc").expect("root"), "/etc");
        assert_eq!(normalize_remote_path("/").expect("slash"), "/");

        let relative = normalize_remote_path("etc/telegraf").expect_err("relative");
        assert_eq!(relative.message, "Remote path must be absolute.");
        assert!(normalize_remote_path("").is_err());
        assert!(normalize_remote_path("   ").is_err());

        let secret = normalize_remote_path("/etc/hunter2-secret\0conf").expect_err("nul");
        assert_eq!(secret.message, "Remote path is not valid.");
        assert!(!secret.message.contains("hunter2-secret"));
        assert!(!format!("{secret:?}").contains("hunter2-secret"));
    }

    #[test]
    fn a_listing_name_cannot_escape_the_directory() {
        assert_eq!(listable_name("telegraf.conf"), Some("telegraf.conf"));
        assert_eq!(listable_name("/etc/telegraf/cpu.conf"), Some("cpu.conf"));
        assert_eq!(listable_name("../../etc/passwd"), Some("passwd"));
        assert_eq!(
            join_remote("/etc/telegraf", "../../etc/passwd"),
            Some("/etc/telegraf/passwd".to_string())
        );
        assert_eq!(join_remote("/", "cpu.conf"), Some("/cpu.conf".to_string()));
        assert_eq!(join_remote("/etc", "."), None);
        assert_eq!(join_remote("/etc", ".."), None);
        assert_eq!(join_remote("/etc", ""), None);
        assert_eq!(join_remote("/etc", "a\0b"), None);
    }
}
