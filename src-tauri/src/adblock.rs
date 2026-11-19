// EasyList-syntax filter lists -> WebKit-format content rules.
//   macOS:   WKContentRuleList                       (install_native, objc2)
//   Windows: WebView2 WebResourceRequested hook fed by the same JSON (win_filter.rs)
// Only the bundled lists feed the page-level JS layer (see Out::js), which keeps the
// per-page payload tiny.

use crate::settings::{config_dir, AppState, FilterList};

use serde_json::{json, Value};

use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tauri::{AppHandle, Manager, WebviewWindow};

const CORE: &str = include_str!("../filters/core.txt");
const GOOGLE_ADS: &str = include_str!("../filters/google-ads.txt");

const MAX_RULES: usize = 140_000;
const MAX_LIST_BYTES: u64 = 32_000_000;

const START_URL: &str = "https://www.youtube.com/";
const STALE_SECS: u64 = 7 * 86_400;

/// Hosts that make up YouTube itself. Nothing here is ever blocked at the network
/// layer: thumbnails and video come from ytimg/googlevideo, and YouTube's ad and
/// telemetry endpoints must keep answering normally (failed requests there are one
/// of the signals behind the "Experiencing interruptions?" slowdown). Ads are removed
/// from the data and the DOM instead (shell.js).
pub const FIRST_PARTY: &[&str] = &[
    "youtube.com",
    "youtube-nocookie.com",
    "youtubei.googleapis.com",
    "googlevideo.com",
    "ytimg.com",
    "ggpht.com",
    "googleusercontent.com",
    "gstatic.com",
];

const NEVER_HOST: &[&str] = &[
    "youtube.com",
    "google.com",
    "gstatic.com",
    "googlevideo.com",
    "ytimg.com",
];

/// Values stored in `AppState::native` (shown on the settings page).
const NATIVE_OK: u8 = if cfg!(windows) { 5 } else { 2 };
const NATIVE_FAIL: u8 = 3;

/// Set once the main window has left the splash page.
static NAVIGATED: AtomicBool = AtomicBool::new(false);
/// When the splash page finished loading (animation clock starts here).
static SPLASH_AT: OnceLock<Instant> = OnceLock::new();
/// Minimum time on the splash while the intro animation is enabled.
/// Matches the ~0.85 animation scale in splash.html plus headroom.
const INTRO_MS: u64 = 5000;
/// Short hold when the user disabled the full intro.
const QUICK_MS: u64 = 900;

/// Fade-out before YouTube loads.
const OUTRO_MS: u64 = 500;

/// Never leave the user on the splash indefinitely if rule setup stalls.
const STARTUP_WATCHDOG_SECS: u64 = 12;

/// Called from the page-load handler when splash.html has finished loading.
pub fn note_splash_shown() {
    let _ = SPLASH_AT.set(Instant::now());
}

#[derive(Default, Clone)]
pub struct Compiled {
    pub hosts: Vec<String>,
    pub needles: Vec<String>,
    pub native: usize,
}

#[derive(Default)]
struct Out {
    blocks: Vec<Value>,
    cosm: Vec<Value>,
    exc: Vec<Value>,
    hosts: HashSet<String>,
    needles: HashSet<String>,
    /// Collect host/needle sets for the page-level JS layer (bundled lists only).
    js: bool,
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

fn res_type(o: &str) -> Option<&'static str> {
    Some(match o {
        "script" => "script",
        "image" => "image",
        "stylesheet" => "style-sheet",
        "font" => "font",
        "media" => "media",
        "subdocument" => "document",
        "xmlhttprequest" | "xhr" | "ping" => "raw",
        _ => return None,
    })
}

