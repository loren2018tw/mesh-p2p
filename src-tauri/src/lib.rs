mod share;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let share_state = share::ShareState::new();

    tauri::Builder::default()
        .manage(share_state)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            share::pick_share_file,
            share::pick_share_files,
            share::add_share_files,
            share::start_share,
            share::stop_share,
            share::get_share_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
