//! Resource limiter: a memory ceiling and a battery saver.
//!
//! A small background thread measures
//!   * memory: this process plus its WebView2 child processes on Windows, plus the
//!     WKWebView web-content process on macOS (found through the webview itself), and
//!   * battery: percentage and whether the charger is connected.
//!
//! It pushes the result to the YouTube page every few seconds. The page (shell.js) does the
//! actual work: trimming caches and previews, recycling the page when it is idle and over the
//! limit, and switching on the battery saver. Memory figures are working-set sums, so they
//! slightly overcount shared pages; treat them as approximate.
//!
//! The thread only samples processes when a memory limit is set or the settings window is
//! open, so with both off it costs almost nothing.

use crate::settings::AppState;
use serde::Serialize;
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::{AppHandle, Manager};

#[derive(Default, Clone, Serialize)]
pub struct Status {
    pub used_mb: u32,
    pub battery_percent: Option<u32>,
    pub on_battery: bool,
    pub saver: bool,
    pub over_limit: bool,
}

const TICK: Duration = Duration::from_secs(4);
/// `pmset` is a process spawn on macOS, so the battery is re-read at most this often.
const BATTERY_EVERY: Duration = Duration::from_secs(20);

pub fn start(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("seraph-limits".into())
        .spawn(move || {
            let mut sys = System::new();
            let mut battery: Option<(Instant, Option<(u32, bool)>)> = None;
            // Let startup (splash, rules, first page load) finish before measuring anything.
            std::thread::sleep(Duration::from_secs(8));
            loop {
                tick(&app, &mut sys, &mut battery);
                std::thread::sleep(TICK);
            }
        });
    if let Err(error) = spawned {
        eprintln!("[limits] could not start monitor: {error}");
    }
}

fn tick(app: &AppHandle, sys: &mut System, battery: &mut Option<(Instant, Option<(u32, bool)>)>) {
    let st = app.state::<AppState>();
    let (ram_mb, saver_pct) = {
        let s = st.settings.lock().unwrap();
        (s.ram_limit_mb, s.battery_saver_pct)
    };
    let settings_open = app.get_webview_window("settings").is_some();

    let mut status = st.limits.lock().unwrap().clone();

    if ram_mb > 0 || settings_open {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        status.used_mb = (memory_bytes(app, sys) / (1024 * 1024)) as u32;
    } else {
        status.used_mb = 0;
    }

    let stale = battery.as_ref().is_none_or(|(at, _)| at.elapsed() >= BATTERY_EVERY);
    if stale && (saver_pct > 0 || settings_open) {
        *battery = Some((Instant::now(), read_battery()));
    }
    let reading = battery.as_ref().and_then(|(_, value)| *value);
    status.battery_percent = reading.map(|(percent, _)| percent);
    status.on_battery = reading.is_some_and(|(_, on_ac)| !on_ac);

    status.over_limit = ram_mb > 0 && status.used_mb > ram_mb;
    status.saver = saver_pct > 0
        && status.on_battery
        && status.battery_percent.is_some_and(|percent| percent <= saver_pct);

    *st.limits.lock().unwrap() = status.clone();
    push(app, &status, ram_mb, saver_pct);
}

fn push(app: &AppHandle, status: &Status, ram_mb: u32, saver_pct: u32) {
    let Some(window) = app.get_webview_window("main") else { return };
    let info = serde_json::json!({
        "over": status.over_limit,
        "saver": status.saver,
        "usedMb": status.used_mb,
        "ramMb": ram_mb,
        "saverPct": saver_pct,
        "percent": status.battery_percent,
        "onBattery": status.on_battery,
    });
    let _ = window.eval(format!("window.__seraph&&window.__seraph.limits&&window.__seraph.limits({info})"));
}

fn memory_bytes(app: &AppHandle, sys: &System) -> u64 {
    let mut members: HashSet<u32> = HashSet::new();
    members.insert(std::process::id());

    // Walk the process tree: WebView2's browser, renderer, GPU and utility processes are all
    // descendants of this process on Windows.
    loop {
        let mut grew = false;
        for (pid, process) in sys.processes() {
            if let Some(parent) = process.parent() {
                if members.contains(&parent.as_u32()) && members.insert(pid.as_u32()) {
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(pid) = web_process_pid(app) {
        members.insert(pid);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;

    members
        .iter()
        .filter_map(|pid| sys.process(Pid::from_u32(*pid)))
        .map(|process| process.memory())
        .sum()
}

/// macOS: WebKit's web-content process is not a child of the app, so ask the webview for it.
/// This uses a long-standing private selector, guarded by a runtime check; if it ever
/// disappears the figure simply covers the app process alone.
#[cfg(target_os = "macos")]
fn web_process_pid(app: &AppHandle) -> Option<u32> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CACHED: AtomicU32 = AtomicU32::new(0);

    let window = app.get_webview_window("main")?;
    let (tx, rx) = std::sync::mpsc::channel::<i32>();

    window
        .with_webview(move |platform_webview| unsafe {
            use objc2::{msg_send, runtime::AnyObject, sel};
            let pid: i32 = match (platform_webview.inner() as *const AnyObject).as_ref() {
                Some(webview) if webview.class().responds_to(sel!(_webProcessIdentifier)) => {
                    msg_send![webview, _webProcessIdentifier]
                }
                _ => 0,
            };
            let _ = tx.send(pid);
        })
        .ok()?;

    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(pid) if pid > 0 => {
            CACHED.store(pid as u32, Ordering::Relaxed);
            Some(pid as u32)
        }
        _ => match CACHED.load(Ordering::Relaxed) {
            0 => None,
            pid => Some(pid),
        },
    }
}

/// (percent, on_ac). `None` on machines without a battery.
#[cfg(target_os = "windows")]
fn read_battery() -> Option<(u32, bool)> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).ok()? };

    // BatteryFlag 128 = no system battery; 255 / percent 255 = unknown.
    if status.BatteryFlag == 128 || status.BatteryFlag == 255 || status.BatteryLifePercent > 100 {
        return None;
    }
    Some((status.BatteryLifePercent as u32, status.ACLineStatus == 1))
}

#[cfg(target_os = "macos")]
fn read_battery() -> Option<(u32, bool)> {
    let output = std::process::Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    parse_pmset(&String::from_utf8_lossy(&output.stdout))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
/// Parses `pmset -g batt`, e.g.
/// "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1) 87%; discharging; 4:12 remaining"
fn parse_pmset(text: &str) -> Option<(u32, bool)> {
    if !text.contains("InternalBattery") {
        return None;
    }
    let end = text.find('%')?;
    let digits: String = text[..end]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let percent: u32 = digits.parse().ok()?;
    Some((percent, text.contains("AC Power")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_battery_power() {
        let text = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t87%; discharging; 4:12 remaining present: true\n";
        assert_eq!(parse_pmset(text), Some((87, false)));
    }

    #[test]
    fn parses_ac_power() {
        let text = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t100%; charged; 0:00 remaining present: true\n";
        assert_eq!(parse_pmset(text), Some((100, true)));
    }

    #[test]
    fn desktops_have_no_battery() {
        assert_eq!(parse_pmset("Now drawing from 'AC Power'\n"), None);
    }
}
