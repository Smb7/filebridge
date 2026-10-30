//! Editor windows and the in-memory draft. The draft is not written to SQLite
//! or disk. The webview cannot create these windows or choose their URL.

mod policy;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::fs::path;
use crate::fs::text::{self, TextFile};
use crate::session::{SessionState, SessionSummary};

use policy::{OpenKey, OpenPlan, editor_title, plan_open};

const CONFIRM_CLOSE: &str = "editor://confirm-close";
const CONFIRM_QUIT: &str = "editor://confirm-quit";

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

impl From<path::PathError> for CommandError {
    fn from(error: path::PathError) -> Self {
        Self {
            code: "invalid_path",
            message: error.message.to_string(),
        }
    }
}

fn failed(message: &'static str) -> CommandError {
    CommandError {
        code: "editor_failed",
        message: message.to_string(),
    }
}

fn from_text(error: crate::fs::CommandError) -> CommandError {
    CommandError {
        code: error.code,
        message: error.message,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorOpened {
    pub editor_id: String,
    pub focused: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorView {
    editor_id: String,
    session_id: String,
    connection_id: String,
    path: String,
    name: String,
    username: String,
    host: String,
    port: u16,
    title: String,
    connected: bool,
    dirty: bool,
    draft: Option<String>,
    baseline_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveNoted {
    retargeted: bool,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitPrompt {
    count: usize,
    paths: Vec<String>,
}

struct Slot {
    label: String,
    connection_id: String,
    session_id: String,
    path: String,
    name: String,
    username: String,
    host: String,
    port: u16,
    draft: Option<String>,
    dirty: bool,
    baseline_sha256: Option<String>,
    discard_armed: bool,
    close_prompted: bool,
}

struct EditorData {
    slots: HashMap<String, Slot>,
    order: Vec<String>,
    quit_armed: bool,
    quit_prompted: bool,
}

#[derive(Clone)]
pub struct EditorState {
    inner: Arc<Mutex<EditorData>>,
    open_lock: Arc<tokio::sync::Mutex<()>>,
}

impl EditorState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(EditorData {
                slots: HashMap::new(),
                order: Vec::new(),
                quit_armed: false,
                quit_prompted: false,
            })),
            open_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, EditorData>, CommandError> {
        self.inner
            .lock()
            .map_err(|_| failed("Could not update the editor."))
    }

    fn existing(&self, connection_id: &str, session_id: &str, path: &str) -> Option<String> {
        let data = self.inner.lock().ok()?;
        let keys: Vec<OpenKey> = data
            .order
            .iter()
            .filter_map(|label| data.slots.get(label).map(open_key))
            .collect();
        let incoming = OpenKey {
            connection_id: connection_id.to_string(),
            session_id: session_id.to_string(),
            path: path.to_string(),
        };
        match plan_open(&keys, &incoming) {
            OpenPlan::Focus { index } => data.order.get(index).cloned(),
            OpenPlan::Create | OpenPlan::Refuse => None,
        }
    }

    fn count(&self) -> usize {
        self.inner.lock().map(|data| data.order.len()).unwrap_or(0)
    }

    fn at_cap(&self) -> bool {
        self.count() >= policy::MAX_EDITORS
    }

    fn insert(&self, slot: Slot) {
        if let Ok(mut data) = self.inner.lock() {
            let label = slot.label.clone();
            data.order.push(label.clone());
            data.slots.insert(label, slot);
        }
    }

    fn remove(&self, label: &str) {
        if let Ok(mut data) = self.inner.lock() {
            data.slots.remove(label);
            data.order.retain(|item| item != label);
        }
    }

    fn rebind(&self, label: &str, summary: &SessionSummary, path: &str) {
        if let Ok(mut data) = self.inner.lock()
            && let Some(slot) = data.slots.get_mut(label)
        {
            slot.session_id = summary.session_id.clone();
            slot.connection_id = summary.connection_id.clone();
            slot.name = summary.name.clone();
            slot.username = summary.username.clone();
            slot.host = summary.host.clone();
            slot.port = summary.port;
            slot.path = path.to_string();
        }
    }

    fn set_draft(
        &self,
        label: &str,
        content: Option<String>,
        dirty: bool,
    ) -> Result<(), CommandError> {
        if let Some(content) = content.as_deref() {
            text::reject_content(content)
                .map_err(|error| from_text(text::command_error(error, true)))?;
        }
        let mut data = self.lock()?;
        let slot = data
            .slots
            .get_mut(label)
            .ok_or_else(|| failed("That editor is not open."))?;
        if let Some(content) = content {
            slot.draft = Some(content);
        }
        slot.dirty = dirty;
        Ok(())
    }

    fn view(
        &self,
        label: &str,
        sessions: &SessionState,
        include_draft: bool,
    ) -> Result<EditorView, CommandError> {
        self.refresh_binding(label, sessions)?;
        let data = self.lock()?;
        let slot = data
            .slots
            .get(label)
            .ok_or_else(|| failed("That editor is not open."))?;
        let connected = sessions
            .summary_for_connection(&slot.connection_id)
            .ok()
            .flatten()
            .is_some_and(|summary| summary.session_id == slot.session_id);
        Ok(view_of(slot, connected, include_draft))
    }

    fn refresh_binding(&self, label: &str, sessions: &SessionState) -> Result<(), CommandError> {
        let connection_id = {
            let data = self.lock()?;
            data.slots
                .get(label)
                .ok_or_else(|| failed("That editor is not open."))?
                .connection_id
                .clone()
        };
        let live = sessions
            .summary_for_connection(&connection_id)
            .map_err(|_| failed("Could not update the editor."))?;
        let mut data = self.lock()?;
        let slot = data
            .slots
            .get_mut(label)
            .ok_or_else(|| failed("That editor is not open."))?;
        if let Some(summary) = live {
            slot.session_id = summary.session_id;
            slot.name = summary.name;
            slot.username = summary.username;
            slot.host = summary.host;
            slot.port = summary.port;
        }
        Ok(())
    }

    fn note_saved(&self, label: &str, sha256: &str, path: &str) -> Result<SaveNoted, CommandError> {
        if !is_sha256(sha256) {
            return Err(failed("Could not record the save."));
        }
        let mut data = self.lock()?;
        let connection_id = data
            .slots
            .get(label)
            .ok_or_else(|| failed("That editor is not open."))?
            .connection_id
            .clone();
        let taken = data.order.iter().any(|other| {
            other != label
                && data
                    .slots
                    .get(other)
                    .is_some_and(|slot| slot.connection_id == connection_id && slot.path == path)
        });
        let slot = data
            .slots
            .get_mut(label)
            .ok_or_else(|| failed("That editor is not open."))?;
        if path != slot.path && taken {
            return Ok(SaveNoted {
                retargeted: false,
                path: slot.path.clone(),
            });
        }
        slot.path = path.to_string();
        slot.baseline_sha256 = Some(sha256.to_string());
        slot.dirty = false;
        Ok(SaveNoted {
            retargeted: true,
            path: path.to_string(),
        })
    }

    fn title_of(&self, label: &str) -> Option<String> {
        let data = self.inner.lock().ok()?;
        data.slots.get(label).map(slot_title)
    }

    fn blocks_close(&self, label: &str) -> bool {
        let Ok(data) = self.inner.lock() else {
            return true;
        };
        if data.quit_armed {
            return false;
        }
        // A clean window must still close if its page never loaded. Dirty
        // edits are refused until the page confirms discard.
        match data.slots.get(label) {
            Some(slot) => slot.dirty && !slot.discard_armed,
            None => false,
        }
    }

    fn mark_close_prompted(&self, label: &str) -> bool {
        let Ok(mut data) = self.inner.lock() else {
            return false;
        };
        let Some(slot) = data.slots.get_mut(label) else {
            return false;
        };
        if slot.close_prompted {
            return false;
        }
        slot.close_prompted = true;
        true
    }

    fn clear_close_prompt(&self, label: &str) {
        if let Ok(mut data) = self.inner.lock()
            && let Some(slot) = data.slots.get_mut(label)
        {
            slot.close_prompted = false;
        }
    }

    fn arm_discard(&self, label: &str) -> Result<(), CommandError> {
        let mut data = self.lock()?;
        let slot = data
            .slots
            .get_mut(label)
            .ok_or_else(|| failed("That editor is not open."))?;
        slot.discard_armed = true;
        slot.dirty = false;
        Ok(())
    }

    fn blocks_exit(&self) -> bool {
        let Ok(data) = self.inner.lock() else {
            return false;
        };
        !data.quit_armed && data.slots.values().any(|slot| slot.dirty)
    }

    fn begin_quit_prompt(&self) -> Option<QuitPrompt> {
        let Ok(mut data) = self.inner.lock() else {
            return None;
        };
        if data.quit_armed || data.quit_prompted || !data.slots.values().any(|slot| slot.dirty) {
            return None;
        }
        data.quit_prompted = true;
        let paths = data
            .slots
            .values()
            .filter(|slot| slot.dirty)
            .map(|slot| slot.path.clone())
            .collect::<Vec<_>>();
        let count = paths.len();
        Some(QuitPrompt { count, paths })
    }

    fn clear_quit_prompt(&self) {
        if let Ok(mut data) = self.inner.lock() {
            data.quit_prompted = false;
        }
    }

    fn arm_quit(&self) {
        if let Ok(mut data) = self.inner.lock() {
            data.quit_armed = true;
            for slot in data.slots.values_mut() {
                slot.discard_armed = true;
            }
        }
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

fn open_key(slot: &Slot) -> OpenKey {
    OpenKey {
        connection_id: slot.connection_id.clone(),
        session_id: slot.session_id.clone(),
        path: slot.path.clone(),
    }
}

fn slot_title(slot: &Slot) -> String {
    editor_title(
        &slot.name,
        &slot.username,
        &slot.host,
        slot.port,
        &slot.path,
    )
}

fn view_of(slot: &Slot, connected: bool, include_draft: bool) -> EditorView {
    EditorView {
        editor_id: slot.label.clone(),
        session_id: slot.session_id.clone(),
        connection_id: slot.connection_id.clone(),
        path: slot.path.clone(),
        name: slot.name.clone(),
        username: slot.username.clone(),
        host: slot.host.clone(),
        port: slot.port,
        title: slot_title(slot),
        connected,
        dirty: slot.dirty,
        draft: if include_draft {
            slot.draft.clone()
        } else {
            None
        },
        baseline_sha256: slot.baseline_sha256.clone(),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn focus_label(app: &AppHandle, label: &str, title: &str) -> bool {
    let Some(window) = app.get_webview_window(label) else {
        return false;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_title(title);
    let _ = window.set_focus();
    true
}

fn watch_close(window: &tauri::WebviewWindow, editors: EditorState, app: AppHandle) {
    let label = window.label().to_string();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if editors.blocks_close(&label) {
                api.prevent_close();
                if editors.mark_close_prompted(&label)
                    && let Some(target) = app.get_webview_window(&label)
                {
                    let _ = target.emit(CONFIRM_CLOSE, ());
                }
            }
        } else if let WindowEvent::Destroyed = event {
            editors.remove(&label);
        }
    });
}

pub fn should_prevent_exit(app: &AppHandle) -> bool {
    app.try_state::<EditorState>()
        .is_some_and(|editors| editors.blocks_exit())
}

pub fn prompt_quit(app: &AppHandle) {
    let Some(editors) = app.try_state::<EditorState>() else {
        return;
    };
    let Some(prompt) = editors.begin_quit_prompt() else {
        return;
    };
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit(CONFIRM_QUIT, &prompt);
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
        return;
    }
    let label = editors
        .inner
        .lock()
        .ok()
        .and_then(|data| data.order.first().cloned());
    if let Some(label) = label
        && let Some(window) = app.get_webview_window(&label)
    {
        let _ = window.emit(CONFIRM_QUIT, &prompt);
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub async fn editor_open(
    app: AppHandle,
    sessions: tauri::State<'_, SessionState>,
    editors: tauri::State<'_, EditorState>,
    session_id: String,
    path: String,
) -> Result<EditorOpened, CommandError> {
    let path = path::normalize_remote_path(&path)?;
    if path == "/" {
        return Err(CommandError {
            code: "not_a_file",
            message: "That path is not a file.".to_string(),
        });
    }
    let summary = sessions
        .summary(&session_id)
        .map_err(|_| failed("Could not open that file."))?
        .ok_or_else(|| CommandError {
            code: "not_connected",
            message: "That server is not connected.".to_string(),
        })?;

    let _guard = editors.open_lock.lock().await;
    if let Some(label) = editors.existing(&summary.connection_id, &summary.session_id, &path) {
        editors.rebind(&label, &summary, &path);
        let title = editors.title_of(&label).unwrap_or_else(|| {
            editor_title(
                &summary.name,
                &summary.username,
                &summary.host,
                summary.port,
                &path,
            )
        });
        if focus_label(&app, &label, &title) {
            return Ok(EditorOpened {
                editor_id: label,
                focused: true,
            });
        }
        editors.remove(&label);
    }
    if editors.at_cap() {
        return Err(CommandError {
            code: "editor_limit",
            message: "Only two editor windows can be open.".to_string(),
        });
    }

    let file = text::read_text(&sessions, &session_id, &path)
        .await
        .map_err(|error| from_text(text::command_error(error, false)))?;
    open_window(&app, &editors, &summary, &path, file)
}

fn open_window(
    app: &AppHandle,
    editors: &EditorState,
    summary: &SessionSummary,
    path: &str,
    file: TextFile,
) -> Result<EditorOpened, CommandError> {
    let label = format!("editor-{}", uuid::Uuid::new_v4());
    let title = editor_title(
        &summary.name,
        &summary.username,
        &summary.host,
        summary.port,
        path,
    );
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title(&title)
        .inner_size(960.0, 700.0)
        .min_inner_size(520.0, 360.0)
        .decorations(true)
        .resizable(true)
        .focused(true)
        .center()
        .build()
        .map_err(|_| failed("Could not open an editor window."))?;
    editors.insert(Slot {
        label: label.clone(),
        connection_id: summary.connection_id.clone(),
        session_id: summary.session_id.clone(),
        path: path.to_string(),
        name: summary.name.clone(),
        username: summary.username.clone(),
        host: summary.host.clone(),
        port: summary.port,
        draft: Some(file.content),
        dirty: false,
        baseline_sha256: Some(file.sha256),
        discard_armed: false,
        close_prompted: false,
    });
    watch_close(&window, (*editors).clone(), app.clone());
    Ok(EditorOpened {
        editor_id: label,
        focused: false,
    })
}

#[tauri::command]
pub fn editor_state(
    window: tauri::WebviewWindow,
    editors: tauri::State<'_, EditorState>,
    sessions: tauri::State<'_, SessionState>,
    include_draft: bool,
) -> Result<EditorView, CommandError> {
    let label = window.label().to_string();
    let view = editors.view(&label, &sessions, include_draft)?;
    let _ = window.set_title(&view.title);
    Ok(view)
}

#[tauri::command]
pub fn editor_draft(
    window: tauri::WebviewWindow,
    editors: tauri::State<'_, EditorState>,
    content: Option<String>,
    dirty: bool,
) -> Result<(), CommandError> {
    editors.set_draft(window.label(), content, dirty)
}

#[tauri::command]
pub fn editor_note_saved(
    window: tauri::WebviewWindow,
    editors: tauri::State<'_, EditorState>,
    sha256: String,
    path: String,
) -> Result<SaveNoted, CommandError> {
    let path = path::normalize_remote_path(&path)?;
    let noted = editors.note_saved(window.label(), &sha256, &path)?;
    if noted.retargeted
        && let Some(title) = editors.title_of(window.label())
    {
        let _ = window.set_title(&title);
    }
    Ok(noted)
}

#[tauri::command]
pub fn editor_allow_close(
    window: tauri::WebviewWindow,
    editors: tauri::State<'_, EditorState>,
) -> bool {
    !editors.blocks_close(window.label())
}

#[tauri::command]
pub fn editor_discard(
    window: tauri::WebviewWindow,
    editors: tauri::State<'_, EditorState>,
) -> Result<(), CommandError> {
    editors.arm_discard(window.label())?;
    window
        .close()
        .map_err(|_| failed("Could not close the editor."))
}

#[tauri::command]
pub fn editor_cancel_close(window: tauri::WebviewWindow, editors: tauri::State<'_, EditorState>) {
    editors.clear_close_prompt(window.label());
}

#[tauri::command]
pub fn editor_quit(app: AppHandle, editors: tauri::State<'_, EditorState>) {
    editors.arm_quit();
    app.exit(0);
}

#[tauri::command]
pub fn editor_cancel_quit(editors: tauri::State<'_, EditorState>) {
    editors.clear_quit_prompt();
}

#[cfg(test)]
mod tests {
    use super::{EditorState, Slot, editor_title};

    fn slot(label: &str, path: &str) -> Slot {
        Slot {
            label: label.to_string(),
            connection_id: "conn-a".to_string(),
            session_id: "ses-a".to_string(),
            path: path.to_string(),
            name: "prod-telegraf".to_string(),
            username: "ubuntu".to_string(),
            host: "10.0.0.8".to_string(),
            port: 22,
            draft: None,
            dirty: false,
            baseline_sha256: None,
            discard_armed: false,
            close_prompted: false,
        }
    }

    #[test]
    fn a_draft_stays_in_memory_and_dies_with_the_window() {
        let editors = EditorState::new();
        editors.insert(slot("editor-1", "/etc/telegraf/telegraf.conf"));
        assert!(!editors.blocks_close("editor-1"));
        editors
            .set_draft("editor-1", Some("interval = \"10s\"\n".to_string()), true)
            .expect("draft");
        let data = editors.inner.lock().expect("lock");
        let stored = data.slots.get("editor-1").expect("slot");
        assert_eq!(stored.draft.as_deref(), Some("interval = \"10s\"\n"));
        assert!(stored.dirty);
        drop(data);

        let kept = editors
            .set_draft("editor-1", Some("a\0b".to_string()), true)
            .expect_err("nul");
        assert_eq!(kept.code, "not_text");
        assert!(!kept.message.contains("a\0b"));
        let data = editors.inner.lock().expect("lock");
        assert_eq!(
            data.slots
                .get("editor-1")
                .and_then(|slot| slot.draft.as_deref()),
            Some("interval = \"10s\"\n")
        );
        drop(data);

        editors.remove("editor-1");
        assert!(editors.inner.lock().expect("lock").slots.is_empty());
        assert!(!editors.blocks_close("editor-1"));
    }

    #[test]
    fn quit_prompts_only_while_a_buffer_is_dirty() {
        let editors = EditorState::new();
        editors.insert(slot("editor-1", "/etc/telegraf/cpu.conf"));
        assert!(!editors.blocks_exit());
        editors.set_draft("editor-1", None, true).expect("touch");
        assert!(editors.blocks_exit());
        assert!(editors.blocks_close("editor-1"));
        let prompt = editors.begin_quit_prompt().expect("prompt");
        assert_eq!(prompt.count, 1);
        assert_eq!(prompt.paths, vec!["/etc/telegraf/cpu.conf".to_string()]);
        assert!(editors.begin_quit_prompt().is_none());
        editors.clear_quit_prompt();
        editors.arm_quit();
        assert!(!editors.blocks_exit());
        assert!(!editors.blocks_close("editor-1"));
    }

    #[test]
    fn save_as_does_not_steal_a_file_another_window_has_open() {
        let editors = EditorState::new();
        editors.insert(slot("editor-1", "/etc/telegraf/cpu.conf"));
        editors.insert(slot("editor-2", "/etc/telegraf/mem.conf"));
        let sha = "ab".repeat(32);
        let blocked = editors
            .note_saved("editor-1", &sha, "/etc/telegraf/mem.conf")
            .expect("kept");
        assert!(!blocked.retargeted);
        assert_eq!(blocked.path, "/etc/telegraf/cpu.conf");
        let moved = editors
            .note_saved("editor-1", &sha, "/etc/telegraf/disk.conf")
            .expect("moved");
        assert!(moved.retargeted);
        assert_eq!(moved.path, "/etc/telegraf/disk.conf");
        let data = editors.inner.lock().expect("lock");
        let slot = data.slots.get("editor-1").expect("slot");
        assert!(!slot.dirty);
        assert_eq!(slot.baseline_sha256.as_deref(), Some(sha.as_str()));
        let title = editor_title(
            &slot.name,
            &slot.username,
            &slot.host,
            slot.port,
            &slot.path,
        );
        assert!(title.contains("disk.conf"));
        assert!(!title.contains("hunter2-secret"));
    }
}