/// Convert an ABP pattern into a WebKit URL-filter regular expression.
fn to_regex(p: &str) -> Option<String> {
    if p.starts_with('/') && p.ends_with('/') && p.len() > 2 {
        // Regular-expression filter syntax is not directly compatible.
        return None;
    }

    let mut s = p;
    let mut re = String::new();

    if let Some(r) = s.strip_prefix("||") {
        re.push_str("^[^:]+:(//)?([^/]+\\.)?");
        s = r;
    } else if let Some(r) = s.strip_prefix('|') {
        re.push('^');
        s = r;
    }

    let end = s.ends_with('|');

    if end {
        s = &s[..s.len() - 1];
    }

    let tail_sep = s.ends_with('^');

    if tail_sep {
        s = &s[..s.len() - 1];
    }

    for c in s.chars() {
        match c {
            '*' => re.push_str(".*"),

            '^' => re.push_str("[/:&?=]"),

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

    if tail_sep {
        re.push_str("([/:&?].*)?$");
    } else if end {
        re.push('$');
    }

    if re.is_empty() {
        None
    } else {
        Some(re)
    }
}

/// True for `||youtube.com/...`-style patterns aimed at YouTube's own hosts.
fn targets_first_party(pat: &str) -> bool {
    let Some(rest) = pat.strip_prefix("||") else {
        return false;
    };
    let host: String = rest
        .chars()
        .take_while(|c| !matches!(c, '/' | '^' | '*' | '?' | ':' | '|' | '$'))
        .collect::<String>()
        .to_ascii_lowercase();
    FIRST_PARTY
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// `ignore-previous-rules` entries that shield YouTube's own hosts from every
/// earlier block rule, including generic EasyList path patterns. WebKit content
/// rules have no alternation, so there is one rule per host.
fn first_party_exceptions() -> Vec<Value> {
    FIRST_PARTY
        .iter()
        .map(|d| {
            json!({
                "trigger": { "url-filter": format!("^[^:]+:(//)?([^/]+\\.)?{}", d.replace('.', "\\.")) },
                "action": { "type": "ignore-previous-rules" }
            })
        })
        .collect()
}

/// Convert an ABP rule into a WebKit content blocker rule.
fn convert(rule: &str, out: &mut Out) {
    let (exc, rule) = match rule.strip_prefix("@@") {
        Some(r) => (true, r),
        None => (false, rule),
    };

    let (pat, opts) = match rule.rfind('$') {
        Some(i) => (&rule[..i], &rule[i + 1..]),
        None => (rule, ""),
    };

    let mut types: Vec<&'static str> = Vec::new();
    let mut third: Option<&str> = None;

    let mut yes: Vec<String> = Vec::new();
    let mut no: Vec<String> = Vec::new();

    let mut case_sensitive = false;

    for o in opts.split(',').filter(|o| !o.is_empty()) {
        match o {
            "third-party" | "3p" => {
                third = Some("third-party");
            }

            "~third-party" | "~3p" => {
                third = Some("first-party");
            }

            "important" => {
                // WebKit does not provide an equivalent priority option.
            }

            "match-case" => {
                case_sensitive = true;
            }

            _ if o.starts_with("domain=") => {
                for d in o[7..].split('|') {
                    if d.is_empty() {
                        continue;
                    }

                    match d.strip_prefix('~') {
                        Some(x) => {
                            no.push(format!("*{x}"));
                        }

                        None => {
                            yes.push(format!("*{d}"));
                        }
                    }
                }
            }

            _ => match res_type(o) {
                Some(t) => types.push(t),
                None => {
                    // Unsupported options are skipped rather than
                    // generating incorrect blocking rules.
                    return;
                }
            },
        }
    }

    // WebKit's if-domain and unless-domain conditions cannot safely
    // represent ABP rules containing both positive and negative domains.
    // Skip these rules rather than accidentally overblocking.
    if !yes.is_empty() && !no.is_empty() {
        return;
    }

    // Never emit network rules that target YouTube's own infrastructure.
    if targets_first_party(pat) {
        return;
    }

    let Some(re) = to_regex(pat) else {
        return;
    };

    // Collect host and path patterns for the JavaScript counting layer.
    if out.js && types.is_empty() && yes.is_empty() && no.is_empty() && third.is_none() && !exc {
        if let Some(b) = pat.strip_prefix("||") {
            if let Some(h) = b.strip_suffix('^') {
                let host = h.to_ascii_lowercase();

                if !host.is_empty()
                    && host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                    && !NEVER_HOST.contains(&host.as_str())
                {
                    out.hosts.insert(host);
                }
            } else if b.len() >= 8 && b.contains('/') && !b.contains(['|', '*', '^']) {
                out.needles.insert(b.to_string());
            }
        } else if pat.len() >= 8 && pat.starts_with('/') && !pat.contains(['|', '*', '^']) {
            out.needles.insert(pat.to_string());
        }
    }

    let mut trigger = json!({
        "url-filter": re
    });

    if !types.is_empty() {
        trigger["resource-type"] = json!(types);
    }

    if let Some(load_type) = third {
        trigger["load-type"] = json!([load_type]);
    }

    if !yes.is_empty() {
        trigger["if-domain"] = json!(yes);
    } else if !no.is_empty() {
        trigger["unless-domain"] = json!(no);
    }

    if case_sensitive {
        trigger["url-filter-is-case-sensitive"] = json!(true);
    }

    let result = json!({
        "trigger": trigger,
        "action": {
            "type": if exc {
                "ignore-previous-rules"
            } else {
                "block"
            }
        }
    });

    if exc {
        out.exc.push(result);
    } else {
        out.blocks.push(result);
    }
}

/// Parse EasyList / EasyPrivacy filter-list contents.
fn parse(text: &str, cosmetic_ok: bool, out: &mut Out) {
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
            continue;
        }

        if let Some(i) = line.find("##") {
            // Cosmetic rules are accepted only from the bundled list.
            if !cosmetic_ok {
                continue;
            }

            let (doms, sel) = (&line[..i], &line[i + 2..]);

            if sel.is_empty()
                || doms.contains('~')
                || sel.contains(":-abp")
                || sel.contains(":has-text")
                || sel.contains(":xpath")
                || sel.contains("+js(")
                || sel.starts_with('^')
            {
                continue;
            }

            let mut trigger = json!({
                "url-filter": ".*"
            });

            let domains: Vec<String> = doms
                .split(',')
                .filter(|d| !d.is_empty())
                .map(|d| format!("*{d}"))
                .collect();

            if !domains.is_empty() {
                trigger["if-domain"] = json!(domains);
            }

            out.cosm.push(json!({
                "trigger": trigger,
                "action": {
                    "type": "css-display-none",
                    "selector": sel
                }
            }));
        } else if ["#@#", "#?#", "#$#", "#%#"]
            .iter()
            .any(|m| line.contains(m))
        {
            continue;
        } else {
            convert(line, out);
        }
    }
}

/// Compile enabled filter lists.
///
/// IMPORTANT: This accepts owned FilterList references only for the
/// duration of compilation. No references escape this function.
fn build(app: &AppHandle, lists: &[FilterList]) -> (String, Compiled) {
    let mut out = Out::default();

    for list in lists {
        let text = if list.builtin {
            match list.id.as_str() {
                "core" => Some(CORE.to_string()),
                "google-ads" => Some(GOOGLE_ADS.to_string()),
                _ => None,
            }
        } else {
            fs::read_to_string(filters_dir(app).join(format!("{}.txt", list.id))).ok()
        };

        if let Some(text) = text {
            out.js = list.builtin;
            parse(&text, list.builtin, &mut out);
        }
    }

    // Reserve room for exceptions first.
    // Exceptions must remain at the end of the rule array.
    out.exc.truncate(MAX_RULES);

    let remaining = MAX_RULES.saturating_sub(out.exc.len() + FIRST_PARTY.len());

    out.blocks.truncate(remaining);

    let remaining = remaining.saturating_sub(out.blocks.len());

    out.cosm.truncate(remaining);

    // Order matters: blocks, then exceptions (they only cancel earlier rules),
    // then cosmetic rules so no exception can cancel them.
    let mut all = out.blocks;

    all.extend(out.exc);
    all.extend(first_party_exceptions());
    all.extend(out.cosm);

    let native = all.len();

    let mut hosts: Vec<String> = out.hosts.into_iter().collect();
    hosts.sort_unstable();
    hosts.truncate(30_000);

    let mut needles: Vec<String> = out.needles.into_iter().collect();
    needles.sort_unstable();
    needles.truncate(2_000);

    (
        Value::Array(all).to_string(),
        Compiled {
            hosts,
            needles,
            native,
        },
    )
}

/// Wrap a one-shot callback so it can be cloned into several closures and fires at
/// most once, whichever path (success, failure, webview unavailable) gets there first.
fn once<F: FnOnce(bool) + Send + 'static>(f: F) -> impl Fn(bool) + Clone + Send + Sync + 'static {
    let slot = Arc::new(Mutex::new(Some(f)));
    move |ok| {
        if let Some(f) = slot.lock().unwrap().take() {
            f(ok);
        }
    }
}

/// What to do with the page once new rules are attached.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// First run: leave the splash and load YouTube.
    Start,
    /// User changed something: reload so the rules cover the whole page.
    Reload,
    /// Background refresh: swap rules in place, leave the page alone.
    Silent,
}

