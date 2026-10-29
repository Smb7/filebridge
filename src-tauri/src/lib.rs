mod connections;
mod db;
mod menu;

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
            app.manage(database);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connections::connections_list,
            connections::connections_save,
            connections::connections_delete,
        ])
        .on_menu_event(|app, event| menu::handle_event(app, event.id().as_ref()))
        .run(tauri::generate_context!())
        .expect("error while running FileBridge");
}
