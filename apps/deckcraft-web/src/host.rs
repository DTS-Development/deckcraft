//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! DeckCraft in an `<iframe>`. Messages are plain objects with a `type`:
//!
//! - `deckcraft:ready` (to the parent): `{ version }`, the app is listening; send a file now.
//! - `deckcraft:open` (from the parent): `{ name, bytes, policy }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. It opens like a presentation picked with File › Open or dropped on the window.
//!   `policy` is `{ overrides: { "<command id>": true | false } }` with the commands that differ
//!   from their grade (see `commands.json`); in the PPM build `true` turns an off command on and
//!   `false` an on command off. A missing or malformed policy changes nothing; ids that are no
//!   command and values that are no boolean are ignored.
//! - `deckcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file DeckCraft would otherwise download: Save and Save As (a .pptx presentation) and Export
//!   (such as PDF or slide images). The parent stores it and reports failures itself.
//! - `deckcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `deckcraft:failed` (to the parent): `{ error }` when the app couldn't start.
//! - `deckcraft:insert-table` (from the parent): `{ id, rows }`, rows an array of arrays of
//!   strings. It inserts a table with those cell texts in the middle of the current slide, as
//!   one undo step.
//! - `deckcraft:table-inserted` (to the parent): `{ id }` when it did.
//! - `deckcraft:table-failed` (to the parent): `{ id, error }` when it couldn't (bad rows, no
//!   presentation open).
//!
//! - `deckcraft:goto` (from the parent): `{ id, location }`, location `{ kind: "pptx", slide, text,
//!   occurrence, total? }`: slide and occurrence count from 1, slides in presentation order. It
//!   opens that slide and selects the `occurrence`-th match of `text` on it (shapes in shape-tree
//!   order, groups expanded, tables row by row; speaker notes left out), ready to edit. With
//!   `total`, the slide must hold exactly that many matches or nothing is selected and the answer
//!   is `not-found`. Past 16 waiting jumps the oldest is answered `error` and dropped.
//! - `deckcraft:goto-done` (to the parent): `{ id }` when it did.
//! - `deckcraft:goto-failed` (to the parent): `{ id, reason }`, reason `"not-found"` (no such
//!   slide or match, no presentation open yet), `"unsupported"` (a location of another kind) or
//!   `"error"` (a malformed request).
//!
//! - `deckcraft:request-bytes` (from the parent): `{ id, purpose }`, purpose `"save"`, `"check"` or
//!   `"preview"`. It asks for the presentation as it is now, as a `.pptx`.
//! - `deckcraft:bytes` (to the parent): `{ id, purpose, name, bytes }` (a transferred `ArrayBuffer`),
//!   named as Save would name it. Purpose `"save"` counts as saved, as Save does; `"check"` and
//!   `"preview"` leave the app's state alone.
//! - `deckcraft:bytes-failed` (to the parent): `{ id, purpose, reason }`; reason `"not-ready"` when
//!   the parent's presentation has not arrived, is still opening or failed to open, `"bad-purpose"` for an unknown purpose.
//! - `deckcraft:add-fonts` (from the parent): `{ id, fonts: [{ family, bytes, format }] }`, format
//!   `"ttf"` or `"otf"`. Each font goes into the font database (the font menu and the slides use
//!   it). A font is known by the `family` the parent sent, not the name inside the file, and
//!   replaces a built-in face of the same family and style, as a registered `FontFace` wins in
//!   the parent page. A family sent again is not installed twice; it counts as installed. Slides
//!   are drawn again with the new fonts.
//! - `deckcraft:fonts-added` (to the parent): `{ id, installed, failed }`, lists of family names.
//!
//! Only the parent window on the page's own origin is heard or answered. With a parent on another
//! origin there is no bridge: the app runs as if unframed. In host mode the page starts with an
//! empty presentation instead of the sample deck.
//!
//! Save of a presentation with no file name (and Save As with the DeckCraft type) hands the parent
//! a `.pptx` instead of DeckCraft's own format (`Services::save_as_pptx`), and counts as saved. An
//! explicit File › Export › DeckCraft is sent unchanged as `.deckcraft` and doesn't mark the
//! presentation saved. If handing a file to the parent fails, the user sees a message in the app
//! and the parent gets `deckcraft:failed { error }`; the presentation stays unsaved.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use deckcraft_ui_egui::host_goto::{self, MAX_TEXT_CHARS};
use deckcraft_ui_egui::host_table::{self, MAX_CELL_CHARS, MAX_COLS, MAX_ROWS};
use deckcraft_ui_egui::{Inbox, InboxEvent, InboxFile, Services, SlideApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

/// Saving one of these hands the whole presentation to the parent, which stores it: the
/// presentation counts as saved under that name, as it would after Save on the desktop.
const PRESENTATION_EXTS: &[&str] = &["pptx"];

/// The longest user name taken from `?author=`.
const MAX_AUTHOR_CHARS: usize = 200;

/// Most table requests waiting for the next frame; more are dropped.
const MAX_PENDING_TABLES: usize = 16;

/// The longest request id taken from the parent.
const MAX_ID_BYTES: usize = 200;

/// Most byte and font requests waiting for the next frame; more are dropped.
const MAX_PENDING_REQUESTS: usize = 16;

/// Most fonts taken from one `add-fonts` message; the rest count as failed.
const MAX_FONTS_PER_MESSAGE: usize = 64;

/// The largest font file taken (as `font.add` takes).
const MAX_FONT_BYTES: u32 = 64 << 20;

/// The longest font family name taken from the parent.
const MAX_FAMILY_BYTES: usize = 200;

/// A request for the presentation's bytes: its id and the purpose as the parent sent it.
struct BytesRequest {
    id: String,
    purpose: String,
}

/// A font the parent sent: its family, and the bytes if they could be read.
struct FontEntry {
    family: String,
    bytes: Option<Vec<u8>>,
}

struct FontRequest {
    id: String,
    fonts: Vec<FontEntry>,
}

/// A jump the parent asked for: its request id and where, or why the location can't be used
/// (`"unsupported"` or `"error"`).
struct GotoRequest {
    id: String,
    target: Result<(usize, String, usize, Option<usize>), &'static str>,
}

/// A table the parent asked for: its request id and the rows, or why they couldn't be read.
struct TableRequest {
    id: String,
    rows: Result<Vec<Vec<String>>, String>,
}

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// Files from the parent not yet handed to the app: `(name, bytes)`.
    opened: Rc<RefCell<Vec<(String, Vec<u8>, Vec<(String, bool)>)>>>,
    /// Tables from the parent not yet inserted.
    tables: Rc<RefCell<Vec<TableRequest>>>,
    /// Jumps from the parent not yet made.
    gotos: Rc<RefCell<Vec<GotoRequest>>>,
    /// Names of the files handed to the parent since the last frame.
    saved: Rc<RefCell<Vec<String>>>,
    /// The unsaved-changes state last reported to the parent.
    reported: Rc<Cell<Option<bool>>>,
    /// Why files couldn't be handed to the parent since the last frame.
    failures: Rc<RefCell<Vec<String>>>,
    /// Byte requests from the parent not yet answered.
    byte_requests: Rc<RefCell<Vec<BytesRequest>>>,
    /// Font batches from the parent not yet installed.
    fonts: Rc<RefCell<Vec<FontRequest>>>,
    /// Families (lower case) already installed from the parent.
    installed: Rc<RefCell<HashSet<String>>>,
}

