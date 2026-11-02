//! The browser sends a clipboard shortcut twice: eframe's web runner queues the key press and
//! the `copy`/`cut`/`paste` event too, while egui-winit (the desktop) turns the shortcut into
//! the event alone. DeckCraft's keys are written for the desktop, so the key press would run
//! the shortcut's command a second time (a paste then adds the app's own clipboard on top of the
//! pasted text). Dropping the presses gives the browser the desktop's input.
//!
//! Plain Cmd/Ctrl+C/X/V always lose the press. With Shift, Alt (or Ctrl on a Mac) the browser
//! may send no event (Cmd+Shift+X strikethrough, Cmd+Shift+C copy formatting), and then the
//! press is the only way to run the key map's command, so it stays unless the matching event
//! arrives in the same batch.

/// Remove the presses of the clipboard shortcuts egui-winit turns into `Copy`/`Cut`/`Paste`
/// (Cmd/Ctrl+C/X/V, the Copy/Cut/Paste keys, Ctrl+Insert, Shift+Insert; Ctrl+Insert and
/// Shift+Insert are Windows-only there, but browsers send the event on every system). Releases
/// stay: the picture-paste check reads the V release.
pub(crate) fn drop_clipboard_key_presses(events: &mut Vec<egui::Event>) {
    let has = |f: fn(&egui::Event) -> bool, events: &[egui::Event]| events.iter().any(f);
    let paste = has(|e| matches!(e, egui::Event::Paste(_)), events);
    let copy = has(|e| matches!(e, egui::Event::Copy), events);
    let cut = has(|e| matches!(e, egui::Event::Cut), events);
    events.retain(|e| !matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if is_doubled(*key, *modifiers, paste, copy, cut)));
}

/// Whether this key press repeats a clipboard event that arrived (or always arrives) with it.
fn is_doubled(key: egui::Key, m: egui::Modifiers, paste: bool, copy: bool, cut: bool) -> bool {
    use egui::Key;
    // Shift, Alt or (Mac) Control on top of Cmd: the browser's event is not certain.
    let extra = m.shift || m.alt || (m.mac_cmd && m.ctrl);
    let seen = |event: bool| m.command && (!extra || event);
    match key {
        Key::Copy | Key::Cut | Key::Paste => true,
        Key::C => seen(copy),
        Key::X => seen(cut),
        Key::V => seen(paste),
        Key::Insert => !m.alt && (m.shift != (m.ctrl || m.command)),
        // Shift+Delete cuts; the key press stays as the way to cut where no event comes.
        Key::Delete => m.shift && !m.command && !m.ctrl && !m.alt && cut,
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
    fn plain_clipboard_presses_go_even_without_an_event() {
        for k in [Key::C, Key::X, Key::V] {
            assert!(filtered(vec![key(k, true, cmd())]).is_empty(), "{k:?}");
        }
    }

    #[test]
    fn copy_and_cut_presses_are_dropped_and_events_stay() {
        let out = filtered(vec![key(Key::C, true, cmd()), Event::Copy, key(Key::X, true, cmd()), Event::Cut]);
        assert_eq!(out, vec![Event::Copy, Event::Cut]);
    }

    #[test]
    fn shift_and_alt_presses_go_only_with_their_event() {
        let combos = [(Key::V, Event::Paste("x".into())), (Key::C, Event::Copy), (Key::X, Event::Cut)];
        for m in [cmd() | Modifiers::SHIFT, cmd() | Modifiers::ALT, cmd() | Modifiers::SHIFT | Modifiers::ALT] {
            for (k, event) in &combos {
                let press = key(*k, true, m);
                assert_eq!(filtered(vec![press.clone()]), vec![press.clone()], "{k:?} {m:?} alone stays");
                assert_eq!(filtered(vec![press, event.clone()]), vec![event.clone()], "{k:?} {m:?} with its event goes");
            }
        }
    }

    #[test]
    fn a_press_stays_when_only_another_kind_of_event_came() {
        let press = key(Key::X, true, cmd() | Modifiers::SHIFT);
        assert_eq!(filtered(vec![press.clone(), Event::Paste("x".into()), Event::Copy]), vec![press, Event::Paste("x".into()), Event::Copy]);
    }

    #[test]
    fn mac_control_command_v_counts_as_an_extra_modifier() {
        let mac = Modifiers { ctrl: true, mac_cmd: true, command: true, ..Default::default() };
        let press = key(Key::V, true, mac);
        assert_eq!(filtered(vec![press.clone()]), vec![press.clone()]);
        assert_eq!(filtered(vec![press, Event::Paste("x".into())]), vec![Event::Paste("x".into())]);
        // Ctrl+V on Windows/Linux is `command` and `ctrl` with no `mac_cmd`: a plain paste.
        let win = Modifiers { ctrl: true, command: true, ..Default::default() };
        assert!(filtered(vec![key(Key::V, true, win)]).is_empty());
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
    fn shift_delete_goes_only_with_the_cut_event() {
        let press = key(Key::Delete, true, Modifiers::SHIFT);
        assert_eq!(filtered(vec![press.clone()]), vec![press.clone()]);
        assert_eq!(filtered(vec![press, Event::Cut]), vec![Event::Cut]);
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
            key(Key::Delete, true, Modifiers::NONE),
            key(Key::A, true, cmd()),
        ];
        assert_eq!(filtered(events.clone()), events);
        // A Cut event does not take a bare Delete.
        let with_cut = vec![key(Key::Delete, true, Modifiers::NONE), Event::Cut];
        assert_eq!(filtered(with_cut.clone()), with_cut);
    }
}
