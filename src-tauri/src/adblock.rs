use crate::settings::{config_dir, AppState, FilterList};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, WebviewWindow};

#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
type NativeCompletion = Arc<Mutex<Option<Box<dyn FnOnce(bool) + Send>>>>;

#[cfg(target_os = "linux")]
fn complete_linux(done: &NativeCompletion, success: bool) {
    if let Ok(mut slot) = done.lock() {
        if let Some(callback) = slot.take() {
            callback(success);
        }
    }
}

const CORE: &str = include_str!("../filters/default.txt");
const MAX_RULES: usize = 40_000;
const MAX_EXCEPTIONS: usize = 10_000;
const MAX_HOSTS: usize = 30_000;
const MAX_NEEDLES: usize = 1_000;
const MAX_PATTERN_LENGTH: usize = 2048;
const START_URL: &str = "https://www.youtube.com/";
const STALE_SECS: u64 = 7 * 86_400;
const MAX_LIST_BYTES: u64 = 32_000_000;

const NEVER_HOST: &[&str] = &[
    // Core YouTube, player, image, video and Google API hosts must not be
    // added to the broad JavaScript fallback block set.
    "youtube.com",
    "youtube-nocookie.com",
    "youtubei.googleapis.com",
    "google.com",
    "googleapis.com",
    "googleusercontent.com",
    "gstatic.com",
    "googlevideo.com",
    "ytimg.com",
];

// Use the cross-version WebKit content-extension resource types. Newer
// Safari/WebKit builds expose more granular types (fetch, websocket, etc.),
// but older Linux WebKitGTK installations only accept this common subset.
// Map the newer ABP request types to `raw` below so one list compiles on both.
// ABP's negative resource-type options are converted against this same set.
const WEBKIT_RESOURCE_TYPES: &[&str] = &[
    "document",
    "image",
    "style-sheet",
    "script",
    "font",
    "raw",
    "svg-document",
    "media",
    "popup",
];

#[derive(Default, Clone)]
pub struct Compiled {
    pub hosts: Vec<String>,
    pub needles: Vec<String>,
    pub native: usize,
}

#[derive(Default)]
struct Out {
    blocks: Vec<Value>,
    exc: Vec<Value>,
    hosts: HashSet<String>,
    needles: HashSet<String>,
    // Lists frequently repeat identical network rules. Deduplicate before
    // handing the rules to WebKit so compilation and memory use stay bounded.
    seen: HashSet<String>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn filters_dir(app: &AppHandle) -> PathBuf {
    config_dir(app).join("filters")
}

fn res_type(option: &str) -> Option<&'static str> {
    Some(match option {
        "script" => "script",
        "image" => "image",
        "stylesheet" | "style-sheet" => "style-sheet",
        "font" => "font",
        "media" => "media",
        "document" | "subdocument" => "document",
        "object" | "object-subrequest" => "raw",
        // The common WebKit content-extension format has one `raw` bucket
        // for XHR/fetch/beacon/socket/other untyped resource requests.
        "xmlhttprequest" | "xhr" | "fetch" | "ping" | "websocket" | "other" | "csp-report" => "raw",
        "popup" => "popup",
        "svg-document" => "svg-document",
        _ => return None,
    })
}