/// Leave the splash on a worker thread so a webview callback never has to sleep.
/// Timing is measured from when the splash *page* finished loading (SPLASH_AT),
/// not from process start, so a slow WebView2 init still shows the full intro.
fn leave_splash(window: WebviewWindow, intro: bool) {
    std::thread::spawn(move || {
        let hold = if intro { INTRO_MS } else { QUICK_MS };

        // Wait up to 3s for the splash page-load event (note_splash_shown).
        let wait_for_page = Instant::now();
        while SPLASH_AT.get().is_none() && wait_for_page.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(40));
        }

        // Kick CSS animations that WebView2 may have frozen on a transparent surface.
        let _ = window.eval("window.__seraphSplashKick&&window.__seraphSplashKick()");

        let elapsed = SPLASH_AT
            .get()
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let wait = hold.saturating_sub(elapsed);
        if wait > 0 {
            std::thread::sleep(Duration::from_millis(wait));
        }

        if intro {
            if let Err(error) = window.eval("document.body&&document.body.classList.add('out')") {
                eprintln!("[startup] could not start splash fade-out: {error}");
            }
            std::thread::sleep(Duration::from_millis(OUTRO_MS));
        }
        if let Err(error) = window.eval(&format!("location.replace({})", json!(START_URL))) {
            eprintln!("[startup] could not navigate to YouTube: {error}");
        }
    });
}

