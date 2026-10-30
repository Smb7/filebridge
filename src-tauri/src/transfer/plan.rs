//! Where a copy is allowed to go. This never opens a socket, never reads
//! file bytes, and never builds a shell command. Remote paths stay SFTP strings.

use crate::fs::path::{self, PathError};

const PARTIAL_PREFIX: &str = ".filebridge-partial-";
const BACKUP_PREFIX: &str = ".filebridge-backup-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRequest {
    pub from_session: String,
    pub from_path: String,
    pub to_session: String,
    pub to_path: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub from_session: String,
    pub from_path: String,
    pub to_session: String,
    pub to_path: String,
    pub partial_path: String,
    pub backup_path: String,
    pub overwrite: bool,
    pub same_session: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    InvalidPath,
    SamePath,
    NotAllowed,
}

impl From<PathError> for PlanError {
    fn from(_: PathError) -> Self {
        Self::InvalidPath
    }
}

pub fn plan_transfer(request: &TransferRequest, id: &str) -> Result<TransferPlan, PlanError> {
    if request.from_session.is_empty() || request.to_session.is_empty() || !safe_id(id) {
        return Err(PlanError::NotAllowed);
    }
    let from_path = path::normalize_remote_path(&request.from_path)?;
    let to_path = path::normalize_remote_path(&request.to_path)?;
    if from_path == "/" || to_path == "/" {
        return Err(PlanError::NotAllowed);
    }
    let same_session = request.from_session == request.to_session;
    if same_session && from_path == to_path {
        return Err(PlanError::SamePath);
    }
    let parent = parent_of(&to_path);
    let partial_path = join_generated(&parent, PARTIAL_PREFIX, id);
    let backup_path = join_generated(&parent, BACKUP_PREFIX, id);
    if partial_path == to_path
        || partial_path == from_path
        || backup_path == to_path
        || backup_path == from_path
        || partial_path == backup_path
    {
        return Err(PlanError::NotAllowed);
    }
    Ok(TransferPlan {
        from_session: request.from_session.clone(),
        from_path,
        to_session: request.to_session.clone(),
        to_path,
        partial_path,
        backup_path,
        overwrite: request.overwrite,
        same_session,
    })
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

fn join_generated(parent: &str, prefix: &str, id: &str) -> String {
    let name = format!("{prefix}{id}");
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::{PlanError, TransferRequest, plan_transfer};

    fn request(from: &str, to: &str, overwrite: bool) -> TransferRequest {
        TransferRequest {
            from_session: "ses-a".to_string(),
            from_path: from.to_string(),
            to_session: "ses-b".to_string(),
            to_path: to.to_string(),
            overwrite,
        }
    }

    #[test]
    fn plans_a_cross_server_file_copy_without_using_the_file_name_in_the_partial() {
        let plan = plan_transfer(
            &request(
                "/etc/telegraf/cpu.conf; rm -rf x",
                " /var/telegraf/cpu.conf; rm -rf x ",
                false,
            ),
            "transfer-1",
        )
        .expect("plan");
        assert_eq!(plan.from_path, "/etc/telegraf/cpu.conf; rm -rf x");
        assert_eq!(plan.to_path, "/var/telegraf/cpu.conf; rm -rf x");
        assert_eq!(
            plan.partial_path,
            "/var/telegraf/.filebridge-partial-transfer-1"
        );
        assert_eq!(
            plan.backup_path,
            "/var/telegraf/.filebridge-backup-transfer-1"
        );
        assert!(!plan.same_session);
        assert!(!plan.overwrite);
        assert!(!plan.partial_path.contains("rm"));
        assert!(!plan.partial_path.contains(';'));
        assert!(plan.partial_path.starts_with('/'));
        assert!(!plan.partial_path.contains('\\'));
        let rendered = format!("{plan:?}");
        assert!(!rendered.contains("hunter2-secret"));
        assert!(!rendered.to_lowercase().contains("password"));
    }

    #[test]
    fn same_session_same_path_is_a_rename_prompt_not_a_copy() {
        let mut same = request("/etc/telegraf/cpu.conf", "/etc/telegraf/cpu.conf", true);
        same.to_session = same.from_session.clone();
        assert_eq!(
            plan_transfer(&same, "transfer-1").expect_err("same"),
            PlanError::SamePath
        );
        let mut dotted = request(
            "/etc/foo/../telegraf/cpu.conf",
            "/etc/telegraf/cpu.conf",
            false,
        );
        dotted.to_session = dotted.from_session.clone();
        assert_eq!(
            plan_transfer(&dotted, "transfer-1").expect_err("normalized"),
            PlanError::SamePath
        );
    }

    #[test]
    fn the_same_path_string_on_two_sessions_is_allowed() {
        let plan = plan_transfer(
            &request("/etc/telegraf/cpu.conf", "/etc/telegraf/cpu.conf", false),
            "transfer-1",
        )
        .expect("two servers");
        assert!(!plan.same_session);
        assert_eq!(plan.from_path, plan.to_path);
    }

    #[test]
    fn rejects_relative_nul_root_and_a_bad_id() {
        assert_eq!(
            plan_transfer(&request("cpu.conf", "/tmp/cpu.conf", false), "transfer-1")
                .expect_err("relative"),
            PlanError::InvalidPath
        );
        let nul = plan_transfer(
            &request("/etc/hunter2-secret\0.conf", "/tmp/cpu.conf", false),
            "transfer-1",
        )
        .expect_err("nul");
        assert_eq!(nul, PlanError::InvalidPath);
        assert!(!format!("{nul:?}").contains("hunter2-secret"));
        assert_eq!(
            plan_transfer(&request("/etc/cpu.conf", "/", false), "transfer-1").expect_err("root"),
            PlanError::NotAllowed
        );
        assert_eq!(
            plan_transfer(&request("/", "/tmp/cpu.conf", false), "transfer-1").expect_err("source"),
            PlanError::NotAllowed
        );
        assert_eq!(
            plan_transfer(&request("/etc/cpu.conf", "/tmp/cpu.conf", false), "nope/id")
                .expect_err("slash id"),
            PlanError::NotAllowed
        );
        assert_eq!(
            plan_transfer(&request("/etc/cpu.conf", "/tmp/cpu.conf", false), "")
                .expect_err("empty"),
            PlanError::NotAllowed
        );
        let mut missing = request("/etc/cpu.conf", "/tmp/cpu.conf", false);
        missing.from_session.clear();
        assert_eq!(
            plan_transfer(&missing, "transfer-1").expect_err("session"),
            PlanError::NotAllowed
        );
    }

    #[test]
    fn a_copy_inside_one_session_keeps_independent_paths() {
        let mut request = request("/etc/telegraf/cpu.conf", "/var/backups/cpu.conf", true);
        request.to_session = request.from_session.clone();
        let plan = plan_transfer(&request, "abc").expect("rename across dirs");
        assert!(plan.same_session);
        assert!(plan.overwrite);
        assert_eq!(plan.partial_path, "/var/backups/.filebridge-partial-abc");
        assert_ne!(plan.from_path, plan.to_path);
    }
}
