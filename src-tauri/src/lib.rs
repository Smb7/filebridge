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
            if version != "1" {
                return Err(
                    std::io::Error::other(format!("unexpected schema version {version}")).into(),
                );
            }
            app.manage(database);
            Ok(())
        })
        .on_menu_event(|app, event| menu::handle_event(app, event.id().as_ref()))
        .run(tauri::generate_context!())
        .expect("error while running FileBridge");
}
