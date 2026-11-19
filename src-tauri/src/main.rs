#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("Seraphtube supports macOS and Windows only.");

mod adblock;
mod performance;
mod settings;
mod themes;
mod win_filter;

use serde_json::{json, Value};
use settings::{config_dir, AppState, FilterList, Settings};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{atomic::Ordering::Relaxed, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{webview::PageLoadEvent, AppHandle, Manager, State, Theme, Url, WebviewUrl, WebviewWindowBuilder};
#[cfg(target_os = "macos")]
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri_plugin_opener::OpenerExt;

// Persist diagnostics even for GUI launches with no attached terminal.
static STARTUP_LOG: OnceLock<PathBuf> = OnceLock::new();

fn startup_log_path() -> PathBuf {
    std::env::temp_dir().join("seraphtube-startup.log")
}

fn log_startup(message: impl AsRef<str>) {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let line = format!("{stamp} {}\n", message.as_ref());
    eprint!("{line}");
    let temp = startup_log_path();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&temp) {
        let _ = f.write_all(line.as_bytes());
    }
    if let Some(path) = STARTUP_LOG.get() {
        if path != &temp {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }
}

fn init_startup_log(app: &AppHandle) {
    match app.path().app_config_dir() {
        Ok(dir) => {
            if let Err(e) = fs::create_dir_all(&dir) {
                log_startup(format!("[log] cannot create config directory {}: {e}", dir.display()));
                return;
            }
            let path = dir.join("startup.log");
            let temp = startup_log_path();
            if path != temp && temp.exists() {
                if let Err(e) = fs::copy(&temp, &path) {
                    log_startup(format!("[log] cannot copy early log to {}: {e}", path.display()));
                }
            }
            let _ = STARTUP_LOG.set(path.clone());
            log_startup(format!("[log] config log: {}", path.display()));
            log_startup(format!("[log] fallback log: {}", temp.display()));
        }
        Err(e) => log_startup(format!("[log] cannot resolve app config directory: {e}")),
    }
}

// Safari-like UA: Google sign-in is much more likely to accept it than the bare WKWebView UA.
// macOS only. WebView2 is Chromium and keeps its own UA, because a Safari-on-Mac UA on
// Chromium makes YouTube pick Safari code paths.
#[cfg(target_os = "macos")]
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
const SHELL: &str = include_str!("../../src/shell.js");
const TEMPLATE: &str = include_str!("../../src/themes/_template.json");
const INTERNAL: &[&str] = &["youtube.com", "youtu.be", "google.com", "gstatic.com", "googleusercontent.com", "googlevideo.com", "youtube-nocookie.com"];

// Safari/WebKit uses webkitSetPresentationMode; Chromium uses the standard API.
// (On Windows, shell.js handles Ctrl+Shift+P itself so the call has a user gesture.)
#[cfg(target_os = "macos")]
const PIP_JS: &str = "(()=>{const v=document.querySelector('video.html5-main-video')||document.querySelector('video');if(!v)return;if(typeof v.webkitSetPresentationMode==='function'){v.webkitSetPresentationMode(v.webkitPresentationMode==='picture-in-picture'?'inline':'picture-in-picture')}else if(document.pictureInPictureElement){document.exitPictureInPicture().catch(()=>{})}else if(v.requestPictureInPicture){v.requestPictureInPicture().catch(()=>{})}})()";

fn eval_main(app: &AppHandle, js: &str) {
    if let Some(w) = app.get_webview_window("main") { let _ = w.eval(js); }
}

pub fn payload(app: &AppHandle) -> Value {
    let st = app.state::<AppState>();
    let s = st.settings.lock().unwrap().clone();
    let r = st.rules.lock().unwrap().clone();
    let t = themes::find(app, &s.theme);
    json!({ "on": s.adblock, "intro": s.intro, "hosts": r.hosts, "needles": r.needles, "theme": { "id": t.id, "css": themes::full_css(&t) } })
}

pub fn push_state(app: &AppHandle) {
    eval_main(app, &format!("window.__seraph&&window.__seraph.apply({})", payload(app)));
}

/// Local hosts used by the app itself. In `cargo tauri dev`, the frontend is
/// commonly served from `http://localhost:<port>`; Windows packaged assets use
/// `tauri.localhost`. Do not allow arbitrary HTTP pages inside the app window.
fn is_local_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "localhost" | "127.0.0.1" | "::1") || host.ends_with(".localhost")
}

fn nav_ok(app: &AppHandle, u: &Url) -> bool {
    let host = u.host_str().unwrap_or("");
    match u.scheme() {
        "about" | "blob" | "data" | "tauri" => true,
        "http" | "https" if is_local_host(host) => true,
        "https" if INTERNAL.iter().any(|d| host == *d || host.ends_with(&format!(".{d}"))) => true,
        // Send ordinary web links to the user's browser, not to the embedded webview.
        "http" | "https" => {
            let _ = app.opener().open_url(u.as_str(), None::<&str>);
            false
        }
        _ => {
            eprintln!("[nav] blocked unsupported navigation: {u}");
            false
        }
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") { let _ = w.show(); let _ = w.set_focus(); return; }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Seraphtube · Sanctum").inner_size(880., 640.).min_inner_size(640., 480.)
        .theme(Some(Theme::Dark)).transparent(true).build();
    if built.is_ok() {
        let theme = themes::find(app, &app.state::<AppState>().settings.lock().unwrap().theme);
        themes::apply_effect(app, &theme.effect);
    }
}

