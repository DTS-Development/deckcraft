//! The browser sends a clipboard shortcut twice: eframe's web runner queues the key press and
//! the `copy`/`cut`/`paste` event too, while egui-winit (the desktop) turns the shortcut into
//! the event alone. DeckCraft's keys are written for the desktop, so the key press would run
//! the shortcut's command a second time (a paste then adds the app's own clipboard on top of the
//! pasted text). Dropping the presses gives the browser the desktop's input.

/// Remove the presses of the clipboard shortcuts egui-winit turns into `Copy`/`Cut`/`Paste`
/// (Cmd/Ctrl+C/X/V, the Copy/Cut/Paste keys, Ctrl+Insert, Shift+Insert). Releases stay: the
/// picture-paste check reads the V release.
pub(crate) fn drop_clipboard_key_presses(events: &mut Vec<egui::Event>) {
    events.retain(|e| !matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if is_clipboard_shortcut(*key, *modifiers)));
}

fn is_clipboard_shortcut(key: egui::Key, m: egui::Modifiers) -> bool {
    use egui::Key;
    match key {
        Key::Copy | Key::Cut | Key::Paste => true,
        Key::C | Key::X | Key::V => m.command,
        Key::Insert => !m.alt && (m.shift != (m.ctrl || m.command)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers};

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }
    }

    fn cmd() -> Modifiers {
        Modifiers::COMMAND
    }

    fn filtered(mut events: Vec<Event>) -> Vec<Event> {
        drop_clipboard_key_presses(&mut events);
        events
    }

    #[test]
    fn paste_keeps_only_the_paste_event() {
        let out = filtered(vec![key(Key::V, true, cmd()), Event::Paste("x".into()), key(Key::V, false, cmd())]);
        assert_eq!(out, vec![Event::Paste("x".into()), key(Key::V, false, cmd())]);
    }

    #[test]
    fn paste_variants_with_shift_or_alt_are_dropped_too() {
        for m in [cmd() | Modifiers::SHIFT, cmd() | Modifiers::ALT, cmd() | Modifiers::SHIFT | Modifiers::ALT] {
            assert!(filtered(vec![key(Key::V, true, m)]).is_empty(), "{m:?}");
        }
    }

    #[test]
    fn copy_and_cut_presses_are_dropped_and_events_stay() {
        let out = filtered(vec![key(Key::C, true, cmd()), Event::Copy, key(Key::X, true, cmd()), Event::Cut]);
        assert_eq!(out, vec![Event::Copy, Event::Cut]);
    }

    #[test]
    fn clipboard_keys_and_insert_shortcuts_are_dropped() {
        for k in [Key::Copy, Key::Cut, Key::Paste] {
            assert!(filtered(vec![key(k, true, Modifiers::NONE)]).is_empty());
        }
        for m in [Modifiers::SHIFT, Modifiers::CTRL, cmd()] {
            assert!(filtered(vec![key(Key::Insert, true, m)]).is_empty(), "{m:?}");
        }
    }

    #[test]
    fn other_keys_and_releases_stay() {
        let events = vec![
            key(Key::V, true, Modifiers::NONE),
            key(Key::V, false, cmd()),
            key(Key::C, false, cmd()),
            key(Key::Z, true, cmd()),
            key(Key::Insert, true, Modifiers::NONE),
            key(Key::Insert, true, Modifiers::SHIFT | Modifiers::CTRL),
            key(Key::Delete, true, Modifiers::SHIFT),
            key(Key::A, true, cmd()),
        ];
        assert_eq!(filtered(events.clone()), events);
    }
}