impl Bridge {
    /// `None` when the page isn't framed by a page on its own origin.
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        // Reading another origin's location throws, which is `None` too.
        if parent.location().origin().ok()? != origin {
            return None;
        }
        Some(Self {
            window,
            parent,
            origin,
            opened: Rc::default(),
            tables: Rc::default(),
            gotos: Rc::default(),
            saved: Rc::default(),
            reported: Rc::default(),
            failures: Rc::default(),
            byte_requests: Rc::default(),
            fonts: Rc::default(),
            installed: Rc::default(),
        })
    }

    /// Send saved and exported files to the parent instead of downloading them, and hear files
    /// from it.
    pub fn connect(&self, services: &mut Services, ctx: egui::Context) {
        self.listen(ctx);
        self.block_page_save();
        let bridge = self.clone();
        services.save_as_pptx = true;
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| {
            if let Err(e) = bridge.post_save(name, bytes) {
                log::error!("couldn't hand {name} to the page: {e}");
                // The download callback can't reach the app; `sync` shows it after the frame.
                bridge.failures.borrow_mut().push(format!("Couldn't hand {name} to the page: {e}"));
                bridge.post_failed(&e);
            }
        }));
    }

    /// `?author=`: the host's name for the user signs new comments.
    pub fn set_author(app: &mut SlideApp, author: &str) {
        let author: String = author.trim().chars().take(MAX_AUTHOR_CHARS).collect();
        if !author.is_empty() {
            app.session.prefs.author = author;
        }
    }

    pub fn post_ready(&self) {
        self.send(&[("type", "deckcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]);
    }

    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "deckcraft:failed".into()), ("error", error.into())]);
    }

    /// Before each pass: files from the parent open as if picked with File › Open.
    pub fn deliver(&self, app: &mut SlideApp, inbox: &Inbox) {
        for (name, bytes, overrides) in std::mem::take(&mut *self.opened.borrow_mut()) {
            app.session.set_policy(&overrides);
            let id = app.begin_external_file("open");
            inbox.lock().unwrap_or_else(|e| e.into_inner()).push(InboxEvent::File(InboxFile { id, name, bytes }));
        }
    }

    /// Before each pass: tables from the parent go onto the current slide and the parent hears how
    /// each went.
    pub fn deliver_tables(&self, app: &mut SlideApp) {
        for TableRequest { id, rows } in std::mem::take(&mut *self.tables.borrow_mut()) {
            match rows.and_then(|rows| host_table::insert_table(app, &rows)) {
                Ok(()) => self.send(&[("type", "deckcraft:table-inserted".into()), ("id", id.as_str().into())]),
                Err(error) => self.send(&[("type", "deckcraft:table-failed".into()), ("id", id.as_str().into()), ("error", error.as_str().into())]),
            };
        }
    }

    /// Before each pass: jumps from the parent select their text and the parent hears how each
    /// went.
    pub fn deliver_gotos(&self, app: &mut SlideApp) {
        for GotoRequest { id, target } in std::mem::take(&mut *self.gotos.borrow_mut()) {
            let result = match target {
                Ok((slide, text, occurrence, total)) if app.opened_ok() && self.opened.borrow().is_empty() && !app.opening_file() => {
                    host_goto::goto(app, slide, &text, occurrence, total).map_err(host_goto::Reason::as_str)
                }
                // The presentation hasn't arrived (or is still opening): there is nothing to find yet.
                Ok(_) => Err("not-found"),
                Err(reason) => Err(reason),
            };
            match result {
                Ok(()) => self.send(&[("type", "deckcraft:goto-done".into()), ("id", id.as_str().into())]),
                Err(reason) => self.send(&[("type", "deckcraft:goto-failed".into()), ("id", id.as_str().into()), ("reason", reason.into())]),
            };
        }
    }

    /// Before each pass: fonts from the parent go into the font database and the slides are drawn
    /// again with them; the parent hears which families took.
    pub fn deliver_fonts(&self, app: &mut SlideApp) {
        for FontRequest { id, fonts } in std::mem::take(&mut *self.fonts.borrow_mut()) {
            let db = deckcraft_fonts::FontDb::global();
            let (mut installed, mut failed) = (Vec::<String>::new(), Vec::<String>::new());
            for FontEntry { family, bytes } in fonts {
                let key = family.to_lowercase();
                if installed.iter().chain(&failed).any(|f| f.to_lowercase() == key) {
                    continue;
                }
                if self.installed.borrow().contains(&key) {
                    installed.push(family);
                    continue;
                }
                let added = bytes.is_some_and(|b| db.add_uploaded_font(b, &family) > 0);
                if added {
                    self.installed.borrow_mut().insert(key);
                    installed.push(family);
                } else {
                    failed.push(family);
                }
            }
            if !installed.is_empty() {
                app.textures.invalidate();
            }
            log::info!("add-fonts {id}: installed [{}], failed [{}]", installed.join(", "), failed.join(", "));
            let list = |names: &[String]| names.iter().map(|n| JsValue::from_str(n)).collect::<js_sys::Array>();
            self.send(&[
                ("type", "deckcraft:fonts-added".into()),
                ("id", id.as_str().into()),
                ("installed", list(&installed).into()),
                ("failed", list(&failed).into()),
            ]);
        }
    }

    /// Before each pass: the parent gets the presentation as it is now for each request.
    pub fn deliver_bytes(&self, app: &mut SlideApp) {
        for BytesRequest { id, purpose } in std::mem::take(&mut *self.byte_requests.borrow_mut()) {
            let result = if !matches!(purpose.as_str(), "save" | "check" | "preview") {
                Err("bad-purpose".to_string())
            } else if !app.opened_ok() || !self.opened.borrow().is_empty() || app.opening_file() {
                Err("not-ready".to_string())
            } else if let Some(doc) = app.session.active() {
                // The name and format Save gives a presentation with no file name.
                let name = format!("{}.pptx", doc.title());
                deckcraft_engine::cmd::file::save_bytes(&doc.doc, "pptx").map(|bytes| (name, bytes)).map_err(|e| format!("failed: {e}"))
            } else {
                Err("not-ready".to_string())
            };
            let sent = result.and_then(|(name, bytes)| self.post_bytes(&id, &purpose, &name, &bytes).map(|()| name));
            match sent {
                // Only "save" counts as saved; `sync` marks it after the frame.
                Ok(name) if purpose == "save" => self.saved.borrow_mut().push(name),
                Ok(_) => {}
                Err(reason) => {
                    self.send(&[
                        ("type", "deckcraft:bytes-failed".into()),
                        ("id", id.as_str().into()),
                        ("purpose", purpose.as_str().into()),
                        ("reason", reason.as_str().into()),
                    ]);
                }
            }
        }
    }

    /// After each frame: a presentation handed to the parent this frame counts as saved under the
    /// name it was saved as, and the parent hears when unsaved changes appear or go away.
    pub fn sync(&self, app: &mut SlideApp) {
        for failure in std::mem::take(&mut *self.failures.borrow_mut()) {
            app.notify(failure);
        }
        let saved = std::mem::take(&mut *self.saved.borrow_mut());
        if let Some(name) = saved.iter().rev().find(|name| is_presentation(name))
            && let Some(doc) = app.session.active_mut()
        {
            doc.saved_doc = doc.doc.clone();
            if doc.path.is_none() {
                doc.title = std::path::Path::new(name).file_stem().map_or_else(|| name.clone(), |s| s.to_string_lossy().to_string());
            }
        }
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.reported.get() != Some(dirty) && self.send(&[("type", "deckcraft:dirty".into()), ("dirty", dirty.into())]) {
            self.reported.set(Some(dirty));
        }
    }

    fn post_save(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let message = message(&[("type", "deckcraft:save".into()), ("name", name.as_str().into()), ("bytes", buffer.clone().into())])
            .ok_or_else(|| "couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))?;
        self.saved.borrow_mut().push(name);
        Ok(())
    }

    fn post_bytes(&self, id: &str, purpose: &str, path: &str, bytes: &[u8]) -> Result<(), String> {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let message = message(&[
            ("type", "deckcraft:bytes".into()),
            ("id", id.into()),
            ("purpose", purpose.into()),
            ("name", name.as_str().into()),
            ("bytes", buffer.clone().into()),
        ])
        .ok_or_else(|| "failed: couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| format!("failed: {}", e.as_string().unwrap_or_else(|| format!("{e:?}"))))
    }

    /// Ctrl/Cmd+S must not open the browser's "save page" dialog. eframe stops it for keys the
    /// canvas hears; this also covers focus elsewhere in the frame. The app still gets the key.
    fn block_page_save(&self) {
        let on_key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|event: web_sys::KeyboardEvent| {
            if (event.ctrl_key() || event.meta_key()) && (event.key().eq_ignore_ascii_case("s") || event.code() == "KeyS") {
                event.prevent_default();
            }
        });
        if self.window.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref()).is_ok() {
            on_key.forget();
        }
    }

    fn listen(&self, ctx: egui::Context) {
        let parent = self.parent.clone();
        let origin = self.origin.clone();
        let opened = self.opened.clone();
        let tables = self.tables.clone();
        let gotos = self.gotos.clone();
        let byte_requests = self.byte_requests.clone();
        let fonts = self.fonts.clone();
        let installed = self.installed.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                return;
            }
            let data = event.data();
            match string(&data, "type").as_deref() {
                Some("deckcraft:open") => {
                    let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Presentation.pptx".to_string());
                    let Some(bytes) = bytes(&data) else { return };
                    opened.borrow_mut().push((name, bytes, policy_overrides(&data)));
                }
                Some("deckcraft:insert-table") => {
                    // Without a usable id there is nobody to answer.
                    let Some(id) = string(&data, "id").filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES) else { return };
                    if tables.borrow().len() < MAX_PENDING_TABLES {
                        tables.borrow_mut().push(TableRequest { id, rows: rows(&data) });
                    }
                }
                Some("deckcraft:goto") => {
                    let Some(id) = string(&data, "id").filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES) else { return };
                    // Past the cap the oldest waiting jump is dropped, and its parent told.
                    let dropped = {
                        let mut queue = gotos.borrow_mut();
                        let dropped = (queue.len() >= MAX_PENDING_REQUESTS).then(|| queue.remove(0));
                        queue.push(GotoRequest { id, target: goto_target(&data) });
                        dropped
                    };
                    if let Some(old) = dropped
                        && let Some(m) =
                            message(&[("type", "deckcraft:goto-failed".into()), ("id", old.id.as_str().into()), ("reason", "error".into())])
                    {
                        let _ = parent.post_message(&m, &origin);
                    }
                }
                Some("deckcraft:request-bytes") => {
                    let Some(id) = string(&data, "id").filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES) else { return };
                    // An odd purpose is answered with `bad-purpose`, echoed capped.
                    let purpose = string(&data, "purpose").unwrap_or_default().chars().take(MAX_FAMILY_BYTES).collect();
                    if byte_requests.borrow().len() < MAX_PENDING_REQUESTS {
                        byte_requests.borrow_mut().push(BytesRequest { id, purpose });
                    }
                }
                Some("deckcraft:add-fonts") => {
                    let Some(id) = string(&data, "id").filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES) else { return };
                    if fonts.borrow().len() < MAX_PENDING_REQUESTS {
                        fonts.borrow_mut().push(FontRequest { id, fonts: font_entries(&data, &installed.borrow()) });
                    }
                }
                _ => return,
            }
            ctx.request_repaint();
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

