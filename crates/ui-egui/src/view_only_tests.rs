//! Read-only, chrome-less presentations a host asked for (`readOnly`, `chrome`, `zoomControls`):
//! what each option does, that it belongs to one presentation, and that no input path edits a
//! read-only one. The engine's side is tested in `deckcraft_engine::readonly`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use egui::{Event, ImeEvent, Key, Modifiers};
use serde_json::json;

use deckcraft_engine::Session;
use deckcraft_engine::hostlock::BLOCKED_TEXT;

use crate::{Inbox, InboxEvent, InboxFile, Services, SlideApp, ViewOptions};

type Harness = egui_kittest::Harness<'static, SlideApp>;

const READ_ONLY: ViewOptions = ViewOptions { read_only: true, chrome: true, zoom_controls: false };

fn harness() -> (Harness, Inbox) {
    let inbox = Inbox::default();
    let mut session = Session::with_new();
    session.lock_host();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SlideApp::new(session, Services { inbox: Some(inbox.clone()), ..Default::default() }),
    );
    h.run_steps(3);
    (h, inbox)
}

/// A presentation with one text box saying "Hello".
fn pptx() -> Vec<u8> {
    let mut s = Session::with_new();
    s.execute("insert.textBox", &json!({})).unwrap();
    s.execute("text.insert", &json!({"text": "Hello"})).unwrap();
    s.execute("text.exit", &json!({})).unwrap();
    deckcraft_engine::cmd::file::save_bytes(&s.active().unwrap().doc, "pptx").unwrap()
}

/// The host's `open`: the presentation arrives with `view` and the app steps a few frames.
fn open(h: &mut Harness, inbox: &Inbox, bytes: Vec<u8>, view: ViewOptions) {
    let id = h.state_mut().begin_host_open(view);
    inbox.lock().unwrap().push(InboxEvent::File(InboxFile { id, name: "x.pptx".into(), bytes }));
    h.run_steps(3);
}

/// The text box's id and its text on the first slide that has one.
fn text_box(app: &SlideApp) -> (u64, String) {
    let st = app.session.active().unwrap();
    let sh = st.doc.slides.iter().flat_map(|s| s.shapes.iter()).find(|s| s.text.as_ref().is_some_and(|t| t.paragraphs.iter().any(|p| p.text().contains("Hello")))).unwrap();
    (u64::from(sh.id.0), sh.text.as_ref().unwrap().paragraphs.iter().map(|p| p.text()).collect::<Vec<_>>().join("\n"))
}

fn fingerprint(app: &SlideApp) -> (usize, usize, usize) {
    let st = app.session.active().unwrap();
    (std::sync::Arc::as_ptr(&st.doc) as usize, st.history.undo.len(), usize::from(st.is_dirty()))
}

fn typed(h: &mut Harness) {
    for e in [
        Event::Text("x".into()),
        Event::Paste("pasted".into()),
        Event::Cut,
        Event::Ime(ImeEvent::Preedit { text: "ㅎ".into(), active_range_chars: None }),
        Event::Ime(ImeEvent::Commit("한".into())),
    ] {
        h.event(e);
        h.run_steps(1);
    }
    for key in [Key::Backspace, Key::Delete, Key::Enter, Key::Tab, Key::ArrowLeft] {
        h.key_press(key);
        h.run_steps(1);
    }
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    h.key_press_modifiers(Modifiers::COMMAND, Key::X);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.key_press_modifiers(Modifiers::COMMAND, Key::B);
    h.run_steps(2);
}

#[test]
fn a_read_only_presentation_takes_no_typing_input_method_paste_cut_or_shortcut() {
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), READ_ONLY);
    assert!(h.state().read_only() && h.state().session.read_only());
    let (id, text) = text_box(h.state());
    // Editing the text, and the shape selected: the two states typing reaches the document from.
    for selection in [json!({"id": id}), json!({"ids": [id]})] {
        let (cmd, p) = if selection.get("id").is_some() { ("text.edit", selection) } else { ("edit.select", selection) };
        h.state_mut().run(cmd, p).unwrap();
        h.run_steps(2);
        let before = fingerprint(h.state());
        typed(&mut h);
        assert_eq!(fingerprint(h.state()), before, "{cmd}: the presentation changed");
        assert_eq!(text_box(h.state()).1, text);
    }
    assert!(h.state().status.is_none(), "refusals are quiet: {:?}", h.state().status);
    assert!(h.output().platform_output.ime.is_none(), "the platform's input method stays off");
}

#[test]
fn the_same_input_edits_an_editable_presentation() {
    // The control for the test above: without the option the events do change the text.
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), ViewOptions::default());
    let (id, text) = text_box(h.state());
    h.state_mut().run("text.edit", json!({"id": id})).unwrap();
    h.run_steps(2);
    h.event(Event::Text("x".into()));
    h.run_steps(2);
    assert_ne!(text_box(h.state()).1, text, "typing edits when the app is not read-only");
}

#[test]
fn a_read_only_presentation_can_still_be_looked_at_and_copied() {
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), READ_ONLY);
    let (id, _) = text_box(h.state());
    for (cmd, p) in [("slide.go", json!({"index": 0})), ("text.edit", json!({"id": id})), ("text.selectAll", json!({})), ("edit.copy", json!({}))] {
        h.state_mut().run(cmd, p).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    }
    for id in ["view.zoomIn", "view.zoomOut", "view.fit", "app.find", "view.reading"] {
        assert!(h.state_mut().run(id, json!({})).is_ok(), "{id} is for looking");
        h.run_steps(1);
    }
    h.state_mut().show = None;
    assert_eq!(fingerprint(h.state()).2, 0, "looking never makes it dirty");
}

