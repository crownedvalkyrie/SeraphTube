//! Request filtering for Windows (WebView2).
#![cfg_attr(not(windows), allow(dead_code))] // the matcher is exercised by tests on macOS too

//!
//! WebView2 has no declarative content blocker like WKContentRuleList, so the
//! WebKit JSON produced by `adblock::build` is parsed back into an in-process
//! matcher and evaluated from `WebResourceRequested`. One rule set therefore
//! feeds macOS, Linux and Windows.
//!
//! The matcher itself is plain Rust and compiles on every platform, so its tests
//! run anywhere (`cargo test win_filter`). Only `native` is Windows-specific.
//!
//! Lookup strategy (same idea as Brave's adblock engine):
//!   1. Each rule's longest literal substring goes into an Aho-Corasick automaton.
//!   2. One pass over the URL yields candidate rules.
//!   3. Only candidates are checked (resource type, party, domains, regex).
//!   Regexes compile lazily on first candidate hit, so startup stays fast.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use std::{collections::HashMap, sync::OnceLock};

const T_SCRIPT: u8 = 1;
const T_IMAGE: u8 = 2;
const T_STYLE: u8 = 4;
const T_FONT: u8 = 8;
const T_MEDIA: u8 = 16;
const T_DOC: u8 = 32;
const T_RAW: u8 = 64;
/// No `resource-type` means "everything except frame documents", so a rule can
/// never block top-level navigation by accident.
const T_DEFAULT: u8 = T_SCRIPT | T_IMAGE | T_STYLE | T_FONT | T_MEDIA | T_RAW;

const PREFIX: &str = "^[^:]+:(//)?([^/]+\\.)?";
const TAIL: &str = "([/:&?].*)?$";
const MIN_LITERAL: usize = 3;
/// Rules with no usable literal are tested on every request, so keep them few.
const MAX_GENERIC: usize = 600;

struct Rule {
    pattern: String,
    case_sensitive: bool,
    types: u8,
    party: u8, // 0 any, 1 third-party, 2 first-party
    if_domain: Vec<String>,
    unless_domain: Vec<String>,
    re: OnceLock<Option<Regex>>,
}

struct Req<'a> {
    url: &'a str,
    kind: u8,
    third: bool,
    page_host: &'a str,
}

impl Rule {
    fn regex(&self) -> Option<&Regex> {
        self.re
            .get_or_init(|| {
                RegexBuilder::new(&self.pattern)
                    .case_insensitive(!self.case_sensitive)
                    .size_limit(1 << 20)
                    .build()
                    .ok()
            })
            .as_ref()
    }

    fn matches(&self, req: &Req) -> bool {
        if self.types & req.kind == 0 {
            return false;
        }
        match self.party {
            1 if !req.third => return false,
            2 if req.third => return false,
            _ => {}
        }
        if !self.if_domain.is_empty()
            && !self
                .if_domain
                .iter()
                .any(|d| domain_match(req.page_host, d))
        {
            return false;
        }
        if self
            .unless_domain
            .iter()
            .any(|d| domain_match(req.page_host, d))
        {
            return false;
        }
        self.regex().map_or(false, |re| re.is_match(req.url))
    }
}

#[derive(Default)]
struct RuleSet {
    rules: Vec<Rule>,
    ac: Option<AhoCorasick>,
    by_literal: Vec<Vec<u32>>,
    generic: Vec<u32>,
}

impl RuleSet {
    fn build(rules: Vec<Rule>) -> Self {
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut literals: Vec<String> = Vec::new();
        let mut by_literal: Vec<Vec<u32>> = Vec::new();
        let mut generic: Vec<u32> = Vec::new();

        for (i, rule) in rules.iter().enumerate() {
            match longest_literal(&rule.pattern) {
                Some(lit) => {
                    let id = *index.entry(lit.clone()).or_insert_with(|| {
                        literals.push(lit);
                        by_literal.push(Vec::new());
                        literals.len() - 1
                    });
                    by_literal[id].push(i as u32);
                }
                None => generic.push(i as u32),
            }
        }
        generic.truncate(MAX_GENERIC);

        let ac = if literals.is_empty() {
            None
        } else {
            AhoCorasickBuilder::new()
                .ascii_case_insensitive(true)
                .build(&literals)
                .map_err(|e| eprintln!("[adblock] automaton build failed: {e}"))
                .ok()
        };

        Self {
            rules,
            ac,
            by_literal,
            generic,
        }
    }

