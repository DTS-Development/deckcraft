//! Whether the page starts DeckCraft. The PPM build (`ppm-host` feature) works only inside a
//! same-origin parent page that talks to it; opened directly it shows a notice and nothing else.

/// Shown in place of the app when a PPM build is not framed by the PPM page.
pub const OPEN_FROM_PPM: &str = "<p>PPM에서 열어 주세요</p>";

#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    Run,
    OpenFromPpm,
}

/// `host_build`: made with the `ppm-host` feature. `host_param`: the page's `?host=` value.
/// `has_bridge`: a same-origin parent page was found.
pub fn decide(host_build: bool, host_param: Option<&str>, has_bridge: bool) -> Start {
    if host_build && !(host_param == Some("parent") && has_bridge) { Start::OpenFromPpm } else { Start::Run }
}

/// Letters whose Ctrl/Cmd shortcut DeckCraft runs itself and the browser would also act on (find and
/// replace). Only the PPM host page cancels the browser's side of them.
#[cfg_attr(not(feature = "ppm-host"), allow(dead_code))]
const BROWSER_CLASHING_LETTERS: [char; 2] = ['f', 'h'];

/// Whether a key press is one of those shortcuts: Ctrl or Cmd held (not both), no Alt, and one of the letters
/// above (Shift allowed). `code` is the physical key, for layouts whose
/// `key` is not a Latin letter. Plain typing and the clipboard keys (C, V, X) are never matched.
#[cfg_attr(not(feature = "ppm-host"), allow(dead_code))]
pub fn blocks_page_default(ctrl: bool, meta: bool, alt: bool, key: &str, code: &str) -> bool {
    // Ctrl and Cmd together is the Mac's full-screen (Ctrl+Cmd+F), not a shortcut of the editor.
    if ctrl == meta || alt {
        return false;
    }
    let by_key = key.chars().count() == 1 && key.chars().next().is_some_and(|c| BROWSER_CLASHING_LETTERS.contains(&c.to_ascii_lowercase()));
    let by_code = code.strip_prefix("Key").is_some_and(|l| l.chars().count() == 1 && l.chars().next().is_some_and(|c| BROWSER_CLASHING_LETTERS.contains(&c.to_ascii_lowercase())));
    by_key || by_code
}

/// The open options this build understands, sent with `deckcraft:ready` so the parent can tell an
/// older build, which would open a presentation editable whatever it asked for, from this one.
pub const SUPPORTED_OPTIONS: &[&str] = &["readOnly", "chrome", "zoomControls"];

/// Why a request for the presentation's bytes is refused for a read-only app: nothing of it is
/// saved. `check` and `preview` only read, so they are answered.
pub fn bytes_refusal(purpose: &str, read_only: bool) -> Option<&'static str> {
    (read_only && purpose == "save").then_some("read-only")
}

/// Why a table can't go in: a read-only app takes none.
pub fn table_refusal(read_only: bool) -> Option<&'static str> {
    read_only.then_some("read-only")
}

/// Whether the parent hears of unsaved changes: a read-only presentation never has any to report.
pub fn reported_dirty(dirty: bool, read_only: bool) -> bool {
    dirty && !read_only
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_app_saves_nothing_and_reports_nothing() {
        assert_eq!(bytes_refusal("save", true), Some("read-only"));
        for purpose in ["check", "preview", "save"] {
            assert_eq!(bytes_refusal(purpose, false), None, "an editable app answers {purpose}");
        }
        assert_eq!(bytes_refusal("check", true), None);
        assert_eq!(bytes_refusal("preview", true), None);
        assert_eq!(table_refusal(true), Some("read-only"));
        assert_eq!(table_refusal(false), None);
        assert!(reported_dirty(true, false));
        assert!(!reported_dirty(true, true));
        assert!(!reported_dirty(false, false));
    }

    #[test]
    fn the_supported_options_are_the_three() {
        assert_eq!(SUPPORTED_OPTIONS, ["readOnly", "chrome", "zoomControls"]);
    }

    #[test]
    fn a_ppm_build_without_the_parent_page_opens_nothing() {
        assert_eq!(decide(true, None, false), Start::OpenFromPpm, "opened directly");
        assert_eq!(decide(true, Some("parent"), false), Start::OpenFromPpm, "no same-origin parent");
        assert_eq!(decide(true, None, true), Start::OpenFromPpm, "framed but not asked for host mode");
        assert_eq!(decide(true, Some("other"), true), Start::OpenFromPpm);
        assert_eq!(decide(true, Some("parent"), true), Start::Run);
    }

    #[test]
    fn the_upstream_build_is_unchanged() {
        for (param, bridge) in [(None, false), (Some("parent"), false), (Some("parent"), true), (None, true)] {
            assert_eq!(decide(false, param, bridge), Start::Run);
        }
    }

    #[test]
    fn the_notice_says_to_open_from_ppm() {
        assert!(OPEN_FROM_PPM.contains("PPM에서 열어 주세요"));
    }

    #[test]
    fn find_keys_lose_the_browsers_default() {
        for l in ['f', 'h'].iter().map(|c: &char| c.to_string()) {
            let up = l.to_uppercase();
            let code = format!("Key{up}");
            assert!(blocks_page_default(false, true, false, &l, &code), "Cmd+{l}");
            assert!(blocks_page_default(true, false, false, &l, &code), "Ctrl+{l}");
            assert!(blocks_page_default(false, true, false, &up, &code), "Cmd+Shift+{l}");
        }
        assert!(blocks_page_default(true, false, false, "ㄹ", "KeyF"), "Korean layout, by the physical key");
    }

    #[test]
    fn typing_the_clipboard_and_other_shortcuts_are_left_alone() {
        assert!(!blocks_page_default(false, false, false, "f", "KeyF"), "plain typing");
        assert!(!blocks_page_default(false, false, false, "G", "KeyG"), "Shift only");
        assert!(!blocks_page_default(true, false, true, "f", "KeyF"), "Ctrl+Alt+F");
        assert!(!blocks_page_default(true, true, false, "f", "KeyF"), "Ctrl+Cmd+F is full screen on a Mac");
        for (k, c) in [("c", "KeyC"), ("v", "KeyV"), ("x", "KeyX"), ("a", "KeyA"), ("z", "KeyZ"), ("Enter", "Enter"), ("Control", "ControlLeft")] {
            assert!(!blocks_page_default(true, false, false, k, c), "Ctrl+{k}");
            assert!(!blocks_page_default(false, true, false, k, c), "Cmd+{k}");
        }
    }
}