/// Record the engine status, then act on the page according to `mode`.
fn settle(app: &AppHandle, window: &WebviewWindow, on: bool, ok: bool, mode: Mode) {
    app.state::<AppState>().native.store(
        if !on {
            0
        } else if ok {
            NATIVE_OK
        } else {
            NATIVE_FAIL
        },
        Ordering::Relaxed,
    );

    match mode {
        Mode::Start => {
            if !NAVIGATED.swap(true, Ordering::AcqRel) {
                let intro = app.state::<AppState>().settings.lock().unwrap().intro;
                eprintln!("[startup] native rules ready (ok={ok}); leaving splash");
                leave_splash(window.clone(), intro);
            }
        }
        Mode::Reload => {
            let _ = window.eval("location.reload()");
        }
        Mode::Silent => {}
    }
}

/// If rule compilation or native installation stalls, don't leave the user on the splash.
/// This is armed before compilation starts, not after it finishes.
fn arm_watchdog(app: &AppHandle, window: &WebviewWindow) {
    let (app, window) = (app.clone(), window.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(STARTUP_WATCHDOG_SECS));
        if !NAVIGATED.swap(true, Ordering::AcqRel) {
            let blocking_on = app.state::<AppState>().settings.lock().unwrap().adblock;
            let native_status = if blocking_on { NATIVE_FAIL } else { 0 };
            app.state::<AppState>()
                .native
                .store(native_status, Ordering::Relaxed);
            let intro = app.state::<AppState>().settings.lock().unwrap().intro;
            eprintln!(
                "[startup] filter setup exceeded {STARTUP_WATCHDOG_SECS} s; continuing to YouTube"
            );
            leave_splash(window, intro);
        }
    });
}

/// Compile the enabled lists and attach them to the main webview.
///
/// First run: the window sits on a tiny splash page until the rules are attached, then
/// navigates to YouTube exactly once (no load-then-reload). If large optional lists are
/// enabled, the bundled subset is attached first and the full set is swapped in
/// afterwards, so startup never waits on a big compile.
pub fn rebuild(app: &AppHandle, first: bool) {
    rebuild_with(app, if first { Mode::Start } else { Mode::Reload });
}