#[test]
fn a_read_only_app_refuses_edits_and_editing_dialogs_quietly() {
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), READ_ONLY);
    for id in ["slide.new", "edit.undo", "edit.paste", "format.bold", "app.palette", "app.replace", "view.outline", "view.pane", "app.insertPictureDialog", "file.save", "file.saveAs"] {
        assert!(h.state_mut().run(id, json!({})).is_err_and(|e| e.contains(BLOCKED_TEXT)), "{id} ran");
        h.run_steps(1);
    }
    assert!(h.state().status.is_none(), "status {:?}", h.state().status);
    assert!(h.state().dialog.is_none() && h.state().palette.is_none());
    // A policy turning a command on does not open the app to edits.
    h.state_mut().session.set_policy(&[("slide.new".into(), true), ("file.saveAs".into(), true)]);
    assert!(h.state_mut().run("slide.new", json!({})).is_err_and(|e| e.contains(BLOCKED_TEXT)));
}

#[test]
fn the_shortcuts_that_edit_or_open_things_do_nothing_in_a_read_only_app() {
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), READ_ONLY);
    let before = fingerprint(h.state());
    for (m, key) in [
        (Modifiers::COMMAND | Modifiers::SHIFT, Key::P),
        (Modifiers::COMMAND, Key::H),
        (Modifiers::COMMAND, Key::K),
        (Modifiers::COMMAND | Modifiers::ALT, Key::V),
        (Modifiers::COMMAND, Key::Comma),
        (Modifiers::COMMAND, Key::D),
        (Modifiers::COMMAND, Key::M),
    ] {
        h.key_press_modifiers(m, key);
        h.run_steps(2);
    }
    assert!(h.state().palette.is_none() && h.state().dialog.is_none() && !h.state().backstage_open());
    assert_eq!(fingerprint(h.state()), before);
}

#[test]
fn options_belong_to_the_presentation_they_came_with() {
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), ViewOptions { read_only: true, chrome: false, zoom_controls: true });
    assert_eq!(h.state().view(), ViewOptions { read_only: true, chrome: false, zoom_controls: true });
    assert!(h.state().session.read_only());
    // The next open has no options: it is the editor as it always was.
    open(&mut h, &inbox, pptx(), ViewOptions::default());
    assert_eq!(h.state().view(), ViewOptions::default());
    assert!(!h.state().session.read_only(), "a read-only open does not leak into the next one");
    h.state_mut().run("slide.new", json!({})).unwrap();
    // One that fails to open leaves the options as they were, in both directions.
    open(&mut h, &inbox, b"not a presentation".to_vec(), READ_ONLY);
    assert_eq!(h.state().view(), ViewOptions::default(), "a failed open does not turn read-only on");
    open(&mut h, &inbox, pptx(), READ_ONLY);
    assert!(h.state().session.read_only());
    open(&mut h, &inbox, b"not a presentation".to_vec(), ViewOptions::default());
    assert_eq!(h.state().view(), READ_ONLY, "a failed open does not lift read-only");
    assert!(h.state().session.read_only());
}

#[test]
fn chrome_off_hides_the_editor_around_the_slide_and_zoom_controls_come_back_on_request() {
    let (mut h, inbox) = harness();
    open(&mut h, &inbox, pptx(), ViewOptions::default());
    let with = h.state().slide_rect.unwrap();
    let zoom = |h: &Harness| h.ctx.memory(|m| m.areas().visible_last_frame(&egui::LayerId::new(egui::Order::Foreground, egui::Id::new("zoom_controls"))));
    assert!(!zoom(&h));
    open(&mut h, &inbox, pptx(), ViewOptions { read_only: false, chrome: false, zoom_controls: false });
    h.run_steps(3);
    let without = h.state().slide_rect.unwrap();
    assert!(without.min.y < with.min.y - 40.0, "the title bar and ribbon are gone: {with:?} -> {without:?}");
    assert!(without.height() > with.height(), "the slide has the room");
    assert!(!zoom(&h), "no zoom buttons unless asked for");
    open(&mut h, &inbox, pptx(), ViewOptions { read_only: false, chrome: false, zoom_controls: true });
    h.run_steps(3);
    assert!(zoom(&h), "zoom buttons over the slide");
    // With the ribbon on, zoomControls adds nothing: the status bar has its slider.
    open(&mut h, &inbox, pptx(), ViewOptions { read_only: false, chrome: true, zoom_controls: true });
    h.run_steps(3);
    assert!(!zoom(&h));
}

#[test]
fn a_read_only_app_has_no_right_click() {
    let (mut h, inbox) = harness();
    let secondary = || Event::PointerButton { pos: egui::pos2(300.0, 300.0), button: egui::PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE };
    let primary = || Event::PointerButton { pos: egui::pos2(300.0, 300.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
    open(&mut h, &inbox, pptx(), ViewOptions::default());
    let mut raw = egui::RawInput { events: vec![secondary(), primary()], ..Default::default() };
    h.state_mut().raw_input_hook(&mut raw);
    assert_eq!(raw.events.len(), 2, "an editable app keeps its context menu");
    open(&mut h, &inbox, pptx(), READ_ONLY);
    let mut raw = egui::RawInput { events: vec![secondary(), primary()], ..Default::default() };
    h.state_mut().raw_input_hook(&mut raw);
    assert_eq!(raw.events.len(), 1, "the right click is dropped, the left stays");
}
