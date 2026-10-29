#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod imgproc;
use imgproc::{Fail, Opts, PreviewOut, RunOut};
use serde_json::json;
use tauri::Emitter;

#[tauri::command]
fn expand(paths: Vec<String>) -> Vec<String> {
    imgproc::expand(paths)
}

#[tauri::command]
async fn preview(path: String, opts: Opts) -> Result<PreviewOut, String> {
    tauri::async_runtime::spawn_blocking(move || imgproc::preview(&path, &opts))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn run(app: tauri::AppHandle, files: Vec<String>, opts: Opts) -> Result<RunOut, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let n = files.len();
        let mut out = RunOut::default();
        for (i, f) in files.iter().enumerate() {
            match imgproc::convert(f, i, &opts) {
                Ok(true) => out.done += 1,
                Ok(false) => out.skipped += 1,
                Err(e) => out.failed.push(Fail { file: f.clone(), error: e }),
            }
            let _ = app.emit("progress", json!({ "i": i + 1, "n": n }));
        }
        out
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal(path: String) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(&path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![expand, preview, run, reveal])
        .run(tauri::generate_context!())
        .expect("error while running Photo Resizer");
}
