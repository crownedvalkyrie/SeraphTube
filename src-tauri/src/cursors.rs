//! Custom cursor sets.
//!
//! Each set is a pair of 32x32 SVG images (an arrow for the default cursor and a "hand"
//! for interactive elements), tinted with the active theme's accent colour and delivered as
//! CSS inside the theme stylesheet. The same CSS styles the YouTube page and the settings
//! window, and it follows a theme change live.
//!
//! Rules of the road:
//!   * the default cursor is set once on <html> and inherited, so YouTube's own
//!     `cursor: none` (auto-hiding controls) and text cursors keep working;
//!   * the video player is never restyled (see the :not(.html5-video-player *) guard);
//!   * the "system" set emits no CSS at all.

use serde_json::{json, Value};

pub const IDS: &[(&str, &str)] = &[
    ("system", "System default"),
    ("seraph", "Seraph"),
    ("feather", "Feather"),
    ("orb", "Orb"),
    ("cross", "Cathedral cross"),
    ("ring", "Halo ring"),
];

pub fn exists(id: &str) -> bool {
    IDS.iter().any(|(known, _)| *known == id)
}

struct Art {
    arrow: String,
    hand: String,
    arrow_hot: (u8, u8),
    hand_hot: (u8, u8),
}

/// Only plain hex colours are interpolated into the SVG.
fn color(value: &str, fallback: &str) -> String {
    let ok = value.starts_with('#')
        && (4..=9).contains(&value.len())
        && value[1..].chars().all(|c| c.is_ascii_hexdigit());
    if ok { value.to_string() } else { fallback.to_string() }
}

fn svg(body: &str) -> String {
    format!("<svg xmlns='http://www.w3.org/2000/svg' width='32' height='32' viewBox='0 0 32 32'>{body}</svg>")
}

