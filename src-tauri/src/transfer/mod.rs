mod copy;
mod plan;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::session::{LinkError, RawMeta, SessionState};

use copy::{COPY_CHUNK, ChunkIo, CopyError, stream_copy};
use plan::{PlanError, TransferPlan, TransferRequest, plan_transfer};

pub const PROGRESS_EVENT: &str = "transfer://progress";

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferStarted {
    pub transfer_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferProgress {
    id: String,
    bytes: u64,
    total: u64,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

pub struct TransferState {
    jobs: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TransferState {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
        }
    }

    fn track(&self, id: &str, cancel: Arc<AtomicBool>) -> Result<(), CommandError> {
        self.jobs
            .lock()
            .map_err(|_| internal_error())?
            .insert(id.to_string(), cancel);
        Ok(())
    }

    fn finish(&self, id: &str) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.remove(id);
        }
    }

    fn cancel(&self, id: &str) -> Result<(), CommandError> {
        let jobs = self.jobs.lock().map_err(|_| internal_error())?;
        let Some(flag) = jobs.get(id) else {
            return Err(CommandError {
                code: "not_found",
                message: "That copy is not running.".to_string(),
            });
        };
        flag.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Default for TransferState {
    fn default() -> Self {
        Self::new()
    }
}

struct LiveIo<'a> {
    sessions: &'a SessionState,
    session_id: String,
}

impl ChunkIo for LiveIo<'_> {
    fn lstat<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RawMeta, LinkError>> + Send + 'a>> {
        let path = path.to_string();
        Box::pin(async move { self.sessions.transfer_lstat(&self.session_id, &path).await })
    }

    fn open_read<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, LinkError>> + Send + 'a>> {
        let path = path.to_string();
        Box::pin(async move {
            self.sessions
                .transfer_open_read(&self.session_id, &path)
                .await
        })
    }

    fn read_at<'a>(
        &'a self,
        token: &'a str,
        max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, LinkError>> + Send + 'a>> {
        let token = token.to_string();
        Box::pin(async move {
            self.sessions
                .transfer_read(&self.session_id, &token, max)
                .await
        })
    }

    fn open_write<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, LinkError>> + Send + 'a>> {
        let path = path.to_string();
        Box::pin(async move {
            self.sessions
                .transfer_open_write(&self.session_id, &path)
                .await
        })
    }

    fn write_at<'a>(
        &'a self,
        token: &'a str,
        data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        let token = token.to_string();
        let data = data.to_vec();
        Box::pin(async move {
            self.sessions
                .transfer_write(&self.session_id, &token, &data)
                .await
        })
    }

    fn close_file<'a>(
        &'a self,
        token: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        let token = token.to_string();
        Box::pin(async move { self.sessions.transfer_close(&self.session_id, &token).await })
    }

    fn remove_file<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        let path = path.to_string();
        Box::pin(async move { self.sessions.transfer_remove(&self.session_id, &path).await })
    }

    fn rename_file<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        let from = from.to_string();
        let to = to.to_string();
        Box::pin(async move {
            self.sessions
                .transfer_rename(&self.session_id, &from, &to)
                .await
        })
    }
}

fn plan_error(error: PlanError) -> CommandError {
    match error {
        PlanError::InvalidPath => CommandError {
            code: "invalid_path",
            message: "Remote path must be absolute.".to_string(),
        },
        PlanError::SamePath => CommandError {
            code: "same_path",
            message: "Choose a different name.".to_string(),
        },
        PlanError::NotAllowed => CommandError {
            code: "not_allowed",
            message: "That path cannot be changed.".to_string(),
        },
    }
}

fn copy_error(error: CopyError) -> (&'static str, String) {
    let (code, message) = match error {
        CopyError::InvalidPath => ("invalid_path", "Remote path must be absolute."),
        CopyError::NotConnected => ("not_connected", "That server is not connected."),
        CopyError::NotFound => ("not_found", "That file does not exist."),
        CopyError::PermissionDenied => ("permission_denied", "Permission denied."),
        CopyError::AlreadyExists => ("already_exists", "That name already exists."),
        CopyError::NotAFile => ("not_a_file", "Only a file can be copied."),
        CopyError::NotAllowed => ("not_allowed", "That path cannot be changed."),
        CopyError::SamePath => ("same_path", "Choose a different name."),
        CopyError::Cancelled => ("cancelled", "Copy cancelled."),
        CopyError::Failed => ("sftp_failed", "Could not copy that file."),
    };
    (code, message.to_string())
}

fn internal_error() -> CommandError {
    CommandError {
        code: "internal",
        message: "Could not copy that file.".to_string(),
    }
}

