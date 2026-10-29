mod connections;
mod db;
mod menu;
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
        ])
        .on_menu_event(|app, event| menu::handle_event(app, event.id().as_ref()))
        .run(tauri::generate_context!())
        .expect("error while running FileBridge");
}
