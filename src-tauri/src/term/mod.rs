mod policy;

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use zeroize::Zeroize;

use crate::session::shell::{OpenedShell, ShellBytes};
use crate::session::{LinkError, SessionState};

use policy::{InputError, OpenDecision, PtySlot, decode_input, encode_output, plan_login_shell};

pub const DATA_EVENT: &str = "term://data";
pub const CLOSED_EVENT: &str = "term://closed";

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TermOpened {
    pub term_id: String,
}

#[derive(Serialize)]
struct TermData {
    id: String,
    data: String,
}

impl std::fmt::Debug for TermData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermData")
            .field("id", &self.id)
            .field("data", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Serialize)]
struct TermClosed {
    id: String,
}

#[derive(Debug)]
enum TermError {
    NotConnected,
    NotFound,
    ShellUnavailable,
    InvalidSize,
    TooLarge,
    InvalidInput,
    Closed,
    Internal,
}

impl From<TermError> for CommandError {
    fn from(error: TermError) -> Self {
        let (code, message) = match error {
            TermError::NotConnected | TermError::Closed => {
                ("not_connected", "That server is not connected.")
            }
            TermError::NotFound => ("not_found", "That terminal is not open."),
            TermError::ShellUnavailable => {
                ("shell_unavailable", "The server did not open a shell.")
            }
            TermError::InvalidSize => ("invalid_size", "Terminal size is not valid."),
            TermError::TooLarge => ("too_large", "That input is too large."),
            TermError::InvalidInput => ("invalid_input", "That input could not be sent."),
            TermError::Internal => ("internal", "Could not open a shell."),
        };
        Self {
            code,
            message: message.to_string(),
        }
    }
}

fn from_link(error: LinkError) -> TermError {
    match error {
        LinkError::NotConnected => TermError::NotConnected,
        _ => TermError::ShellUnavailable,
    }
}

fn from_input(error: InputError) -> TermError {
    match error {
        InputError::TooLarge => TermError::TooLarge,
        InputError::Invalid => TermError::InvalidInput,
    }
}

trait TermSink: Send + Sync {
    fn data(&self, id: &str, bytes: &[u8]);
    fn closed(&self, id: &str);
}

struct AppSink(AppHandle);

impl TermSink for AppSink {
    fn data(&self, id: &str, bytes: &[u8]) {
        let event = TermData {
            id: id.to_string(),
            data: encode_output(bytes),
        };
        let _ = self.0.emit(DATA_EVENT, &event);
    }

    fn closed(&self, id: &str) {
        let _ = self
            .0
            .emit(CLOSED_EVENT, &TermClosed { id: id.to_string() });
    }
}

trait ShellHost: Send + Sync {
    fn open(
        &self,
        session_id: &str,
        cols: u32,
        rows: u32,
    ) -> impl Future<Output = Result<OpenedShell, LinkError>> + Send;
}

struct SessionHost<'a> {
    sessions: &'a SessionState,
}

impl ShellHost for SessionHost<'_> {
    async fn open(&self, session_id: &str, cols: u32, rows: u32) -> Result<OpenedShell, LinkError> {
        self.sessions.open_shell(session_id, cols, rows).await
    }
}

struct LivePty {
    term_id: String,
    session_id: String,
    writer: Arc<dyn crate::session::shell::ShellWriter>,
    closed: Arc<AtomicBool>,
}

struct Registry {
    by_term: HashMap<String, LivePty>,
    by_session: HashMap<String, String>,
}