pub fn rebuild_with(app: &AppHandle, mode: Mode) {
    let first = mode == Mode::Start;
    // SPLASH_AT is set from the page-load handler via note_splash_shown(), not here.
    let app = app.clone();

    std::thread::spawn(move || {
        // Arm the fallback as soon as the worker starts. Filter parsing can be slow
        // when users enable large remote lists, so it must not happen before the timer.
        let Some(window) = app.get_webview_window("main") else {
            eprintln!("[startup] main webview was unavailable during filter setup");
            return;
        };
        if first {
            arm_watchdog(&app, &window);
        }

        let st = app.state::<AppState>();
        let settings = st.settings.lock().unwrap().clone();
        let on = settings.adblock;

        let enabled: Vec<FilterList> = settings
            .lists
            .iter()
            .filter(|l| l.enabled)
            .cloned()
            .collect();
        let core: Vec<FilterList> = enabled.iter().filter(|l| l.builtin).cloned().collect();
        let has_extra = enabled.iter().any(|l| !l.builtin);

        let (full_json, core_json, compiled) = if on {
            let (full, compiled) = build(&app, &enabled);
            let core_json = has_extra.then(|| build(&app, &core).0);
            (Some(full), core_json, compiled)
        } else {
            (None, None, Compiled::default())
        };

        *st.rules.lock().unwrap() = compiled;
        st.native.store(if on { 1 } else { 0 }, Ordering::Relaxed);
        crate::push_state(&app);

        // Blocking off: detach any rules, then continue.
        if !on {
            let (a, w) = (app.clone(), window.clone());
            install_native(&window, None, move |ok| settle(&a, &w, false, ok, mode));
            return;
        }

        // First run with big optional lists: go live on the bundled rules, then upgrade.
        if first && core_json.is_some() {
            let (a, w) = (app.clone(), window.clone());
            install_native(&window, core_json, move |ok| {
                settle(&a, &w, true, ok, mode);
                std::thread::spawn(move || {
                    let a2 = a.clone();
                    install_native(&w, full_json, move |ok_full| {
                        if ok_full {
                            a2.state::<AppState>()
                                .native
                                .store(NATIVE_OK, Ordering::Relaxed);
                        }
                    });
                });
            });
            return;
        }

        // Normal path: full set, falling back to the bundled subset if it is rejected.
        let (a, w) = (app.clone(), window.clone());
        install_native(&window, full_json, move |ok| {
            if ok {
                settle(&a, &w, true, true, mode);
                return;
            }
            match core_json {
                Some(core) => {
                    eprintln!("[adblock] Full rule set rejected; falling back to bundled filters");
                    let (a2, w2) = (a.clone(), w.clone());
                    install_native(&w, Some(core), move |ok2| settle(&a2, &w2, true, ok2, mode));
                }
                None => settle(&a, &w, true, false, mode),
            }
        });
    });
}

/// Install native WebKit content rules on macOS.
#[cfg(target_os = "macos")]
fn install_native(
    window: &WebviewWindow,
    json: Option<String>,
    done: impl FnOnce(bool) + Send + 'static,
) {
    let done = once(done);
    let done_in_webview = done.clone();

    let result = window.with_webview(move |platform_webview| unsafe {
        use block2::RcBlock;
        use objc2::{rc::Retained, MainThreadMarker};
        use objc2_foundation::{NSError, NSString};
        use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore, WKUserContentController};

        let done = done_in_webview;

        let Some(controller) = Retained::retain(
            platform_webview
                .controller()
                .cast::<WKUserContentController>(),
        ) else {
            done(false);
            return;
        };

        controller.removeAllContentRuleLists();

        let Some(json) = json else {
            // Blocking disabled.
            done(true);
            return;
        };

        let Some(store) = WKContentRuleListStore::defaultStore(MainThreadMarker::new_unchecked())
        else {
            eprintln!("[adblock] Failed to initialize WKContentRuleListStore");
            done(false);
            return;
        };

        let identifier = NSString::from_str("seraphtube-rules");
        let source = NSString::from_str(&json);

        let callback = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
            let success = match (list.as_ref(), error.as_ref()) {
                (Some(list), None) => {
                    controller.addContentRuleList(list);
                    true
                }
                (_, Some(error)) => {
                    eprintln!(
                        "[adblock] Native compilation error: {}",
                        error.localizedDescription()
                    );
                    false
                }
                _ => false,
            };
            done(success);
        });

        store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
            Some(&identifier),
            Some(&source),
            Some(&*callback),
        );
    });

    if let Err(error) = result {
        eprintln!("[adblock] Failed to access native webview: {error}");
        done(false);
    }
}