/// Convert an ABP pattern to a WebKit-compatible regex.
///
/// Avoid regex alternation (`|`) because WebKit's content blocker
/// parser does not support disjunctions.
fn to_regex(pattern: &str) -> Option<String> {
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_LENGTH {
        return None;
    }

    // ABP regular-expression rules are unsupported.
    if pattern.starts_with('/') && pattern.ends_with('/') && pattern.len() > 2 {
        return None;
    }

    let mut s = pattern;
    let mut re = String::new();

    // Domain-anchored rule.
    if let Some(rest) = s.strip_prefix("||") {
        // Match a URL scheme and optional subdomain without alternation.
        re.push_str("^[^:]+://([^/]+\\.)?");
        s = rest;
    } else if let Some(rest) = s.strip_prefix('|') {
        re.push('^');
        s = rest;
    }

    let end = s.ends_with('|');

    if end {
        s = &s[..s.len() - 1];
    }

    for c in s.chars() {
        match c {
            '*' => re.push_str(".*"),

            // ABP separator: use a negated character class.
            // The end-of-string alternative is intentionally omitted.
            '^' => re.push_str("[^a-zA-Z0-9_.%\\-]"),

            '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' | '|' | '$' => {
                re.push('\\');
                re.push(c);
            }

            c if c.is_ascii() && !c.is_ascii_control() => {
                re.push(c);
            }

            _ => return None,
        }
    }

    if end {
        re.push('$');
    }

    if re.is_empty() || re.len() > MAX_PATTERN_LENGTH * 4 {
        return None;
    }

    Some(re)
}

fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '*')
}

fn convert(rule: &str, out: &mut Out, compile_native: bool) {
    let raw_rule = rule;
    let (exception, rule) = match rule.strip_prefix("@@") {
        Some(r) => (true, r),
        None => (false, rule),
    };

    let (pattern, options) = match rule.rfind('$') {
        Some(i) => (&rule[..i], &rule[i + 1..]),
        None => (rule, ""),
    };

    if pattern.is_empty() {
        return;
    }

    let mut types: Vec<&str> = Vec::new();
    let mut excluded_types: Vec<&str> = Vec::new();
    let mut third_party: Option<&str> = None;
    let mut yes: Vec<String> = Vec::new();
    let mut no: Vec<String> = Vec::new();

    for option in options.split(',').filter(|o| !o.is_empty()) {
        match option {
            "third-party" | "3p" => {
                third_party = Some("third-party");
            }

            "~third-party" | "~3p" => {
                third_party = Some("first-party");
            }

            "important" | "match-case" => {}

            _ if option.starts_with("domain=") => {
                for domain in option[7..].split('|') {
                    if let Some(domain) = domain.strip_prefix('~') {
                        if valid_domain(domain) {
                            no.push(format!("*{}", domain));
                        }
                    } else if valid_domain(domain) {
                        yes.push(format!("*{}", domain));
                    }
                }
            }

            _ => {
                let (negated, resource_option) = match option.strip_prefix('~') {
                    Some(value) => (true, value),
                    None => (false, option),
                };
                match res_type(resource_option) {
                    Some(resource_type) if negated => {
                        if !excluded_types.contains(&resource_type) {
                            excluded_types.push(resource_type);
                        }
                    }
                    Some(resource_type) => {
                        if !types.contains(&resource_type) {
                            types.push(resource_type);
                        }
                    }
                    None => return,
                }
            }
        }
    }

    // Translate ABP's negative type options into a WebKit resource-type set.
    // An empty result means the rule can never match a supported request.
    if !excluded_types.is_empty() {
        let candidates: Vec<&str> = if types.is_empty() {
            WEBKIT_RESOURCE_TYPES.to_vec()
        } else {
            types.clone()
        };
        types = candidates
            .into_iter()
            .filter(|resource_type| !excluded_types.contains(resource_type))
            .collect();
        if types.is_empty() {
            return;
        }
    }

    // The YouTube wrapper only loads YouTube as its top-level site. A plain
    // host/path rule marked third-party is therefore safe to apply to the
    // fetch/XHR/beacon paths seen by the injected fallback. First-party and
    // domain-constrained rules stay native-only to avoid false positives.
    let js_visible_types =
        types.is_empty() || types.iter().all(|resource_type| *resource_type == "raw");
    if js_visible_types
        && yes.is_empty()
        && no.is_empty()
        && third_party != Some("first-party")
        && !exception
    {
        if let Some(body) = pattern.strip_prefix("||") {
            if let Some(host) = body.strip_suffix('^') {
                let host = host.to_lowercase();

                if valid_domain(&host)
                    && !host.contains('*')
                    && !NEVER_HOST.contains(&host.as_str())
                    && out.hosts.len() < MAX_HOSTS
                {
                    out.hosts.insert(host);
                }
            } else if body.len() >= 8
                && body.contains('/')
                && !body.chars().any(|c| matches!(c, '|' | '*' | '^'))
            {
                if out.needles.len() < MAX_NEEDLES {
                    out.needles.insert(body.to_ascii_lowercase());
                }
            }
        } else if pattern.len() >= 8
            && pattern.starts_with('/')
            && !pattern.chars().any(|c| matches!(c, '|' | '*' | '^'))
        {
            if out.needles.len() < MAX_NEEDLES {
                out.needles.insert(pattern.to_ascii_lowercase());
            }
        }
    }

    if !compile_native {
        return;
    }

    let Some(regex) = to_regex(pattern) else {
        return;
    };

    let mut trigger = json!({
        "url-filter": regex
    });

    if !types.is_empty() {
        trigger["resource-type"] = json!(types);
    }

    if let Some(load_type) = third_party {
        trigger["load-type"] = json!([load_type]);
    }

    if !yes.is_empty() {
        trigger["if-domain"] = json!(yes);
    }

    if !no.is_empty() {
        trigger["unless-domain"] = json!(no);
    }

    // Bound allocations while parsing untrusted filter lists. Exceptions are
    // retained separately so a large block list cannot crowd them out.
    if (exception && out.exc.len() >= MAX_EXCEPTIONS)
        || (!exception && out.blocks.len() >= MAX_RULES)
    {
        return;
    }

    // A compact source-line key is cheaper to retain than serializing every
    // generated JSON rule into a second long-lived HashSet allocation.
    if !out.seen.insert(raw_rule.to_string()) {
        return;
    }

    let generated = json!({
        "trigger": trigger,
        "action": {
            "type": if exception {
                "ignore-previous-rules"
            } else {
                "block"
            }
        }
    });

    if exception {
        out.exc.push(generated);
    } else {
        out.blocks.push(generated);
    }
}