    fn matches(&self, req: &Req) -> bool {
        if let Some(ac) = &self.ac {
            let mut seen: Vec<usize> = Vec::new();
            for m in ac.find_overlapping_iter(req.url) {
                let id = m.pattern().as_usize();
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
                if self.by_literal[id]
                    .iter()
                    .any(|&i| self.rules[i as usize].matches(req))
                {
                    return true;
                }
            }
        }
        self.generic
            .iter()
            .any(|&i| self.rules[i as usize].matches(req))
    }
}

pub struct Matcher {
    block: RuleSet,
    allow: RuleSet,
}

impl Matcher {
    /// Build from the WebKit content-rule JSON that `adblock::build` emits.
    /// Cosmetic (`css-display-none`) rules are ignored: the JS layer handles them.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let rules: Vec<Value> = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let (mut block, mut allow) = (Vec::new(), Vec::new());

        for r in &rules {
            let target = match r["action"]["type"].as_str() {
                Some("block") => &mut block,
                Some("ignore-previous-rules") => &mut allow,
                _ => continue,
            };
            let t = &r["trigger"];
            let Some(pattern) = t["url-filter"].as_str() else {
                continue;
            };

            let types = match t["resource-type"].as_array() {
                None => T_DEFAULT,
                Some(list) => list.iter().filter_map(Value::as_str).fold(0u8, |acc, s| {
                    acc | match s {
                        "script" => T_SCRIPT,
                        "image" => T_IMAGE,
                        "style-sheet" => T_STYLE,
                        "font" => T_FONT,
                        "media" => T_MEDIA,
                        "document" => T_DOC,
                        "raw" => T_RAW,
                        _ => 0,
                    }
                }),
            };
            let party = match t["load-type"][0].as_str() {
                Some("third-party") => 1,
                Some("first-party") => 2,
                _ => 0,
            };
            let domains = |key: &str| -> Vec<String> {
                t[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|d| d.trim_start_matches('*').to_ascii_lowercase())
                            .collect()
                    })
                    .unwrap_or_default()
            };

            target.push(Rule {
                pattern: pattern.to_string(),
                case_sensitive: t["url-filter-is-case-sensitive"].as_bool().unwrap_or(false),
                types,
                party,
                if_domain: domains("if-domain"),
                unless_domain: domains("unless-domain"),
                re: OnceLock::new(),
            });
        }

        Ok(Self {
            block: RuleSet::build(block),
            allow: RuleSet::build(allow),
        })
    }

    /// `ctx` is COREWEBVIEW2_WEBRESOURCE_CONTEXT as an integer.
    /// `page_url` is only evaluated for requests that survive the cheap checks.
    pub fn should_block(&self, url: &str, ctx: i32, page_url: impl FnOnce() -> String) -> bool {
        let Some(host) = host_of(url) else {
            return false;
        };
        // Wry's own custom protocols (ipc.localhost etc.) must never be touched.
        if host == "localhost" || host.ends_with(".localhost") {
            return false;
        }
        // YouTube's own hosts are never blocked (thumbnails, video segments, player API,
        // ad/telemetry endpoints). This is also the hot path: most requests on a YouTube
        // page are first-party, so they skip matching entirely.
        if crate::adblock::FIRST_PARTY.iter().any(|d| {
            host == *d
                || (host.len() > d.len()
                    && host.ends_with(d)
                    && host.as_bytes()[host.len() - d.len() - 1] == b'.')
        }) {
            return false;
        }
        let page = page_url();
        let page_host = host_of(&page).unwrap_or("");
        let req = Req {
            url,
            kind: kind_of(ctx),
            third: site(host) != site(page_host),
            page_host,
        };
        self.block.matches(&req) && !self.allow.matches(&req)
    }
}

fn kind_of(ctx: i32) -> u8 {
    match ctx {
        1 => T_DOC,
        2 => T_STYLE,
        3 => T_IMAGE,
        4 => T_MEDIA,
        5 => T_FONT,
        6 => T_SCRIPT,
        // XHR, fetch, text track, event source, websocket, manifest, ping, CSP report, other
        _ => T_RAW,
    }
}

