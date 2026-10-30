mod connections;
mod db;
mod editor;
mod fs;
mod known_hosts;
mod menu;
mod session;
mod term;
mod transfer;
mod vault;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            menu::install(app)?;
            let database = db::open(app.handle())?;
            let version = database.schema_version()?;
            let expected = db::latest_schema_version();
            if version != expected {
                return Err(std::io::Error::other(format!(
                    "unexpected schema version {version}, expected {expected}"
                ))
                .into());
            }
            app.manage(vault::VaultState::system());
            app.manage(known_hosts::HostKeyState::new());
            app.manage(session::SessionState::new());
            app.manage(term::TerminalState::new());
            app.manage(transfer::TransferState::new());
            app.manage(editor::EditorState::new());
            app.manage(database);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connections::connections_list,
            connections::connections_save,
            connections::connections_delete,
            vault::vault_status,
            vault::vault_init_password,
            vault::vault_unlock,
            vault::vault_set_credential,
            vault::vault_clear_credential,
            vault::vault_has_credential,
            known_hosts::hostkey_present,
            known_hosts::hostkey_list,
            known_hosts::hostkey_forget,
            known_hosts::hostkey_confirm,
            session::session_connect,
            session::session_trust_host,
            session::session_disconnect,
            session::session_list,
            fs::fs_list,
            fs::fs_stat,
            fs::fs_mkdir,
            fs::fs_rename,
            fs::fs_delete,
            fs::text::fs_read_text,
            fs::text::fs_write,
            transfer::transfer_start,
            transfer::transfer_cancel,
            term::term_open,
            term::term_write,
            term::term_resize,
            term::term_close,
            editor::editor_open,
            editor::editor_state,
            editor::editor_draft,
            editor::editor_note_saved,
            editor::editor_allow_close,
            editor::editor_discard,
            editor::editor_cancel_close,
            editor::editor_quit,
            editor::editor_cancel_quit,
        ])
        .on_menu_event(|app, event| menu::handle_event(app, event.id().as_ref()))
        .build(tauri::generate_context!())
        .expect("error while running FileBridge")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event
                && editor::should_prevent_exit(app)
            {
                api.prevent_exit();
                editor::prompt_quit(app);
            }
        });
}