/// Parse ABP filters. Cosmetic selectors are intentionally skipped.
fn parse(text: &str, out: &mut Out, compile_native: bool) {
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
            continue;
        }

        if line.contains("##")
            || line.contains("#@#")
            || line.contains("#?#")
            || line.contains("#$#")
            || line.contains("#%#")
        {
            continue;
        }

        convert(line, out, compile_native);
    }
}

/// Build native rules. The bundled core is always parsed independently
/// of saved settings. Remote lists are loaded only when enabled.
fn build(app: &AppHandle, lists: &[&FilterList], compile_native: bool) -> (String, Compiled) {
    let mut out = Out::default();

    eprintln!(
        "[adblock] Loading bundled core filters: {} bytes",
        CORE.len()
    );

    parse(CORE, &mut out, compile_native);

    eprintln!(
        "[adblock] Bundled core produced {} blocking rules",
        out.blocks.len()
    );

    // Load enabled remote lists, but never load builtin a second time.
    for list in lists {
        if list.builtin {
            continue;
        }

        let path = filters_dir(app).join(format!("{}.txt", list.id));

        match fs::read_to_string(&path) {
            Ok(text) => {
                let before = out.blocks.len() + out.exc.len();

                parse(&text, &mut out, compile_native);

                let after = out.blocks.len() + out.exc.len();

                eprintln!(
                    "[adblock] Loaded {}: {} additional rules",
                    list.name,
                    after.saturating_sub(before)
                );
            }

            Err(error) => {
                eprintln!("[adblock] Could not read {}: {}", list.name, error);
            }
        }
    }

    // Native rule JSON is built for the macOS and Linux WebKit content-filter
    // engines. Windows currently uses the injected fallback and avoids the
    // unnecessary cost of serializing a large native ruleset.
    if compile_native {
        out.exc.truncate(MAX_EXCEPTIONS);

        let room = MAX_RULES.saturating_sub(out.exc.len());
        out.blocks.truncate(room);
    }

    // WebKit exceptions must follow the rules they override.
    let mut all = if compile_native {
        out.blocks
    } else {
        Vec::new()
    };
    if compile_native {
        all.extend(out.exc);
    }

    let native = all.len();

    let mut hosts: Vec<String> = out.hosts.into_iter().collect();
    hosts.sort();

    let mut needles: Vec<String> = out.needles.into_iter().collect();
    needles.sort();

    eprintln!(
        "[adblock] Prepared {} native rules, {} hosts, {} needles",
        native,
        hosts.len(),
        needles.len()
    );

    let json = if compile_native {
        Value::Array(all).to_string()
    } else {
        String::new()
    };

    eprintln!("[adblock] Serialized rule list: {} bytes", json.len());

    (
        json,
        Compiled {
            hosts,
            needles,
            native,
        },
    )
}