/// Host of an http(s)/ws(s) URL, without userinfo or port. Other schemes: None.
fn host_of(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss"
    ) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = match host.strip_prefix('[') {
        Some(_) => host, // IPv6 literal: leave as-is
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then_some(host)
}

/// Approximate registrable domain (no public-suffix list; covers co.uk-style TLDs).
fn site(host: &str) -> &str {
    let mut labels = host.rsplit('.');
    let tld = labels.next().unwrap_or("");
    let sld = labels.next().unwrap_or("");
    let want =
        if tld.len() == 2 && matches!(sld, "co" | "com" | "org" | "net" | "gov" | "edu" | "ac") {
            3
        } else {
            2
        };
    let mut dots = 0;
    for (i, b) in host.bytes().enumerate().rev() {
        if b == b'.' {
            dots += 1;
            if dots == want {
                return &host[i + 1..];
            }
        }
    }
    host
}

fn domain_match(page_host: &str, domain: &str) -> bool {
    if domain.is_empty() {
        return false;
    }
    page_host == domain
        || (page_host.len() > domain.len()
            && page_host.ends_with(domain)
            && page_host.as_bytes()[page_host.len() - domain.len() - 1] == b'.')
}

/// Longest literal run inside a regex produced by `adblock::to_regex`.
/// Those regexes only use `.*`, `[/:&?=]`, escaped characters, and the fixed
/// prefix/tail above, so a simple scan is sound.
fn longest_literal(re: &str) -> Option<String> {
    let mid = re
        .strip_prefix(PREFIX)
        .or_else(|| re.strip_prefix('^'))
        .unwrap_or(re);
    let mid = mid
        .strip_suffix(TAIL)
        .or_else(|| mid.strip_suffix('$'))
        .unwrap_or(mid);

    let mut best = String::new();
    let mut cur = String::new();
    let mut chars = mid.chars();

    fn flush(cur: &mut String, best: &mut String) {
        if cur.len() > best.len() {
            *best = cur.clone();
        }
        cur.clear();
    }

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n) => cur.push(n),
                None => flush(&mut cur, &mut best),
            },
            '[' => {
                flush(&mut cur, &mut best);
                for n in chars.by_ref() {
                    if n == ']' {
                        break;
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '=' | '%' | '~') => {
                cur.push(c)
            }
            _ => flush(&mut cur, &mut best),
        }
    }
    flush(&mut cur, &mut best);

    (best.len() >= MIN_LITERAL).then(|| best.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Windows: WebView2 hook
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub mod native {
    use super::Matcher;
    use crate::settings::AppState;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    };

    use tauri::{webview::PlatformWebview, AppHandle, Manager};

    use webview2_com::{
        take_pwstr, Microsoft::Web::WebView2::Win32::*, WebResourceRequestedEventHandler,
    };

    use windows::{
        core::{w, PWSTR},
        Foundation::EventRegistrationToken,
        Win32::System::Com::IStream,
    };

    // The handler is registered once per webview and reads the live matcher.
    static ACTIVE: RwLock<Option<Arc<Matcher>>> = RwLock::new(None);
    static ATTACHED: AtomicBool = AtomicBool::new(false);

    pub fn set_active(matcher: Option<Arc<Matcher>>) {
        if let Ok(mut slot) = ACTIVE.write() {
            *slot = matcher;
        }
    }

    fn active() -> Option<Arc<Matcher>> {
        ACTIVE.read().ok().and_then(|slot| slot.clone())
    }

    /// Runs on the main thread (inside with_webview). Idempotent.
    pub fn attach(webview: &PlatformWebview, app: AppHandle) -> bool {
        if ATTACHED.load(Ordering::Acquire) {
            return true;
        }

        let attached = unsafe {
            (|| -> windows::core::Result<()> {
                let core = webview.controller().CoreWebView2()?;
                let env = webview.environment();

                core.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;

                let handler =
                    WebResourceRequestedEventHandler::create(Box::new(move |sender, args| {
                        let (Some(args), Some(matcher)) = (args, active()) else {
                            return Ok(());
                        };

                        let request = args.Request()?;

                        let mut uri = PWSTR::null();
                        request.Uri(&mut uri)?;

                        let url = take_pwstr(uri);

                        let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();

                        args.ResourceContext(&mut ctx)?;

                        let page = || {
                            sender
                                .as_ref()
                                .and_then(|s| {
                                    let mut p = PWSTR::null();
                                    s.Source(&mut p).ok()?;
                                    Some(take_pwstr(p))
                                })
                                .unwrap_or_default()
                        };

                        if matcher.should_block(&url, ctx.0, page) {
                            let response = env.CreateWebResourceResponse(
                                None::<&IStream>,
                                403,
                                w!("Blocked"),
                                w!("Content-Type: text/plain"),
                            )?;

                            args.SetResponse(&response)?;

                            app.state::<AppState>()
                                .blocked
                                .fetch_add(1, Ordering::Relaxed);
                        }

                        Ok(())
                    }));

                let mut token = EventRegistrationToken::default();

                core.add_WebResourceRequested(&handler, &mut token)?;

                Ok(())
            })()
        };

        match attached {
            Ok(()) => {
                ATTACHED.store(true, Ordering::Release);
                true
            }
            Err(error) => {
                eprintln!("[adblock] WebView2 request hook failed: {error}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = r#"[
      {"trigger":{"url-filter":"^[^:]+:(//)?([^/]+\\.)?doubleclick\\.net([/:&?].*)?$"},"action":{"type":"block"}},
      {"trigger":{"url-filter":"^[^:]+:(//)?([^/]+\\.)?youtube\\.com/pagead/"},"action":{"type":"block"}},
      {"trigger":{"url-filter":"^[^:]+:(//)?([^/]+\\.)?tracker\\.test([/:&?].*)?$","load-type":["third-party"],"resource-type":["script"]},"action":{"type":"block"}},
      {"trigger":{"url-filter":"^[^:]+:(//)?([^/]+\\.)?ads\\.example\\.com([/:&?].*)?$"},"action":{"type":"block"}},
      {"trigger":{"url-filter":".*","if-domain":["*example.org"]},"action":{"type":"css-display-none","selector":".ad"}},
      {"trigger":{"url-filter":"^[^:]+:(//)?([^/]+\\.)?ads\\.example\\.com/allowed"},"action":{"type":"ignore-previous-rules"}}
    ]"#;

    fn block(url: &str, ctx: i32) -> bool {
        Matcher::from_json(RULES)
            .unwrap()
            .should_block(url, ctx, || "https://www.youtube.com/watch?v=x".into())
    }

    #[test]
    fn blocks_host_anchored_rules() {
        assert!(block("https://ad.doubleclick.net/x.js", 6));
        assert!(block("https://DoubleClick.net/", 8));
        assert!(!block("https://notdoubleclick.net/", 6));
    }

    #[test]
    fn never_blocks_youtube_itself() {
        // Even with an explicit rule, YouTube's own endpoints must keep answering.
        assert!(!block("https://www.youtube.com/pagead/x", 8));
        assert!(!block("https://www.youtube.com/ptracking?x=1", 8));
        assert!(!block("https://www.youtube.com/watch?v=abc", 1));
        assert!(!block("https://i.ytimg.com/vi/abc/hq720.jpg", 3));
        assert!(!block(
            "https://rr3---sn-a.googlevideo.com/videoplayback",
            4
        ));
        assert!(block("https://ad.doubleclick.net/x.js", 6));
    }

    #[test]
    fn respects_type_and_party() {
        assert!(block("https://cdn.tracker.test/a.js", 6));
        assert!(!block("https://cdn.tracker.test/a.png", 3)); // wrong type
    }

    #[test]
    fn exceptions_win() {
        assert!(block("https://ads.example.com/other", 3));
        assert!(!block("https://ads.example.com/allowed.js", 6));
    }

    #[test]
    fn never_touches_internal_hosts() {
        assert!(!block("http://ipc.localhost/command", 8));
        assert!(!block("data:text/plain,hi", 8));
    }

    #[test]
    fn frame_documents_need_an_explicit_type() {
        assert!(!block("https://ads.example.com/", 1));
    }

    #[test]
    fn helpers() {
        assert_eq!(site("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(site("www.youtube.com"), "youtube.com");
        assert_eq!(host_of("https://u:p@Host.com:8443/x?y"), Some("Host.com"));
        assert!(domain_match("m.example.org", "example.org"));
        assert!(!domain_match("badexample.org", "example.org"));
        assert_eq!(
            longest_literal("^[^:]+:(//)?([^/]+\\.)?youtube\\.com/pagead/").as_deref(),
            Some("youtube.com/pagead/")
        );
    }
}
