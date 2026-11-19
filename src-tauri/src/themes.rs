//! Theme registry. Built-ins are embedded; user themes are JSON files in
//! <config dir>/app.seraphtube.desktop/themes/ (macOS: ~/Library/Application Support, Windows: %APPDATA%).
//! The active theme id persists in settings.json.
use crate::settings::config_dir;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs};
#[allow(unused_imports)]
use tauri::{utils::config::WindowEffectsConfig,
            window::{Effect, EffectState, EffectsBuilder},
            AppHandle, Manager};

const BASE: &str = include_str!("../../src/themes/youtube-base.css");

#[derive(Serialize, Clone)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub effect: String, // "none" | "glass"
    pub swatch: Vec<String>,
    pub builtin: bool,
    #[serde(skip)]
    pub css: String,
}

#[derive(Deserialize)]
struct UserTheme {
    id: String,
    name: String,
    #[serde(default)] effect: String,
    #[serde(default)] swatch: Vec<String>,
    #[serde(default)] vars: BTreeMap<String, String>,
    #[serde(default)] css: String,
}

fn builtin(id: &str, name: &str, effect: &str, sw: [&str; 3], css: &str) -> Theme {
    Theme { id: id.into(), name: name.into(), effect: effect.into(), swatch: sw.map(String::from).to_vec(), builtin: true, css: css.into() }
}

pub fn all(app: &AppHandle) -> Vec<Theme> {
    let mut v = vec![
        builtin("gothic-cathedral", "Gothic Cathedral", "none", ["#141214", "#c8a45a", "#8e2f45"], include_str!("../../src/themes/gothic-cathedral.css")),
        builtin("liquid-glass", "Liquid Glass", "glass", ["#2b2f3a", "#9cc2ff", "#ffffff"], include_str!("../../src/themes/liquid-glass.css")),
        builtin("golden-gate-27", "Golden Gate 27", "none", ["#0b1426", "#c0362c", "#b7bdc6"], include_str!("../../src/themes/golden-gate-27.css")),
        // Plain YouTube. Also the quickest A/B test: if something misbehaves in a themed
        // window and not here, the theme CSS is the cause.
        builtin("stock", "YouTube Stock", "none", ["#0f0f0f", "#ff0033", "#f1f1f1"], include_str!("../../src/themes/stock.css")),
    ];
    if let Ok(rd) = fs::read_dir(config_dir(app).join("themes")) {
        for e in rd.flatten() {
            let p = e.path();
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !fname.ends_with(".json") || fname.starts_with('_') { continue; }
            let Some(t) = fs::read(&p).ok().and_then(|b| serde_json::from_slice::<UserTheme>(&b).ok()) else { continue };
            let id: String = t.id.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
            if id.is_empty() || v.iter().any(|x| x.id == id) { continue; }
            let vars: String = t.vars.iter()
                .filter(|(k, _)| k.starts_with("--st-") || k.starts_with("--yt-spec-"))
                .map(|(k, val)| format!("{k}:{val};")).collect();
            v.push(Theme {
                css: format!(":root[data-seraph-theme=\"{id}\"]{{{vars}}}\n{}", t.css),
                swatch: if t.swatch.is_empty() { vec!["#222".into(), "#888".into(), "#fff".into()] } else { t.swatch },
                effect: if t.effect == "glass" { "glass".into() } else { "none".into() },
                id, name: t.name, builtin: false,
            });
        }
    }
    v
}

pub fn find(app: &AppHandle, id: &str) -> Theme {
    let mut v = all(app);
    let i = v.iter().position(|t| t.id == id).unwrap_or(0);
    v.swap_remove(i)
}

pub fn full_css(t: &Theme) -> String { format!("{BASE}\n{}", t.css) }

/// "Glass" themes get a native window effect where one exists:
/// macOS vibrancy (HudWindow), Windows 11 Mica. Linux compositors expose no such
/// API, so the theme falls back to its plain CSS there. Removed for every other theme.
pub fn apply_effect(app: &AppHandle, effect: &str) {
    let glass = effect == "glass";
    for label in ["main", "settings"] {
        if let Some(w) = app.get_webview_window(label) {
            // On Windows, set_effects(None) immediately after window creation can
            // reset the composition controller and leave a blank surface. Only
            // clear effects when we are actually switching away from glass.
            #[cfg(target_os = "windows")]
            {
                if !glass {
                    // Leave the opaque window alone; no effect to clear.
                    continue;
                }
            }
            let cfg: Option<WindowEffectsConfig> = glass.then(|| glass_config()).flatten();
            let _ = w.set_effects(cfg);
        }
    }
}

#[cfg(target_os = "macos")]
fn glass_config() -> Option<WindowEffectsConfig> {
    Some(EffectsBuilder::new().effect(Effect::HudWindow).state(EffectState::Active).radius(10.0).build())
}

#[cfg(target_os = "windows")]
fn glass_config() -> Option<WindowEffectsConfig> {
    // Mica needs Windows 11; on Windows 10 set_effects fails and the error is ignored above.
    Some(EffectsBuilder::new().effect(Effect::Mica).build())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn glass_config() -> Option<WindowEffectsConfig> { None }