/// Compile and attach rules to the main webview.
pub fn rebuild(app: &AppHandle, first: bool) {
    let app = app.clone();

    std::thread::spawn(move || {
        let state = app.state::<AppState>();

        let settings = state.settings.lock().unwrap().clone();
        let enabled_adblock = settings.adblock;

        eprintln!("[adblock] Adblock enabled: {}", enabled_adblock);

        for list in &settings.lists {
            eprintln!(
                "[adblock] Filter: id={}, builtin={}, enabled={}",
                list.id, list.builtin, list.enabled
            );
        }

        let enabled: Vec<&FilterList> = settings
            .lists
            .iter()
            .filter(|l| l.enabled && !l.builtin)
            .collect();

        let native_supported = cfg!(any(target_os = "macos", target_os = "linux"));
        let (full, safe, compiled) = if enabled_adblock {
            let (full_json, compiled) = build(&app, &enabled, native_supported);

            #[cfg(any(target_os = "macos", target_os = "linux"))]
            let safe_json = Some(build(&app, &[], true).0);
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let safe_json: Option<String> = None;

            (
                if native_supported {
                    Some(full_json)
                } else {
                    None
                },
                safe_json,
                compiled,
            )
        } else {
            (None, None, Compiled::default())
        };

        *state.rules.lock().unwrap() = compiled;

        state
            .native
            .store(if enabled_adblock { 1 } else { 0 }, Ordering::Relaxed);

        crate::push_state(&app);

        let Some(window) = app.get_webview_window("main") else {
            eprintln!("[adblock] Main webview not found");
            return;
        };

        // Windows uses the injected request fallback; never report a no-op
        // native installer as success. macOS and Linux attach WebKit rules.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            state
                .native
                .store(if enabled_adblock { 4 } else { 0 }, Ordering::Relaxed);
            crate::push_state(&app);

            if first && enabled_adblock {
                let _ = window.eval(&format!("location.replace('{}')", START_URL));
            } else if !first {
                let _ = window.eval("location.reload()");
            }
            return;
        }

        let (app1, window1) = (app.clone(), window.clone());

        let finish = move |success: bool| {
            app1.state::<AppState>().native.store(
                if !enabled_adblock {
                    0
                } else if success {
                    2
                } else {
                    3
                },
                Ordering::Relaxed,
            );

            match (first, enabled_adblock) {
                (true, true) => {
                    let _ = window1.eval(&format!("location.replace('{}')", START_URL));
                }
                (true, false) => {}
                (false, _) => {
                    let _ = window1.eval("location.reload()");
                }
            }
        };

        let fallback_window = window.clone();
        let app_for_install = app.clone();
        let app_for_closure = app_for_install.clone();

        install_native(&app_for_install, &window, full, move |success| {
            if success {
                eprintln!("[adblock] Full rule set compiled successfully");
                finish(true);
            } else {
                eprintln!("[adblock] Full rules rejected. Trying bundled core.");

                let app_for_fallback = app_for_closure.clone();
                install_native(
                    &app_for_fallback,
                    &fallback_window,
                    safe,
                    move |fallback_success| {
                        if fallback_success {
                            eprintln!("[adblock] Core rules compiled successfully");
                        } else {
                            eprintln!("[adblock] ERROR: Core rules also failed");
                        }

                        finish(fallback_success);
                    },
                );
            }
        });
    }); // <-- Closes std::thread::spawn(move || {
} // <-- Closes pub fn rebuild(...)

