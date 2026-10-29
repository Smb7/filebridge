use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

const QUIT_ID: &str = "quit";

pub fn install(app: &mut tauri::App) -> tauri::Result<()> {
    let menu = build(app.handle())?;
    app.set_menu(menu)?;
    Ok(())
}

pub fn handle_event(app: &AppHandle, event_id: &str) {
    if event_id == QUIT_ID {
        app.exit(0);
    }
}

fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[&MenuItem::with_id(
            app,
            QUIT_ID,
            "Quit",
            true,
            Some("CmdOrCtrl+Q"),
        )?],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&file, &edit])
}
