
use crate::adblock::Compiled;

use serde::{Deserialize, Serialize};

use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicU8},
        Mutex,
    },
};

use tauri::{AppHandle, Manager};

/// Bump when defaults change in a way existing installs must pick up.
///
/// 2: EasyList/EasyPrivacy become opt-in and the stricter Google ads list
///    is added.
pub const SETTINGS_VERSION: u32 = 3;

/// Default value for the splash intro.
///
/// Existing settings files that do not contain an "intro" value
/// will use true.
fn default_intro() -> bool {
    true
}

fn default_version() -> u32 {
    0
}

pub fn config_dir(app: &AppHandle) -> PathBuf {
    // macOS:   ~/Library/Application Support/<identifier>
    // Windows: %APPDATA%\<identifier>
    app.path().app_config_dir().expect("no config dir")
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

fn list(
    id: &str,
    name: &str,
    url: Option<&str>,
    builtin: bool,
    enabled: bool,
) -> FilterList {
    FilterList {
        id: id.into(),
        name: name.into(),
        url: url.map(Into::into),
        builtin,
        enabled,
        updated: None,
    }
}

fn core_list() -> FilterList {
    list("core", "Seraphtube core", None, true, true)
}

fn google_ads_list() -> FilterList {
    list(
        "google-ads",
        "Google ad servers (stricter)",
        None,
        true,
        false,
    )
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub adblock: bool,
    pub lists: Vec<FilterList>,

    /// Show the animated splash intro when the app launches.
    ///
    /// true  = show the full intro.
    /// false = use the quick intro and skip the intro waiting period.
    #[serde(default = "default_intro")]
    pub intro: bool,

    #[serde(default = "default_version")]
    pub version: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "gothic-cathedral".into(),
            adblock: true,
            intro: true,

            lists: vec![
                core_list(),
                google_ads_list(),
                list(
                    "easylist",
                    "EasyList",
                    Some("https://easylist.to/easylist/easylist.txt"),
                    false,
                    false,
                ),
                list(
                    "easyprivacy",
                    "EasyPrivacy",
                    Some("https://easylist.to/easylist/easyprivacy.txt"),
                    false,
                    false,
                ),
            ],

            version: SETTINGS_VERSION,
        }
    }
}

impl Settings {
    pub fn load(app: &AppHandle) -> Self {
        let path = config_dir(app).join("settings.json");

        let mut settings: Settings = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();

        if settings.version < SETTINGS_VERSION {
            settings.migrate();
            settings.save(app);
        }

        settings
    }

    fn migrate(&mut self) {
        if self.version < 2 {
            for item in self
                .lists
                .iter_mut()
                .filter(|item| {
                    item.id == "easylist" || item.id == "easyprivacy"
                })
            {
                item.enabled = false;
            }
        }

        // Make sure the bundled core list exists.
        if !self.lists.iter().any(|item| item.id == "core") {
            self.lists.insert(0, core_list());
        }

        // Make sure the Google ads list exists.
        if !self.lists.iter().any(|item| item.id == "google-ads") {
            let position = self
                .lists
                .iter()
                .position(|item| item.id == "core")
                .map_or(0, |index| index + 1);

            self.lists.insert(position, google_ads_list());
        }

        // Do not overwrite self.intro here.
        // This preserves the user's saved intro toggle setting.

        self.version = SETTINGS_VERSION;
    }

    pub fn save(&self, app: &AppHandle) {
        let directory = config_dir(app);

        if let Err(error) = fs::create_dir_all(&directory) {
            eprintln!("[settings] Could not create settings directory: {error}");
            return;
        }

        match serde_json::to_vec_pretty(self) {
            Ok(json) => {
                let path = directory.join("settings.json");

                if let Err(error) = fs::write(path, json) {
                    eprintln!("[settings] Could not save settings: {error}");
                }
            }

            Err(error) => {
                eprintln!("[settings] Could not serialize settings: {error}");
            }
        }
    }
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub rules: Mutex<Compiled>,
    pub blocked: AtomicU64,

    /// 0 off
    /// 1 compiling
    /// 2 WebKit rules active (macOS)
    /// 3 failed -> JS layer only
    /// 5 WebView2 request filter active (Windows)
    pub native: AtomicU8,
}

impl AppState {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings: Mutex::new(settings),
            rules: Mutex::new(Compiled::default()),
            blocked: AtomicU64::new(0),
            native: AtomicU8::new(0),
        }
    }
}