#[cfg(target_os = "macos")]
fn install_native(
    _app: &AppHandle,
    window: &WebviewWindow,
    json: Option<String>,
    done: impl FnOnce(bool) + Send + 'static,
) {
    let _ = window.with_webview(move |platform_webview| unsafe {
        use block2::RcBlock;
        use objc2::{rc::Retained, MainThreadMarker};
        use objc2_foundation::{NSError, NSString};
        use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore, WKUserContentController};

        let Some(controller) = Retained::retain(
            platform_webview
                .controller()
                .cast::<WKUserContentController>(),
        ) else {
            eprintln!("[adblock] Could not obtain WKUserContentController");
            done(false);
            return;
        };

        let Some(json) = json else {
            controller.removeAllContentRuleLists();
            done(true);
            return;
        };

        if json == "[]" {
            controller.removeAllContentRuleLists();
            eprintln!("[adblock] No native rules available; skipping compilation");
            done(true);
            return;
        }

        let Some(store) = WKContentRuleListStore::defaultStore(MainThreadMarker::new_unchecked())
        else {
            eprintln!("[adblock] Could not initialize WKContentRuleListStore");
            done(false);
            return;
        };

        let identifier = NSString::from_str("seraphtube-rules");
        let source = NSString::from_str(&json);

        let done = std::cell::Cell::new(Some(done));

        let block = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
            let success = match (list.as_ref(), error.as_ref()) {
                (Some(rule_list), None) => {
                    // Keep the current rules live until the replacement
                    // list has compiled successfully.
                    controller.removeAllContentRuleLists();
                    controller.addContentRuleList(rule_list);
                    true
                }

                (_, Some(error)) => {
                    eprintln!(
                        "[adblock] WebKit compilation failed: {}",
                        error.localizedDescription()
                    );

                    eprintln!(
                        "[adblock] NSError domain: {}, code: {}",
                        error.domain(),
                        error.code()
                    );

                    false
                }

                _ => {
                    eprintln!("[adblock] WebKit returned no rule list and no error");
                    false
                }
            };

            if let Some(callback) = done.take() {
                callback(success);
            }
        });

        store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
            Some(&identifier),
            Some(&source),
            Some(&*block),
        );
    });
}

#[cfg(target_os = "linux")]
fn install_native(
    app: &AppHandle,
    window: &WebviewWindow,
    json: Option<String>,
    done: impl FnOnce(bool) + Send + 'static,
) {
    use std::fs;
    use webkit2gtk::{prelude::*, UserContentFilterStore};

    let store_dir = filters_dir(app).join("webkit-filter-store");
    if let Err(error) = fs::create_dir_all(&store_dir) {
        eprintln!("[adblock] Could not create WebKit filter store: {error}");
        done(false);
        return;
    }
    let store_path = store_dir.to_string_lossy().into_owned();

    let completion: NativeCompletion = Arc::new(Mutex::new(Some(Box::new(done))));
    let callback_completion = completion.clone();
    let result = window.with_webview(move |platform_webview| {
        let Some(manager) = platform_webview.inner().user_content_manager() else {
            eprintln!("[adblock] Linux WebKit view has no user content manager");
            complete_linux(&callback_completion, false);
            return;
        };

        let Some(json) = json else {
            manager.remove_all_filters();
            complete_linux(&callback_completion, true);
            return;
        };
        if json == "[]" {
            manager.remove_all_filters();
            complete_linux(&callback_completion, true);
            return;
        }

        let store = UserContentFilterStore::new(&store_path);
        let source = webkit2gtk::glib::Bytes::from(json.as_bytes());
        let async_completion = callback_completion.clone();

        webkit2gtk::glib::MainContext::default().spawn_local(async move {
            match store.save_future("seraphtube-rules", &source).await {
                Ok(filter) => {
                    // Keep the currently active filter until its replacement
                    // has compiled and is ready to attach.
                    manager.remove_all_filters();
                    manager.add_filter(&filter);
                    complete_linux(&async_completion, true);
                }
                Err(error) => {
                    eprintln!("[adblock] WebKitGTK filter compilation failed: {error}");
                    complete_linux(&async_completion, false);
                }
            }
        });
    });

    if let Err(error) = result {
        eprintln!("[adblock] Could not access Linux WebKit view: {error}");
        complete_linux(&completion, false);
    }
}

