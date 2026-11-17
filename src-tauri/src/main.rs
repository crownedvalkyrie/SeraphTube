#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod adblock;
mod performance;
mod settings;
mod themes;

use serde_json::{json, Value};
use settings::{config_dir, AppState, FilterList, Settings};
use std::{fs, sync::atomic::Ordering::Relaxed, time::Duration};
use tauri::{menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder}, webview::PageLoadEvent,
            AppHandle, Manager, State, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

// Safari-like UA: Google sign-in is much more likely to accept it than the bare WKWebView UA.
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
const SHELL: &str = include_str!("../../src/shell.js");
const TEMPLATE: &str = include_str!("../../src/themes/_template.json");
const INTERNAL: &[&str] = &["youtube.com", "youtu.be", "google.com", "gstatic.com", "googleusercontent.com", "googlevideo.com", "youtube-nocookie.com"];

fn eval_main(app: &AppHandle, js: &str) {
    if let Some(w) = app.get_webview_window("main") { let _ = w.eval(js); }
}

pub fn payload(app: &AppHandle) -> Value {
    let st = app.state::<AppState>();
    let s = st.settings.lock().unwrap().clone();
    let r = st.rules.lock().unwrap().clone();
    let t = themes::find(app, &s.theme);
    json!({ "on": s.adblock, "hosts": r.hosts, "needles": r.needles, "theme": { "id": t.id, "css": themes::full_css(&t) } })
}

pub fn push_state(app: &AppHandle) {
    eval_main(app, &format!("window.__seraph&&window.__seraph.apply({})", payload(app)));
}

fn nav_ok(app: &AppHandle, u: &Url) -> bool {
    match u.scheme() {
        "about" | "blob" | "data" => true,
        "https" => {
            let h = u.host_str().unwrap_or("");
            if INTERNAL.iter().any(|d| h == *d || h.ends_with(&format!(".{d}"))) { true }
            else { let _ = app.opener().open_url(u.as_str(), None::<&str>); false } // external -> default browser
        }
        _ => false,
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") { let _ = w.show(); let _ = w.set_focus(); return; }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Seraphtube · Sanctum").inner_size(780., 700.).min_inner_size(640., 520.).transparent(true).build();
    if built.is_ok() {
        let theme = themes::find(app, &app.state::<AppState>().settings.lock().unwrap().theme);
        themes::apply_effect(app, &theme.effect);
    }
}

// ---------- commands ----------
#[tauri::command]
fn get_state(app: AppHandle, st: State<AppState>) -> Value {
    let s = st.settings.lock().unwrap().clone();
    let t = themes::find(&app, &s.theme);
    json!({ "settings": s, "themes": themes::all(&app), "css": themes::full_css(&t) })
}
#[tauri::command]
fn get_stats(st: State<AppState>) -> Value {
    json!({ "blocked": st.blocked.load(Relaxed), "native": st.native.load(Relaxed), "rules": st.rules.lock().unwrap().native })
}
#[tauri::command]
fn record_block(n: u64, st: State<AppState>) { st.blocked.fetch_add(n.min(10_000), Relaxed); }
#[tauri::command]
fn open_settings(app: AppHandle) { show_settings(&app); }