/// `location` as `(slide, text, occurrence, total)`, `total` only if sent; `"unsupported"` for another kind of location,
/// `"error"` for a pptx one that can't be used.
fn goto_target(data: &JsValue) -> Result<(usize, String, usize, Option<usize>), &'static str> {
    let location = js_sys::Reflect::get(data, &JsValue::from_str("location")).map_err(|_| "error")?;
    if !location.is_object() {
        return Err("error");
    }
    if string(&location, "kind").as_deref() != Some("pptx") {
        return Err("unsupported");
    }
    let count = |key: &str| {
        let n = js_sys::Reflect::get(&location, &JsValue::from_str(key)).ok()?.as_f64()?;
        (n.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(&n)).then_some(n as usize)
    };
    let text = string(&location, "text").filter(|t| !t.is_empty() && t.chars().count() <= MAX_TEXT_CHARS).ok_or("error")?;
    // `total` is optional; one that is sent but is no count makes the request unusable.
    let total = match js_sys::Reflect::get(&location, &JsValue::from_str("total")) {
        Ok(v) if v.is_undefined() || v.is_null() => None,
        _ => Some(count("total").ok_or("error")?),
    };
    Ok((count("slide").ok_or("error")?, text, count("occurrence").ok_or("error")?, total))
}

fn is_presentation(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| PRESENTATION_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

/// `policy.overrides` as `(command id, on)` pairs; anything else (no policy, not an object,
/// values that are no boolean) is left out.
fn policy_overrides(data: &JsValue) -> Vec<(String, bool)> {
    let overrides = js_sys::Reflect::get(data, &JsValue::from_str("policy"))
        .and_then(|p| js_sys::Reflect::get(&p, &JsValue::from_str("overrides")))
        .unwrap_or(JsValue::UNDEFINED);
    if !overrides.is_object() || js_sys::Array::is_array(&overrides) {
        return Vec::new();
    }
    js_sys::Object::entries(&js_sys::Object::from(overrides))
        .iter()
        .filter_map(|entry| {
            let pair = js_sys::Array::from(&entry);
            Some((pair.get(0).as_string()?, pair.get(1).as_bool()?))
        })
        .collect()
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// `rows` as an array of arrays of strings, no larger than a table may be (checked before the
/// values are copied out).
fn rows(data: &JsValue) -> Result<Vec<Vec<String>>, String> {
    let bad = || "rows must be an array of arrays of strings".to_string();
    let rows = js_sys::Reflect::get(data, &JsValue::from_str("rows")).map_err(|_| bad())?;
    if !js_sys::Array::is_array(&rows) {
        return Err(bad());
    }
    let rows = js_sys::Array::from(&rows);
    if rows.length() as usize > MAX_ROWS {
        return Err(format!("a table needs 1 to {MAX_ROWS} rows"));
    }
    rows.iter()
        .map(|row| {
            if !js_sys::Array::is_array(&row) {
                return Err(bad());
            }
            let row = js_sys::Array::from(&row);
            if row.length() as usize > MAX_COLS {
                return Err(format!("a row needs 1 to {MAX_COLS} cells"));
            }
            row.iter()
                .map(|cell| {
                    // UTF-16 units are never fewer than characters: a longer string is refused before it is copied.
                    let units = cell.dyn_ref::<js_sys::JsString>().map(|s| s.length() as usize).ok_or_else(bad)?;
                    if units > MAX_CELL_CHARS * 2 {
                        return Err(format!("a cell holds at most {MAX_CELL_CHARS} characters"));
                    }
                    cell.as_string().ok_or_else(bad)
                })
                .collect()
        })
        .collect()
}

/// `fonts` as family names with their bytes: a font with no usable family name is left out, one
/// whose bytes, size or format are wrong keeps its name with no bytes (it fails), and fonts past
/// the limit keep their names only.
fn font_entries(data: &JsValue, installed: &HashSet<String>) -> Vec<FontEntry> {
    let Ok(list) = js_sys::Reflect::get(data, &JsValue::from_str("fonts")) else { return Vec::new() };
    if !js_sys::Array::is_array(&list) {
        return Vec::new();
    }
    // Families already installed, or earlier in this message, keep their names only: their bytes
    // are not copied.
    let mut seen = HashSet::<String>::new();
    js_sys::Array::from(&list)
        .iter()
        .enumerate()
        .filter_map(|(i, font)| {
            let family = string(&font, "family").map(|f| f.trim().to_string()).filter(|f| !f.is_empty() && f.len() <= MAX_FAMILY_BYTES)?;
            let format_ok = matches!(string(&font, "format").as_deref(), Some("ttf" | "otf"));
            let key = family.to_lowercase();
            let wanted = !installed.contains(&key) && seen.insert(key);
            let bytes = if wanted && i < MAX_FONTS_PER_MESSAGE && format_ok { bytes_within(&font, MAX_FONT_BYTES) } else { None };
            Some(FontEntry { family, bytes })
        })
        .collect()
}

/// Like [`bytes`], but None for more than `max` bytes (checked before they are copied).
fn bytes_within(data: &JsValue, max: u32) -> Option<Vec<u8>> {
    let value = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    let len =
        if let Some(array) = value.dyn_ref::<js_sys::Uint8Array>() { array.length() } else { value.dyn_ref::<js_sys::ArrayBuffer>()?.byte_length() };
    if len > max { None } else { bytes(data) }
}

/// `bytes` as an `ArrayBuffer` or `Uint8Array`.
fn bytes(data: &JsValue) -> Option<Vec<u8>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(&bytes).to_vec())
    } else {
        None
    }
}