#[cfg(windows)]
fn install_native(
    _app: &AppHandle,
    _window: &WebviewWindow,
    _json: Option<String>,
    done: impl FnOnce(bool) + Send + 'static,
) {
    // WebView2 interception is intentionally not reported as active until a
    // persistent WebResourceRequested handler is installed by the app host.
    done(false);
}

/// Download an HTTPS filter list.
fn write_filter_cache(path: &std::path::Path, body: &str) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Invalid filter cache filename".to_string())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        nonce
    ));

    fs::write(&temp, body).map_err(|error| error.to_string())?;

    // std::fs::rename does not replace an existing file on Windows.
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }

    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }
    Ok(())
}

fn fetch(url: &str) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("HTTPS URLs only".into());
    }

    let mut text = String::new();
    let response = ureq::get(url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| e.to_string())?;

    response
        .into_reader()
        .take(MAX_LIST_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;

    if text.len() as u64 > MAX_LIST_BYTES {
        return Err("Filter list exceeds the 32 MB size limit".into());
    }

    let header = text.lines().next().unwrap_or("");

    if header.starts_with("[Adblock") || header.starts_with('!') {
        Ok(text)
    } else {
        Err("Not a recognized filter list".into())
    }
}

/// True on first run or when an enabled remote list is older than 7 days.
pub fn needs_update(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap();

    settings.adblock
        && settings.lists.iter().any(|list| {
            list.enabled
                && list.url.is_some()
                && list.updated.map_or(true, |timestamp| {
                    now().saturating_sub(timestamp) > STALE_SECS
                })
        })
}

/// Download enabled remote lists, cache them, and rebuild.
pub fn update_lists(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();

    let lists: Vec<FilterList> = state
        .settings
        .lock()
        .unwrap()
        .lists
        .iter()
        .filter(|list| list.enabled && list.url.is_some())
        .cloned()
        .collect();

    fs::create_dir_all(filters_dir(app)).map_err(|e| e.to_string())?;

    let (mut updated, mut errors) = (0, Vec::new());

    for list in lists {
        match fetch(list.url.as_deref().unwrap()) {
            Ok(body) => {
                write_filter_cache(&filters_dir(app).join(format!("{}.txt", list.id)), &body)?;

                if let Some(entry) = state
                    .settings
                    .lock()
                    .unwrap()
                    .lists
                    .iter_mut()
                    .find(|entry| entry.id == list.id)
                {
                    entry.updated = Some(now());
                }

                updated += 1;
            }

            Err(error) => {
                errors.push(format!("{}: {}", list.name, error));
            }
        }
    }

    state.settings.lock().unwrap().save(app);

    if updated > 0 {
        rebuild(app, false);
    }

    Ok(format!(
        "Updated {} list(s){}",
        updated,
        if errors.is_empty() {
            String::new()
        } else {
            format!(" · {}", errors.join("; "))
        }
    ))
}