#[tauri::command]
fn get_state(app: AppHandle, st: State<AppState>) -> Value {
    let settings = st.settings.lock().unwrap().clone();
    let theme = themes::find(&app, &settings.theme);

    json!({
        "settings": settings,
        "themes": themes::all(&app),
        "css": themes::full_css(&theme),
        "native": st.native.load(Relaxed),
        "rules": st.rules.lock().unwrap().native,
        "blocked": st.blocked.load(Relaxed),
        "platform": std::env::consts::OS,
        "version": app.package_info().version.to_string(),
    })
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
fn set_intro(app: AppHandle, on: bool) -> Result<(), String> {
    let st = app.state::<AppState>();

    {
        let mut settings = st
            .settings
            .lock()
            .map_err(|error| error.to_string())?;

        settings.intro = on;
        settings.save(&app);
    }

    println!(
        "[settings] Splash intro {}",
        if on { "enabled" } else { "disabled" }
    );

    Ok(())
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
    // A freshly enabled remote list has nothing cached yet: fetch it in the background.
    if on {
        let app2 = app.clone();
        std::thread::spawn(move || { if adblock::needs_update(&app2) { let _ = adblock::update_lists(&app2, true); } });
    }
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
    tauri::async_runtime::spawn_blocking(move || adblock::update_lists(&app, false)).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn open_themes_dir(app: AppHandle) -> Result<(), String> {
    let d = config_dir(&app).join("themes");
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let t = d.join("_template.json");
    if !t.exists() { fs::write(&t, TEMPLATE).map_err(|e| e.to_string())?; }
    app.opener().open_path(d.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// macOS menu bar. Windows has no menu: shell.js handles Ctrl+, and Ctrl+Shift+P in-page,
/// and WebView2 already handles reload/back/forward/clipboard keys natively.
#[cfg(target_os = "macos")]
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
    std::panic::set_hook(Box::new(|panic| {
        log_startup(format!("[panic] {panic:?}\n{}", std::backtrace::Backtrace::force_capture()));
    }));
    log_startup("process entry");

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .on_page_load(|wv, p| {
            let event = match p.event() {
                PageLoadEvent::Started => "started",
                PageLoadEvent::Finished => "finished",
            };
            log_startup(format!("[page-load] {event}: {}", p.url()));
            if wv.label() == "main" && matches!(p.event(), PageLoadEvent::Finished) {
                push_state(wv.app_handle());
            }
        });

    #[cfg(target_os = "macos")]
    let builder = builder.on_menu_event(|app, e| match e.id().as_ref() {
        "settings" => show_settings(app),
        "reload" => eval_main(app, "location.reload()"),
        "back" => eval_main(app, "history.back()"),
        "fwd" => eval_main(app, "history.forward()"),
        "pip" => eval_main(app, PIP_JS),
        _ => {}
    });

    builder
        .invoke_handler(tauri::generate_handler![
            get_state, get_stats, set_theme, set_adblock, set_intro, update_filters, add_filter_list,
            remove_filter_list, toggle_filter_list, open_themes_dir, record_block, open_settings,
            performance::get_performance, performance::set_performance
        ])
        .setup(|app| {
            let h = app.handle().clone();
            init_startup_log(&h);
            log_startup("Tauri setup entered");
            let settings = Settings::load(&h);
            log_startup(format!("settings loaded: adblock={}, intro={}", settings.adblock, settings.intro));
            let theme_id = settings.theme.clone();
            app.manage(AppState::new(settings));
            #[cfg(target_os = "macos")]
            build_menu(&h)?;

            // Start on the local, animated splash page (src/splash.html); adblock::rebuild navigates to YouTube once the rules
            // are attached, so YouTube loads a single time instead of load-then-reload.
            let theme = themes::find(&h, &theme_id);
            let nav = h.clone();
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("splash.html".into()))
                .title("Seraphtube").inner_size(1360., 860.).min_inner_size(900., 600.)
                .theme(Some(Theme::Dark)).transparent(true)
                .initialization_script(format!("window.__SERAPH_BOOT={};{}", payload(&h), SHELL))
                .on_navigation(move |u| nav_ok(&nav, u));
            #[cfg(target_os = "macos")]
            let window = window.user_agent(UA);
            let built = window.build()?;
            match built.url() {
                Ok(url) => log_startup(format!("main webview built at {url}")),
                Err(error) => log_startup(format!("main webview built; URL unavailable: {error}")),
            }
            themes::apply_effect(&h, &theme.effect);
            log_startup("starting initial filter rebuild");
            adblock::rebuild(&h, true);

            // Refresh remote lists in the background (only lists the user has enabled).
            let h2 = h.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if adblock::needs_update(&h2) { let _ = adblock::update_lists(&h2, true); }
            });
            log_startup("Tauri setup completed");
            Ok(())
        })
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            log_startup(format!("[fatal] Tauri event loop returned: {error}"));
            panic!("error while running Seraphtube: {error}");
        });
}