/// Install request filtering on Windows (WebView2).
///
/// The matcher is built here, off the main thread, so the main thread only has to
/// attach the handler once; later rebuilds just swap the matcher.
#[cfg(target_os = "windows")]
fn install_native(
    window: &WebviewWindow,
    json: Option<String>,
    done: impl FnOnce(bool) + Send + 'static,
) {
    use crate::win_filter::{native, Matcher};

    let done = once(done);

    let matcher = match json {
        Some(json) => match Matcher::from_json(&json) {
            Ok(matcher) => Some(Arc::new(matcher)),
            Err(error) => {
                eprintln!("[adblock] WebView2 rule compilation failed: {error}");
                done(false);
                return;
            }
        },
        None => None, // blocking disabled
    };
    native::set_active(matcher);

    let done_in_webview = done.clone();
    let app = window.app_handle().clone();

    let result = window.with_webview(move |platform_webview| {
        done_in_webview(native::attach(&platform_webview, app));
    });

    if let Err(error) = result {
        eprintln!("[adblock] Failed to access native webview: {error}");
        done(false);
    }
}

/// Download and validate an HTTPS filter list.
fn fetch(url: &str) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("HTTPS URLs only".into());
    }

    let mut response = String::new();

    ureq::get(url)
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| e.to_string())?
        .into_reader()
        .take(MAX_LIST_BYTES + 1)
        .read_to_string(&mut response)
        .map_err(|e| e.to_string())?;

    if response.len() as u64 > MAX_LIST_BYTES {
        return Err("Filter list exceeds the 32 MB size limit".into());
    }

    let head = response
        .trim_start_matches('\u{feff}')
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();

    if head.starts_with("[Adblock") || head.starts_with('!') || head.starts_with("! ") {
        Ok(response)
    } else {
        Err("Downloaded content does not appear to be a filter list".into())
    }
}

/// True on first run or when an enabled remote list is older than 7 days.
pub fn needs_update(app: &AppHandle) -> bool {
    let st = app.state::<AppState>();
    let settings = st.settings.lock().unwrap();

    settings.adblock
        && settings.lists.iter().any(|list| {
            list.enabled
                && list.url.is_some()
                && list
                    .updated
                    .map_or(true, |updated| now().saturating_sub(updated) > STALE_SECS)
        })
}

/// Download enabled remote lists, cache them, save settings and rebuild.
pub fn update_lists(app: &AppHandle, quiet: bool) -> Result<String, String> {
    let st = app.state::<AppState>();

    let lists: Vec<FilterList> = st
        .settings
        .lock()
        .unwrap()
        .lists
        .iter()
        .filter(|list| list.enabled && list.url.is_some())
        .cloned()
        .collect();

    let directory = filters_dir(app);

    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let (mut success_count, mut errors) = (0usize, Vec::new());

    for list in lists {
        let Some(url) = list.url.as_deref() else {
            continue;
        };

        match fetch(url) {
            Ok(body) => {
                let destination = directory.join(format!("{}.txt", list.id));

                let temporary = directory.join(format!("{}.txt.tmp", list.id));

                // Write to a temporary file first so a failed download
                // cannot destroy the previously working filter cache.
                if let Err(error) = fs::write(&temporary, body) {
                    errors.push(format!("{}: {}", list.name, error));

                    continue;
                }

                if let Err(error) = fs::rename(&temporary, &destination) {
                    let _ = fs::remove_file(&temporary);

                    errors.push(format!("{}: {}", list.name, error));

                    continue;
                }

                if let Some(existing) = st
                    .settings
                    .lock()
                    .unwrap()
                    .lists
                    .iter_mut()
                    .find(|existing| existing.id == list.id)
                {
                    existing.updated = Some(now());
                }

                success_count += 1;
            }

            Err(error) => {
                errors.push(format!("{}: {}", list.name, error));
            }
        }
    }

    st.settings.lock().unwrap().save(app);

    if success_count > 0 {
        rebuild_with(app, if quiet { Mode::Silent } else { Mode::Reload });
    }

    Ok(format!(
        "Updated {} list(s){}",
        success_count,
        if errors.is_empty() {
            String::new()
        } else {
            format!(" · {}", errors.join("; "))
        }
    ))
}
