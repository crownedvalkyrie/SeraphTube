//! Theme registry. Built-ins are embedded; user themes are JSON files in
//! <config dir>/app.seraphtube.desktop/themes/ (macOS: ~/Library/Application Support, Windows: %APPDATA%).
//! The active theme id persists in settings.json.
use crate::cursors;
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

/// The stylesheet for a theme: shared base, the theme itself, the chosen cursor set, and on
/// Windows a solid backdrop for glass themes.
///
/// Windows windows are always opaque. A transparent (layered) WebView2 window can freeze CSS
/// animations and fail to paint, so "glass" themes get a solid backdrop there instead of
/// the translucent one that macOS vibrancy shows through.
pub fn full_css(t: &Theme, cursor: &str) -> String {
    let accent = t.swatch.get(1).map(String::as_str).unwrap_or("#c8a45a");
    let outline = t.swatch.first().map(String::as_str).unwrap_or("#141214");

    let mut css = format!("{BASE}\n{}", t.css);
    css.push('\n');
    css.push_str(&cursors::css(cursor, accent, outline));

    #[cfg(target_os = "windows")]
    if t.effect == "glass" {
        let id = &t.id;
        css.push_str(&format!(
            ":root[data-seraph-theme=\"{id}\"]{{--st-bg:#10141e}}\n\
             :root[data-seraph-theme=\"{id}\"],:root[data-seraph-theme=\"{id}\"] body{{background:#10141e!important}}\n"
        ));
    }

    css
}

/// Cursor picker data for the settings page, tinted for this theme.
pub fn cursor_catalog(t: &Theme) -> serde_json::Value {
    let accent = t.swatch.get(1).map(String::as_str).unwrap_or("#c8a45a");
    let outline = t.swatch.first().map(String::as_str).unwrap_or("#141214");
    cursors::catalog(accent, outline)
}

/// "Glass" themes get a native window effect on macOS (vibrancy). Windows windows stay
/// opaque (see full_css), so there is nothing to apply or clear there.
pub fn apply_effect(app: &AppHandle, effect: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = (app, effect);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let glass = effect == "glass";
        for label in ["main", "settings"] {
            if let Some(w) = app.get_webview_window(label) {
                let cfg: Option<WindowEffectsConfig> = glass.then(glass_config).flatten();
                let _ = w.set_effects(cfg);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn glass_config() -> Option<WindowEffectsConfig> {
    Some(EffectsBuilder::new().effect(Effect::HudWindow).state(EffectState::Active).radius(10.0).build())
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn glass_config() -> Option<WindowEffectsConfig> { None }