fn emit_progress(app: &AppHandle, progress: &TransferProgress) {
    let _ = app.emit(PROGRESS_EVENT, progress);
}

async fn run_copy(
    sessions: &SessionState,
    plan: &TransferPlan,
    cancel: &AtomicBool,
    app: &AppHandle,
    id: &str,
) -> Result<u64, CopyError> {
    let source = LiveIo {
        sessions,
        session_id: plan.from_session.clone(),
    };
    let dest = LiveIo {
        sessions,
        session_id: plan.to_session.clone(),
    };
    let mut last_emit = 0u64;
    let mut last_at = Instant::now();
    stream_copy(&source, &dest, plan, cancel, COPY_CHUNK, |bytes, total| {
        let due = bytes == 0
            || bytes.saturating_sub(last_emit) >= 256 * 1024
            || last_at.elapsed() >= Duration::from_millis(200);
        if !due {
            return;
        }
        last_emit = bytes;
        last_at = Instant::now();
        emit_progress(
            app,
            &TransferProgress {
                id: id.to_string(),
                bytes,
                total,
                state: "running",
                code: None,
                message: None,
            },
        );
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartArgs {
    from_session: String,
    from_path: String,
    to_session: String,
    to_path: String,
    overwrite: bool,
}

#[tauri::command]
pub async fn transfer_start(
    app: AppHandle,
    transfers: tauri::State<'_, TransferState>,
    from_session: String,
    from_path: String,
    to_session: String,
    to_path: String,
    overwrite: bool,
) -> Result<TransferStarted, CommandError> {
    let args = StartArgs {
        from_session,
        from_path,
        to_session,
        to_path,
        overwrite,
    };
    let id = uuid::Uuid::new_v4().to_string();
    let plan = plan_transfer(
        &TransferRequest {
            from_session: args.from_session,
            from_path: args.from_path,
            to_session: args.to_session,
            to_path: args.to_path,
            overwrite: args.overwrite,
        },
        &id,
    )
    .map_err(plan_error)?;
    let cancel = Arc::new(AtomicBool::new(false));
    transfers.track(&id, cancel.clone())?;
    let transfer_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let emit_app = app.clone();
        let result = {
            let sessions = app.state::<SessionState>();
            run_copy(&sessions, &plan, &cancel, &emit_app, &transfer_id).await
        };
        if let Some(state) = app.try_state::<TransferState>() {
            state.finish(&transfer_id);
        }
        let (state, bytes, total, code, message) = match result {
            Ok(bytes) => ("done", bytes, bytes, None, None),
            Err(CopyError::Cancelled) => (
                "cancelled",
                0,
                0,
                Some("cancelled"),
                Some("Copy cancelled.".to_string()),
            ),
            Err(error) => {
                let (code, message) = copy_error(error);
                ("failed", 0, 0, Some(code), Some(message))
            }
        };
        emit_progress(
            &emit_app,
            &TransferProgress {
                id: transfer_id,
                bytes,
                total,
                state,
                code,
                message,
            },
        );
    });
    Ok(TransferStarted { transfer_id: id })
}

#[tauri::command]
pub fn transfer_cancel(
    transfers: tauri::State<'_, TransferState>,
    transfer_id: String,
) -> Result<(), CommandError> {
    transfers.cancel(&transfer_id)
}

#[cfg(test)]
mod tests {
    use super::{copy_error, plan_error};
    use crate::transfer::copy::CopyError;
    use crate::transfer::plan::PlanError;

    #[test]
    fn errors_are_safe_to_show_and_do_not_offer_sudo() {
        for error in [
            CopyError::InvalidPath,
            CopyError::NotConnected,
            CopyError::NotFound,
            CopyError::PermissionDenied,
            CopyError::AlreadyExists,
            CopyError::NotAFile,
            CopyError::NotAllowed,
            CopyError::SamePath,
            CopyError::Cancelled,
            CopyError::Failed,
        ] {
            let (code, message) = copy_error(error);
            let rendered = format!("{code} {message}");
            assert!(!rendered.contains("hunter2-secret"));
            assert!(!rendered.to_lowercase().contains("password"));
            assert!(!rendered.to_lowercase().contains("passphrase"));
            assert!(!rendered.to_lowercase().contains("private"));
            assert!(!rendered.to_lowercase().contains("sudo"));
        }
        for error in [
            PlanError::InvalidPath,
            PlanError::SamePath,
            PlanError::NotAllowed,
        ] {
            let command = plan_error(error);
            assert!(!command.message.to_lowercase().contains("sudo"));
            assert!(!format!("{command:?}").contains("hunter2-secret"));
        }
    }
}