pub struct TerminalState {
    inner: Arc<Mutex<Registry>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl TerminalState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Registry {
                by_term: HashMap::new(),
                by_session: HashMap::new(),
            })),
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn session_lock(&self, session_id: &str) -> Result<Arc<tokio::sync::Mutex<()>>, TermError> {
        let mut locks = self.locks.lock().map_err(|_| TermError::Internal)?;
        Ok(locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    async fn open(
        &self,
        sink: Arc<dyn TermSink>,
        host: &impl ShellHost,
        session_id: &str,
        cols: u32,
        rows: u32,
    ) -> Result<String, TermError> {
        plan_login_shell(cols, rows).map_err(|_| TermError::InvalidSize)?;
        let lock = self.session_lock(session_id)?;
        let _guard = lock.lock().await;
        if let Some(existing) = self.reusable(session_id)? {
            match existing.writer.resize(cols, rows).await {
                Ok(()) => return Ok(existing.term_id),
                Err(_) => self.forget(&existing.term_id),
            }
        }
        let pipe = host.open(session_id, cols, rows).await.map_err(from_link)?;
        let term_id = uuid::Uuid::new_v4().to_string();
        let closed = Arc::new(AtomicBool::new(false));
        {
            let mut registry = self.inner.lock().map_err(|_| TermError::Internal)?;
            registry
                .by_session
                .insert(session_id.to_string(), term_id.clone());
            registry.by_term.insert(
                term_id.clone(),
                LivePty {
                    term_id: term_id.clone(),
                    session_id: session_id.to_string(),
                    writer: pipe.writer,
                    closed,
                },
            );
        }
        let inner = self.inner.clone();
        let id = term_id.clone();
        tokio::spawn(async move {
            pump(sink, inner, id, pipe.incoming).await;
        });
        Ok(term_id)
    }

    async fn write(&self, term_id: &str, data: &[u8]) -> Result<(), TermError> {
        policy::validate_input(data.len()).map_err(from_input)?;
        let writer = self.writer(term_id)?;
        writer.write(data).await.map_err(|_| TermError::Closed)
    }

    async fn resize(&self, term_id: &str, cols: u32, rows: u32) -> Result<(), TermError> {
        plan_login_shell(cols, rows).map_err(|_| TermError::InvalidSize)?;
        let writer = self.writer(term_id)?;
        writer
            .resize(cols, rows)
            .await
            .map_err(|_| TermError::Closed)
    }

    async fn close_term(&self, term_id: &str) -> Result<(), TermError> {
        let writer = self.take_term(term_id)?;
        writer.close().await;
        Ok(())
    }

    pub async fn close_session(&self, session_id: &str) {
        let Some(writer) = self.take_session(session_id) else {
            return;
        };
        writer.close().await;
    }

    fn reusable(&self, session_id: &str) -> Result<Option<Reusable>, TermError> {
        let registry = self.inner.lock().map_err(|_| TermError::Internal)?;
        let decision = policy::decide_open(&slots(&registry), session_id);
        let OpenDecision::Reuse(term_id) = decision else {
            return Ok(None);
        };
        let Some(pty) = registry.by_term.get(&term_id) else {
            return Ok(None);
        };
        Ok(Some(Reusable {
            term_id,
            writer: pty.writer.clone(),
        }))
    }

    fn writer(
        &self,
        term_id: &str,
    ) -> Result<Arc<dyn crate::session::shell::ShellWriter>, TermError> {
        let registry = self.inner.lock().map_err(|_| TermError::Internal)?;
        let pty = registry.by_term.get(term_id).ok_or(TermError::NotFound)?;
        if pty.closed.load(Ordering::SeqCst) {
            return Err(TermError::Closed);
        }
        Ok(pty.writer.clone())
    }

    fn forget(&self, term_id: &str) {
        if let Ok(mut registry) = self.inner.lock() {
            forget_locked(&mut registry, term_id);
        }
    }

    fn take_term(
        &self,
        term_id: &str,
    ) -> Result<Arc<dyn crate::session::shell::ShellWriter>, TermError> {
        let mut registry = self.inner.lock().map_err(|_| TermError::Internal)?;
        let Some(pty) = registry.by_term.remove(term_id) else {
            return Err(TermError::NotFound);
        };
        registry.by_session.remove(&pty.session_id);
        pty.closed.store(true, Ordering::SeqCst);
        Ok(pty.writer)
    }

    fn take_session(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn crate::session::shell::ShellWriter>> {
        let mut registry = self.inner.lock().ok()?;
        let term_id = registry.by_session.remove(session_id)?;
        let pty = registry.by_term.remove(&term_id)?;
        pty.closed.store(true, Ordering::SeqCst);
        Some(pty.writer)
    }
}

impl Default for TerminalState {
    fn default() -> Self {
        Self::new()
    }
}

struct Reusable {
    term_id: String,
    writer: Arc<dyn crate::session::shell::ShellWriter>,
}

fn slots(registry: &Registry) -> Vec<PtySlot> {
    registry
        .by_term
        .values()
        .map(|pty| PtySlot {
            term_id: pty.term_id.clone(),
            session_id: pty.session_id.clone(),
            closed: pty.closed.load(Ordering::SeqCst),
        })
        .collect()
}

fn forget_locked(registry: &mut Registry, term_id: &str) {
    if let Some(pty) = registry.by_term.remove(term_id) {
        pty.closed.store(true, Ordering::SeqCst);
        registry.by_session.remove(&pty.session_id);
    }
}

async fn pump(
    sink: Arc<dyn TermSink>,
    inner: Arc<Mutex<Registry>>,
    term_id: String,
    mut incoming: tokio::sync::mpsc::Receiver<ShellBytes>,
) {
    while let Some(event) = incoming.recv().await {
        match event {
            ShellBytes::Data(mut bytes) => {
                sink.data(&term_id, &bytes);
                bytes.zeroize();
            }
            ShellBytes::Closed => break,
        }
    }
    if let Ok(mut registry) = inner.lock() {
        forget_locked(&mut registry, &term_id);
    }
    sink.closed(&term_id);
}

#[tauri::command]
pub async fn term_open(
    app: AppHandle,
    sessions: tauri::State<'_, SessionState>,
    terminals: tauri::State<'_, TerminalState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<TermOpened, CommandError> {
    let term_id = terminals
        .open(
            Arc::new(AppSink(app)),
            &SessionHost {
                sessions: &sessions,
            },
            &session_id,
            cols,
            rows,
        )
        .await
        .map_err(CommandError::from)?;
    Ok(TermOpened { term_id })
}

#[tauri::command]
pub async fn term_write(
    terminals: tauri::State<'_, TerminalState>,
    term_id: String,
    mut data: String,
) -> Result<(), CommandError> {
    let decoded = decode_input(&data).map_err(from_input);
    data.zeroize();
    let bytes = zeroize::Zeroizing::new(decoded.map_err(CommandError::from)?);
    terminals
        .write(&term_id, &bytes)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn term_resize(
    terminals: tauri::State<'_, TerminalState>,
    term_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), CommandError> {
    terminals
        .resize(&term_id, cols, rows)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn term_close(
    terminals: tauri::State<'_, TerminalState>,
    term_id: String,
) -> Result<(), CommandError> {
    terminals
        .close_term(&term_id)
        .await
        .map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::{
        CommandError, ShellHost, TermError, TermSink, TerminalState, from_input, from_link,
    };
    use crate::session::LinkError;
    use crate::session::shell::{OpenedShell, ShellBytes, ShellWriter};
    use crate::term::policy::{decode_input, encode_output};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Rec {
        data: Mutex<Vec<(String, Vec<u8>)>>,
        closed: Mutex<Vec<String>>,
    }

    impl TermSink for Rec {
        fn data(&self, id: &str, bytes: &[u8]) {
            self.data
                .lock()
                .expect("data")
                .push((id.to_string(), bytes.to_vec()));
        }

        fn closed(&self, id: &str) {
            self.closed.lock().expect("closed").push(id.to_string());
        }
    }

    struct FakeWriter {
        writes: Mutex<Vec<Vec<u8>>>,
        closed: AtomicBool,
        resizes: AtomicUsize,
    }

    impl ShellWriter for FakeWriter {
        fn write<'a>(
            &'a self,
            data: &'a [u8],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            Box::pin(async move {
                if self.closed.load(Ordering::SeqCst) {
                    return Err(LinkError::NotConnected);
                }
                self.writes.lock().expect("writes").push(data.to_vec());
                Ok(())
            })
        }

        fn resize<'a>(
            &'a self,
            _cols: u32,
            _rows: u32,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            Box::pin(async move {
                if self.closed.load(Ordering::SeqCst) {
                    return Err(LinkError::NotConnected);
                }
                self.resizes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn close<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
            self.closed.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    struct FakeHost {
        opens: AtomicUsize,
        writers: Mutex<Vec<Arc<FakeWriter>>>,
        senders: Mutex<Vec<tokio::sync::mpsc::Sender<ShellBytes>>>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                opens: AtomicUsize::new(0),
                writers: Mutex::new(Vec::new()),
                senders: Mutex::new(Vec::new()),
            }
        }
    }

    impl ShellHost for FakeHost {
        async fn open(
            &self,
            _session_id: &str,
            _cols: u32,
            _rows: u32,
        ) -> Result<OpenedShell, LinkError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let writer = Arc::new(FakeWriter {
                writes: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
                resizes: AtomicUsize::new(0),
            });
            self.writers.lock().expect("writers").push(writer.clone());
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            self.senders.lock().expect("senders").push(tx);
            Ok(OpenedShell {
                writer,
                incoming: rx,
            })
        }
    }

    fn sink() -> Arc<Rec> {
        Arc::new(Rec {
            data: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
        })
    }

    #[test]
    fn errors_are_safe_to_show() {
        for error in [
            TermError::NotConnected,
            TermError::NotFound,
            TermError::ShellUnavailable,
            TermError::InvalidSize,
            TermError::TooLarge,
            TermError::InvalidInput,
            TermError::Closed,
            TermError::Internal,
        ] {
            let command = CommandError::from(error);
            let rendered = format!("{} {}", command.code, command.message);
            assert!(!rendered.contains("hunter2-secret"));
            assert!(!rendered.to_lowercase().contains("password"));
            assert!(!rendered.to_lowercase().contains("passphrase"));
            assert!(!rendered.to_lowercase().contains("private"));
            assert!(!rendered.to_lowercase().contains("sudo"));
        }
        let denied = CommandError::from(from_link(LinkError::NotConnected));
        assert_eq!(denied.code, "not_connected");
        let refused = CommandError::from(from_link(LinkError::Failed));
        assert_eq!(refused.code, "shell_unavailable");
        assert!(!refused.message.contains("hunter2-secret"));
    }

    #[test]
    fn event_debug_redacts_terminal_bytes() {
        let secret = b"hunter2-secret";
        let event = super::TermData {
            id: "term-1".to_string(),
            data: encode_output(secret),
        };
        let rendered = format!("{event:?}");
        assert!(rendered.contains("term-1"));
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("hunter2-secret"));
        assert!(!rendered.contains(&encode_output(secret)));
    }

    #[tokio::test]
    async fn each_session_keeps_its_shell_when_the_other_is_opened() {
        let state = TerminalState::new();
        let host = FakeHost::new();
        let recorded = sink();
        let first = state
            .open(recorded.clone(), &host, "ses-a", 80, 24)
            .await
            .expect("a");
        let second = state
            .open(recorded.clone(), &host, "ses-b", 80, 24)
            .await
            .expect("b");
        let again = state
            .open(recorded.clone(), &host, "ses-a", 100, 40)
            .await
            .expect("reuse");
        assert_eq!(first, again);
        assert_ne!(first, second);
        assert_eq!(host.opens.load(Ordering::SeqCst), 2);
        let writers = host.writers.lock().expect("writers");
        assert!(!writers[0].closed.load(Ordering::SeqCst));
        assert!(!writers[1].closed.load(Ordering::SeqCst));
        assert_eq!(writers[0].resizes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn closing_one_shell_does_not_close_the_other_or_echo_input() {
        let state = TerminalState::new();
        let host = FakeHost::new();
        let recorded = sink();
        let first = state
            .open(recorded.clone(), &host, "ses-a", 80, 24)
            .await
            .expect("a");
        let second = state
            .open(recorded, &host, "ses-b", 80, 24)
            .await
            .expect("b");
        state.close_session("ses-a").await;
        let secret = b"hunter2-secret";
        let missing = state.write(&first, secret).await.expect_err("closed");
        let command = CommandError::from(missing);
        assert_eq!(command.code, "not_found");
        assert!(!command.message.contains("hunter2"));
        assert!(!format!("{command:?}").contains("hunter2-secret"));
        state.write(&second, b"ls\n").await.expect("still open");
        let writers = host.writers.lock().expect("writers");
        assert!(writers[0].closed.load(Ordering::SeqCst));
        assert!(!writers[1].closed.load(Ordering::SeqCst));
        assert_eq!(writers[1].writes.lock().expect("writes")[0], b"ls\n");
        assert!(writers[0].writes.lock().expect("writes").is_empty());
    }

    #[tokio::test]
    async fn an_oversized_write_is_refused_without_the_payload() {
        let state = TerminalState::new();
        let host = FakeHost::new();
        let term_id = state
            .open(sink(), &host, "ses-a", 80, 24)
            .await
            .expect("open");
        let secret = "hunter2-secret".repeat(20_000);
        let error = state
            .write(&term_id, secret.as_bytes())
            .await
            .expect_err("too large");
        let command = CommandError::from(error);
        assert_eq!(command.code, "too_large");
        assert!(!command.message.contains("hunter2"));
        assert!(!format!("{command:?}").contains("hunter2"));
        assert!(
            host.writers.lock().expect("writers")[0]
                .writes
                .lock()
                .expect("writes")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn shell_output_is_forwarded_and_a_closed_shell_can_be_reopened() {
        let state = TerminalState::new();
        let host = FakeHost::new();
        let recorded = sink();
        let term_id = state
            .open(recorded.clone(), &host, "ses-a", 80, 24)
            .await
            .expect("open");
        let sender = host.senders.lock().expect("tx")[0].clone();
        sender
            .send(ShellBytes::Data(b"prompt".to_vec()))
            .await
            .expect("send");
        sender.send(ShellBytes::Closed).await.expect("close");
        for _ in 0..50 {
            if !recorded.closed.lock().expect("closed").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(recorded.data.lock().expect("data")[0].1, b"prompt");
        assert_eq!(
            recorded.closed.lock().expect("closed").as_slice(),
            [term_id]
        );
        let again = state
            .open(recorded, &host, "ses-a", 80, 24)
            .await
            .expect("reopen");
        assert_eq!(host.opens.load(Ordering::SeqCst), 2);
        assert!(!again.is_empty());
        let bad = decode_input("hunter2-secret").expect_err("bad");
        let command = CommandError::from(from_input(bad));
        assert!(!command.message.contains("hunter2"));
    }
}
