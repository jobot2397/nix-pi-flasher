mod build;
mod flash;
mod search;
mod spawn;
mod state;
mod validate;

use tauri::Manager;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(AppState::new().expect("failed to create HTTP client"));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search::search_packages,
            build::start_build,
            build::cancel_build,
            build::last_image,
            flash::list_disks,
            flash::flash,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
