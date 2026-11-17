//! Persistent performance-mode preference shared by the local settings page
//! and the remote YouTube webview.
//!
//! The command only exposes a small, validated enum. No arbitrary paths or
//! script strings are accepted from the frontend.

use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};

const DEFAULT_MODE: &str = "balanced";
const MODES: &[&str] = &["balanced", "performance"];

fn preference_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("performance-mode.txt"))
}

fn validate_mode(mode: &str) -> Result<&'static str, String> {
    MODES
        .iter()
        .copied()
        .find(|candidate| *candidate == mode)
        .ok_or_else(|| "Unknown performance mode".to_string())
}

#[tauri::command]
pub fn get_performance(app: AppHandle) -> Result<String, String> {
    let path = preference_path(&app)?;
    let mode = match fs::read_to_string(path) {
        Ok(value) => value.trim().to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DEFAULT_MODE.to_string()
        }
        Err(error) => return Err(error.to_string()),
    };

    Ok(validate_mode(&mode).unwrap_or(DEFAULT_MODE).to_string())
}

#[tauri::command]
pub fn set_performance(app: AppHandle, mode: String) -> Result<String, String> {
    let mode = validate_mode(&mode)?;
    let path = preference_path(&app)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_file_name(format!("performance-mode.{}.{}.tmp", std::process::id(), nonce));

    fs::write(&temp, mode.as_bytes()).map_err(|e| e.to_string())?;

    // Windows does not replace an existing destination with std::fs::rename.
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }

    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }

    // Apply immediately when the YouTube webview already exists. The shell
    // also reads this same enum on startup, so opening settings is optional.
    if let Some(main) = app.get_webview_window("main") {
        let mode_literal = serde_json::to_string(mode).map_err(|e| e.to_string())?;
        let script = format!(
            "window.__seraph?.setPerformance?.({mode_literal});"
        );
        let _ = main.eval(&script);
    }

    Ok(mode.to_string())
}