#[tauri::command]
fn set_theme(app: AppHandle, id: String) -> Result<String, String> {
    let t = themes::all(&app).into_iter().find(|t| t.id == id).ok_or("unknown theme")?;
    { let st = app.state::<AppState>(); let mut s = st.settings.lock().unwrap(); s.theme = id; s.save(&app); }
    themes::apply_effect(&app, &t.effect);
    push_state(&app); // hot-swap in the YouTube window
    Ok(themes::full_css(&t))
}
#[tauri::command]
fn set_adblock(app: AppHandle, on: bool) {
    { let st = app.state::<AppState>(); let mut s = st.settings.lock().unwrap(); s.adblock = on; s.save(&app); }
    adblock::rebuild(&app, false);
}
#[tauri::command]
fn toggle_filter_list(app: AppHandle, id: String, on: bool) {
    { let st = app.state::<AppState>(); let mut s = st.settings.lock().unwrap();
      if let Some(l) = s.lists.iter_mut().find(|l| l.id == id) { l.enabled = on; } s.save(&app); }
    adblock::rebuild(&app, false);
}
#[tauri::command]
fn add_filter_list(app: AppHandle, name: String, url: String) -> Result<(), String> {
    if !url.starts_with("https://") { return Err("Use an https:// URL".into()); }
    let st = app.state::<AppState>();
    let mut s = st.settings.lock().unwrap();
    s.lists.push(FilterList { id: format!("u{}", adblock::now()), name, url: Some(url), builtin: false, enabled: true, updated: None });
    s.save(&app);
    Ok(())
}
#[tauri::command]
fn remove_filter_list(app: AppHandle, id: String) {
    { let st = app.state::<AppState>(); let mut s = st.settings.lock().unwrap();
      s.lists.retain(|l| l.builtin || l.id != id); s.save(&app); }
    let _ = fs::remove_file(config_dir(&app).join("filters").join(format!("{id}.txt")));
    adblock::rebuild(&app, false);
}
#[tauri::command]
async fn update_filters(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || adblock::update_lists(&app)).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn open_themes_dir(app: AppHandle) -> Result<(), String> {
    let d = config_dir(&app).join("themes");
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let t = d.join("_template.json");
    if !t.exists() { fs::write(&t, TEMPLATE).map_err(|e| e.to_string())?; }
    app.opener().open_path(d.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

fn build_menu(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, label: &str, key: &str| MenuItemBuilder::with_id(id, label).accelerator(key).build(app);
    let app_m = SubmenuBuilder::new(app, "Seraphtube").about(None).separator()
        .item(&item("settings", "Settings…", "CmdOrCtrl+,")?).separator()
        .services().separator().hide().hide_others().show_all().separator().quit().build()?;
    let edit = SubmenuBuilder::new(app, "Edit").undo().redo().separator().cut().copy().paste().select_all().build()?;
    let view = SubmenuBuilder::new(app, "View")
        .item(&item("reload", "Reload", "CmdOrCtrl+R")?)
        .item(&item("back", "Back", "CmdOrCtrl+[")?)
        .item(&item("fwd", "Forward", "CmdOrCtrl+]")?)
        .item(&item("pip", "Toggle Picture in Picture", "CmdOrCtrl+Shift+P")?)
        .separator().fullscreen().build()?;
    let win = SubmenuBuilder::new(app, "Window").minimize().maximize().close_window().build()?;
    app.set_menu(MenuBuilder::new(app).items(&[&app_m, &edit, &view, &win]).build()?)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .on_page_load(|wv, p| {
            if wv.label() == "main" && matches!(p.event(), PageLoadEvent::Finished) { push_state(wv.app_handle()); }
        })
        .on_menu_event(|app, e| match e.id().as_ref() {
            "settings" => show_settings(app),
            "reload" => eval_main(app, "location.reload()"),
            "back" => eval_main(app, "history.back()"),
            "fwd" => eval_main(app, "history.forward()"),
            "pip" => eval_main(app, "(()=>{const v=document.querySelector('video.html5-main-video')||document.querySelector('video');if(!v)return;v.webkitSetPresentationMode(v.webkitPresentationMode==='picture-in-picture'?'inline':'picture-in-picture')})()"),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_state, get_stats, set_theme, set_adblock, update_filters, add_filter_list,
            remove_filter_list, toggle_filter_list, open_themes_dir, record_block, open_settings,
            performance::get_performance, performance::set_performance
        ])
        .setup(|app| {
            let h = app.handle().clone();
            let settings = Settings::load(&h);
            let theme_id = settings.theme.clone();
            app.manage(AppState::new(settings));
            build_menu(&h)?;

            // Always start on YouTube; adblock::rebuild re-navigates once the rules are attached.
            let start: Url = "https://www.youtube.com/".parse().unwrap();
            let nav = h.clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(start))
                .title("Seraphtube").inner_size(1360., 860.).min_inner_size(900., 600.)
                .transparent(true).user_agent(UA)
                .initialization_script(format!("window.__SERAPH_BOOT={};{}", payload(&h), SHELL))
                .on_navigation(move |u| nav_ok(&nav, u))
                .build()?;
            themes::apply_effect(&h, &themes::find(&h, &theme_id).effect);
            adblock::rebuild(&h, true);

            // First run, or lists older than 7 days: download EasyList/EasyPrivacy in the background.
            let h2 = h.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if adblock::needs_update(&h2) { let _ = adblock::update_lists(&h2); }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Seraphtube");
}