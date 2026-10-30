//! File-operation policy. These decisions never build a shell command and
//! never walk into a directory. A non-empty folder is refused, not emptied.

use super::{LinkError, RawMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteAction {
    /// `SSH_FXP_REMOVE`. Used for files, symlinks, and other non-directories.
    /// A symlink is removed as a link. The target is not followed.
    RemoveFile,
    /// `SSH_FXP_RMDIR` of a directory that had no entries other than `.` and `..`.
    RemoveEmptyDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenamePlan {
    Unchanged,
    Proceed,
}

/// russh-sftp treats the directory mode bit with `contains`. The symlink mode
/// includes that bit, so a link can report `is_dir` as well as `is_symlink`.
/// Only a non-link directory may be removed with `rmdir`.
pub(crate) fn is_real_directory(meta: &RawMeta) -> bool {
    meta.is_dir && !meta.is_symlink
}

pub(crate) fn is_dir_occupant(name: &str) -> bool {
    let trimmed = name.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    !name.is_empty() && name != "." && name != ".."
}

pub(crate) fn plan_mkdir(path: &str) -> Result<(), LinkError> {
    if path == "/" {
        Err(LinkError::NotAllowed)
    } else {
        Ok(())
    }
}

pub(crate) fn plan_rename(from: &str, to: &str) -> Result<RenamePlan, LinkError> {
    if from == "/" || to == "/" {
        return Err(LinkError::NotAllowed);
    }
    if from == to {
        return Ok(RenamePlan::Unchanged);
    }
    // `/etc` into `/etc/conf.d` is refused. `/etc` into `/etc2` is not.
    if to.starts_with(&format!("{from}/")) {
        return Err(LinkError::NotAllowed);
    }
    Ok(RenamePlan::Proceed)
}

/// Children are only consulted for a real directory. A non-empty directory
/// returns [`LinkError::NotEmpty`] and no child path. There is no recursive arm.
pub(crate) fn plan_delete(
    path: &str,
    meta: &RawMeta,
    child_names: &[String],
) -> Result<DeleteAction, LinkError> {
    if path == "/" {
        return Err(LinkError::NotAllowed);
    }
    if !is_real_directory(meta) {
        return Ok(DeleteAction::RemoveFile);
    }
    if child_names.iter().any(|name| is_dir_occupant(name)) {
        return Err(LinkError::NotEmpty);
    }
    Ok(DeleteAction::RemoveEmptyDir)
}

#[cfg(test)]
mod tests {
    use super::{
        DeleteAction, RenamePlan, is_dir_occupant, is_real_directory, plan_delete, plan_mkdir,
        plan_rename,
    };
    use crate::session::{LinkError, RawMeta};

    fn meta(is_dir: bool, is_symlink: bool) -> RawMeta {
        RawMeta {
            is_dir,
            is_file: !is_dir && !is_symlink,
            is_symlink,
            size: 1,
            modified: None,
        }
    }

    #[test]
    fn mkdir_and_rename_refuse_root_and_a_folder_moved_into_itself() {
        assert!(matches!(plan_mkdir("/"), Err(LinkError::NotAllowed)));
        assert!(plan_mkdir("/etc/telegraf/conf.d").is_ok());

        assert!(matches!(
            plan_rename("/", "/etc"),
            Err(LinkError::NotAllowed)
        ));
        assert!(matches!(
            plan_rename("/etc/telegraf", "/"),
            Err(LinkError::NotAllowed)
        ));
        assert!(matches!(
            plan_rename("/etc/telegraf", "/etc/telegraf/conf.d"),
            Err(LinkError::NotAllowed)
        ));
        assert_eq!(
            plan_rename("/etc/telegraf", "/etc/telegraf").expect("same"),
            RenamePlan::Unchanged
        );
        assert_eq!(
            plan_rename("/etc", "/etc2").expect("prefix is not a child"),
            RenamePlan::Proceed
        );
        assert_eq!(
            plan_rename("/etc/telegraf/cpu.conf", "/etc/telegraf/mem.conf").expect("rename"),
            RenamePlan::Proceed
        );
    }

    #[test]
    fn delete_is_non_recursive_and_does_not_follow_a_symlink() {
        assert!(matches!(
            plan_delete("/", &meta(true, false), &[]),
            Err(LinkError::NotAllowed)
        ));

        let file = plan_delete("/etc/telegraf/cpu.conf", &meta(false, false), &[]).expect("file");
        assert_eq!(file, DeleteAction::RemoveFile);

        // The symlink bit quirk: is_dir can be true. Children are ignored.
        let link = meta(true, true);
        assert!(!is_real_directory(&link));
        let action = plan_delete(
            "/etc/telegraf/link",
            &link,
            &["cpu.conf".to_string(), "nested".to_string()],
        )
        .expect("symlink");
        assert_eq!(action, DeleteAction::RemoveFile);

        assert!(is_dir_occupant("cpu.conf"));
        assert!(is_dir_occupant("../../etc/passwd"));
        assert!(!is_dir_occupant("."));
        assert!(!is_dir_occupant(".."));
        assert!(!is_dir_occupant(""));

        let empty = plan_delete(
            "/etc/telegraf/conf.d",
            &meta(true, false),
            &[".".to_string(), "..".to_string()],
        )
        .expect("empty");
        assert_eq!(empty, DeleteAction::RemoveEmptyDir);

        let occupied = plan_delete(
            "/etc/telegraf/conf.d",
            &meta(true, false),
            &[".".to_string(), "cpu.conf".to_string()],
        )
        .expect_err("occupied");
        assert!(matches!(occupied, LinkError::NotEmpty));
        let rendered = format!("{occupied:?}");
        assert!(!rendered.contains("cpu.conf"));
        assert!(!rendered.to_lowercase().contains("sudo"));
    }
}