fn data_uri(svg: &str) -> String {
    let mut out = String::from("data:image/svg+xml;charset=utf-8,");
    for byte in svg.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn art(id: &str, accent: &str, outline: &str) -> Option<Art> {
    let a = color(accent, "#c8a45a");
    let o = color(outline, "#141214");

    match id {
        "seraph" => {
            let arrow = "M5 3L5 24.5L10.2 19.6L13.6 27.4L17 25.9L13.7 18.3L20.8 18.3Z";
            Some(Art {
                arrow: svg(&format!(
                    "<ellipse cx='11' cy='3.6' rx='5.4' ry='1.8' fill='none' stroke='#fff' stroke-opacity='.85' stroke-width='1.1'/>\
                     <path d='{arrow}' fill='{a}' stroke='{o}' stroke-width='1.6' stroke-linejoin='round'/>"
                )),
                hand: svg(&format!(
                    "<ellipse cx='11' cy='3.6' rx='6.2' ry='2.2' fill='none' stroke='{a}' stroke-width='1.6'/>\
                     <path d='{arrow}' fill='#fff' stroke='{o}' stroke-width='1.6' stroke-linejoin='round'/>\
                     <path d='M22 6l2-2M25 10h2.6M22 14l2 2' stroke='{a}' stroke-width='1.4' stroke-linecap='round'/>"
                )),
                arrow_hot: (5, 3),
                hand_hot: (5, 3),
            })
        }
        "feather" => {
            let body = "M4 4C11 4.5 18 9 22.5 16.5C25 20.5 25.6 24 24.6 28C20.6 26.4 17 24.2 14 20.6C8.6 14.4 5.2 9.6 4 4Z";
            let detail = format!(
                "<path d='{body}' fill='{a}' stroke='{o}' stroke-width='1.5' stroke-linejoin='round'/>\
                 <path d='M4 4L24 27' stroke='{o}' stroke-width='1.1' stroke-linecap='round'/>\
                 <path d='M10 8.5l3.4 1.2M13.5 12.6l4 .8M16.8 17.2l3.6-.2' stroke='{o}' stroke-width='.8' stroke-opacity='.7'/>"
            );
            Some(Art {
                arrow: svg(&detail),
                hand: svg(&format!("{detail}<circle cx='4' cy='4' r='3.4' fill='none' stroke='#fff' stroke-width='1.2'/>")),
                arrow_hot: (4, 4),
                hand_hot: (4, 4),
            })
        }
        "orb" => {
            let defs = format!(
                "<defs><radialGradient id='g' cx='38%' cy='35%' r='70%'>\
                 <stop offset='0' stop-color='#fff'/><stop offset='.45' stop-color='{a}'/><stop offset='1' stop-color='{o}'/>\
                 </radialGradient></defs>"
            );
            Some(Art {
                arrow: svg(&format!("{defs}<circle cx='16' cy='16' r='6.5' fill='url(#g)' stroke='{o}' stroke-width='1.2'/>")),
                hand: svg(&format!(
                    "{defs}<circle cx='16' cy='16' r='8.5' fill='url(#g)' stroke='{o}' stroke-width='1.2'/>\
                     <circle cx='16' cy='16' r='12.5' fill='none' stroke='{a}' stroke-width='1.4' stroke-dasharray='3 3'/>"
                )),
                arrow_hot: (16, 16),
                hand_hot: (16, 16),
            })
        }
        "cross" => {
            let strokes = |color: &str, width: &str| {
                format!("<path d='M16 3V29M9 11H23' stroke='{color}' stroke-width='{width}' stroke-linecap='round' fill='none'/>")
            };
            let base = format!(
                "{}{}<circle cx='16' cy='3' r='1.8' fill='{a}' stroke='{o}'/>\
                 <circle cx='9' cy='11' r='1.6' fill='{a}' stroke='{o}'/><circle cx='23' cy='11' r='1.6' fill='{a}' stroke='{o}'/>",
                strokes(&o, "5"),
                strokes(&a, "2.4"),
            );
            Some(Art {
                arrow: svg(&base),
                hand: svg(&format!(
                    "{base}<circle cx='16' cy='16' r='10' fill='none' stroke='{a}' stroke-width='1.4'/>\
                     <circle cx='16' cy='16' r='2.2' fill='#fff' stroke='{o}'/>"
                )),
                arrow_hot: (16, 16),
                hand_hot: (16, 16),
            })
        }
        "ring" => Some(Art {
            arrow: svg(&format!(
                "<circle cx='16' cy='16' r='7' fill='none' stroke='{o}' stroke-width='4.4'/>\
                 <circle cx='16' cy='16' r='7' fill='none' stroke='{a}' stroke-width='2.2'/>\
                 <circle cx='16' cy='16' r='1.6' fill='#fff' stroke='{o}' stroke-width='.8'/>"
            )),
            hand: svg(&format!(
                "<circle cx='16' cy='16' r='10.5' fill='{a}' fill-opacity='.25' stroke='{o}' stroke-width='4.4'/>\
                 <circle cx='16' cy='16' r='10.5' fill='none' stroke='{a}' stroke-width='2.2'/>\
                 <circle cx='16' cy='16' r='3.2' fill='{a}' stroke='{o}'/>"
            )),
            arrow_hot: (16, 16),
            hand_hot: (16, 16),
        }),
        _ => None,
    }
}

/// CSS for the chosen set, or an empty string for "system" / unknown ids.
pub fn css(id: &str, accent: &str, outline: &str) -> String {
    let Some(art) = art(id, accent, outline) else {
        return String::new();
    };
    format!(
        "html{{cursor:url(\"{arrow}\") {ax} {ay},auto}}\n\
         :is(a[href],button,[role=button],[role=tab],[role=link],[role=menuitem],[role=switch],summary,select,label[for],\
         input[type=checkbox],input[type=radio],input[type=submit],input[type=button],input[type=range],.theme,\
         yt-chip-cloud-chip-renderer,ytd-guide-entry-renderer,ytd-mini-guide-entry-renderer,ytd-thumbnail,\
         yt-icon-button,ytd-button-renderer,tp-yt-paper-item):not(.html5-video-player *)\
         {{cursor:url(\"{hand}\") {hx} {hy},pointer!important}}\n",
        arrow = data_uri(&art.arrow),
        hand = data_uri(&art.hand),
        ax = art.arrow_hot.0,
        ay = art.arrow_hot.1,
        hx = art.hand_hot.0,
        hy = art.hand_hot.1,
    )
}

/// Everything the settings page needs to draw the picker (previews use the same images).
pub fn catalog(accent: &str, outline: &str) -> Value {
    Value::Array(
        IDS.iter()
            .map(|(id, name)| match art(id, accent, outline) {
                Some(art) => json!({ "id": id, "name": name, "arrow": data_uri(&art.arrow), "hand": data_uri(&art.hand) }),
                None => json!({ "id": id, "name": name, "arrow": "", "hand": "" }),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_emits_nothing() {
        assert!(css("system", "#c8a45a", "#141214").is_empty());
        assert!(css("nope", "#c8a45a", "#141214").is_empty());
    }

    #[test]
    fn every_set_produces_valid_looking_css() {
        for (id, _) in IDS.iter().filter(|(id, _)| *id != "system") {
            let css = css(id, "#c8a45a", "#141214");
            assert!(css.starts_with("html{cursor:url(\"data:image/svg+xml"), "{id}");
            assert!(css.contains(":not(.html5-video-player *)"), "{id}");
            assert!(!css.contains('<'), "{id}: svg must be percent-encoded");
        }
    }

    #[test]
    fn colours_are_sanitised() {
        assert_eq!(color("#c8a45a", "#000"), "#c8a45a");
        assert_eq!(color("red; background:url(x)", "#000"), "#000");
        assert_eq!(color("#zzz", "#000"), "#000");
    }

    #[test]
    fn ids_are_known() {
        assert!(exists("seraph") && exists("system") && !exists("../etc"));
    }
}
