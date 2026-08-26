#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn diagnostics() -> serde_json::Value {
    personal_agent_core::diagnostic_snapshot()
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![diagnostics])
        .run(tauri::generate_context!())
        .expect("Personal Agent desktop host failed");
}
