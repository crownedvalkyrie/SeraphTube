use crate::adblock::Compiled;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::{atomic::{AtomicU64, AtomicU8}, Mutex}};
use tauri::{AppHandle, Manager};

pub fn config_dir(app: &AppHandle) -> PathBuf {
    app.path().app_config_dir().expect("no config dir") // ~/Library/Application Support/<identifier>
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FilterList {
    pub id: String,
    pub name: String,
    pub url: Option<String>,
    pub builtin: bool,
    pub enabled: bool,
    #[serde(default)]
    pub updated: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub adblock: bool,
    pub lists: Vec<FilterList>,
}

impl Default for Settings {
    fn default() -> Self {
        let l = |id: &str, name: &str, url: Option<&str>, builtin| FilterList {
            id: id.into(), name: name.into(), url: url.map(Into::into), builtin, enabled: true, updated: None,
        };
        Self {
            theme: "gothic-cathedral".into(),
            adblock: true,
            lists: vec![
                l("core", "Seraphtube core (bundled)", None, true),
                l("easylist", "EasyList", Some("https://easylist.to/easylist/easylist.txt"), false),
                l("easyprivacy", "EasyPrivacy", Some("https://easylist.to/easylist/easyprivacy.txt"), false),
            ],
        }
    }
}

impl Settings {
    pub fn load(app: &AppHandle) -> Self {
        fs::read(config_dir(app).join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, app: &AppHandle) {
        let d = config_dir(app);
        let _ = fs::create_dir_all(&d);
        if let Ok(j) = serde_json::to_vec_pretty(self) {
            let _ = fs::write(d.join("settings.json"), j);
        }
    }
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub rules: Mutex<Compiled>,
    pub blocked: AtomicU64, // per-session counter (reset on launch)
    pub native: AtomicU8,   // 0 off, 1 compiling, 2 active, 3 failed -> core-only fallback
}

impl AppState {
    pub fn new(s: Settings) -> Self {
        Self { settings: Mutex::new(s), rules: Mutex::new(Compiled::default()), blocked: AtomicU64::new(0), native: AtomicU8::new(0) }
    }
}