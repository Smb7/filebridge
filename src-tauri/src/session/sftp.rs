use std::future::Future;

use tokio::sync::Mutex;

#[derive(Debug, PartialEq, Eq)]
pub struct PathError {
    pub message: &'static str,
}

/// Remote paths are SFTP strings. `..` is legal. A relative path is not,
/// because FileBridge does not depend on a server-side working directory.
pub fn require_absolute_remote_path(path: &str) -> Result<String, PathError> {
    if path.is_empty() || path.contains('\0') {
        return Err(PathError {
            message: "Remote path is not valid.",
        });
    }
    if !path.starts_with('/') {
        return Err(PathError {
            message: "Remote path must be absolute.",
        });
    }
    Ok(path.to_string())
}

/// One SFTP channel per SSH connection. Callers wait here so two panes cannot
/// interleave requests on that channel.
pub struct SftpGate {
    lock: Mutex<()>,
}

impl SftpGate {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }

    pub async fn run<R, F, Fut>(&self, path: &str, op: F) -> Result<R, PathError>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = R>,
    {
        let path = require_absolute_remote_path(path)?;
        let _guard = self.lock.lock().await;
        Ok(op(path).await)
    }

    pub async fn run_two<R, F, Fut>(&self, from: &str, to: &str, op: F) -> Result<R, PathError>
    where
        F: FnOnce(String, String) -> Fut,
        Fut: Future<Output = R>,
    {
        let from = require_absolute_remote_path(from)?;
        let to = require_absolute_remote_path(to)?;
        let _guard = self.lock.lock().await;
        Ok(op(from, to).await)
    }

    /// Serialize a call that already has an open handle. The path was checked
    /// when that handle was opened.
    pub async fn exclusive<R, F, Fut>(&self, op: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        let _guard = self.lock.lock().await;
        op().await
    }
}

impl Default for SftpGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{SftpGate, require_absolute_remote_path};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn absolute_paths_allow_dotdot_and_reject_relative_or_nul() {
        assert!(require_absolute_remote_path("/etc/telegraf").is_ok());
        assert!(require_absolute_remote_path("/etc/telegraf/../telegraf.conf").is_ok());
        assert!(require_absolute_remote_path("etc/telegraf").is_err());
        assert!(require_absolute_remote_path(".").is_err());
        assert!(require_absolute_remote_path("..").is_err());
        assert!(require_absolute_remote_path("").is_err());
        assert!(require_absolute_remote_path("/etc/\0telegraf").is_err());
    }

    #[tokio::test]
    async fn calls_do_not_overlap_and_a_bad_path_does_not_run() {
        let gate = Arc::new(SftpGate::new());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let ran_bad = Arc::new(AtomicUsize::new(0));

        let bad = ran_bad.clone();
        let rejected = gate
            .run("relative.conf", |_| async move {
                bad.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert!(rejected.is_err());
        assert_eq!(ran_bad.load(Ordering::SeqCst), 0);

        let mut tasks = Vec::new();
        for _ in 0..2 {
            let gate = gate.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            tasks.push(tokio::spawn(async move {
                gate.run("/etc/telegraf", |_| async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                })
                .await
                .expect("absolute path");
            }));
        }
        for task in tasks {
            task.await.expect("task");
        }
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_bad_second_path_does_not_run() {
        let gate = SftpGate::new();
        let ran = AtomicUsize::new(0);
        let rejected = gate
            .run_two("/etc/telegraf", "relative.conf", |_, _| async {
                ran.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert!(rejected.is_err());
        let nul = gate
            .run_two("/etc/\0telegraf", "/etc/other", |_, _| async {
                ran.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert!(nul.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        gate.run_two("/etc/from", "/etc/to", |from, to| async move {
            assert_eq!(from, "/etc/from");
            assert_eq!(to, "/etc/to");
        })
        .await
        .expect("pair");
    }
}
