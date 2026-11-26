//! Shared editing between the engine and the parent page, as plain JSON in and out (no browser
//! types), so it is tested natively. `host.rs` only moves the JSON through `postMessage`.
//!
//! One [`Shared`] per open presentation. Messages from the parent go through [`Shared::on_message`];
//! once per frame [`Shared::pump`] returns what the editor now wants the parent to hear: lock
//! requests, changed slides and the slide list, locks no longer needed.

use deckcraft_engine::Session;
use deckcraft_engine::collab::Outgoing;
use deckcraft_engine::cmd::{base64_decode, base64_encode};
use deckcraft_engine::collab_codec::{BlobStore, CodecError, MAX_BIG, content_hash, decode_order, decode_slide, encode_order, encode_slide, is_hash};
use deckcraft_engine::model::{Presentation, SlideId};
use deckcraft_ui_egui::remote_cursors::{self, RemoteCursor};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::collab_wire::{KEY_STRUCTURE, KIND_ORDER, KIND_SLIDE, Link, parse_slide_key, regions, slide_key};

/// Slides that came before the list that names them (a new slide is sent first, then the list).
const MAX_STASH: usize = 64;
/// Pictures asked for at once; the rest wait for these to arrive.
const MAX_ASKED: usize = 32;

/// What one call has to show the user and send to the parent.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    /// Messages for the parent (`deckcraft:collab-*`).
    pub send: Vec<Value>,
    /// Lines for the status bar.
    pub notes: Vec<String>,
    /// Where the other people are now (the whole list), when the parent just sent it.
    pub cursors: Option<Vec<RemoteCursor>>,
}

#[derive(Default)]
pub struct Shared {
    link: Link,
    /// Slides of others not in this presentation yet, oldest first.
    stash: Vec<(SlideId, Presentation)>,
    /// Slides whose lock was given back and the parent has not confirmed yet.
    released: HashSet<SlideId>,
    /// Bytes of pictures seen, by hash.
    blobs: BlobStore,
    /// Big pictures already sent to the parent for the server.
    uploaded: HashSet<String>,
    /// Slides of others drawn with a stand-in picture: the data as sent, and the hashes still missing.
    waiting: HashMap<SlideId, (Value, Vec<String>)>,
    /// Hashes asked of the parent, and the ones the server could not give.
    asked: HashSet<String>,
    gone: HashSet<String>,
    /// Opened with `cursors`: this editor sends its place and draws the others'.
    cursors: bool,
    /// The place last sent (`None` = nothing sent yet): the slide and its selected shapes.
    last_place: Option<Option<(SlideId, Vec<u32>)>>,
}

impl Shared {
    pub fn new() -> Self {
        Self::default()
    }

    /// With the place of this editor sent and the others' drawn (the open option `cursors`).
    pub fn with_cursors(mut self, on: bool) -> Self {
        self.cursors = on;
        self
    }

    /// `collab-cursor` when the slide or the selected shapes changed since the last one sent. The
    /// place is the open slide in the slide view (none in a master or other page view).
    fn place_msg(&mut self, s: &Session) -> Option<Value> {
        let place = s.active().filter(|d| d.selection.target == deckcraft_engine::Target::Slides).and_then(|d| {
            let id = d.current_slide()?.id;
            // Only shapes the slide has, so the others never look for one that isn't there.
            let shapes = d.selection.shapes.iter().filter(|sh| d.shape(**sh).is_some()).take(remote_cursors::MAX_SHAPES).map(|sh| sh.0).collect();
            Some((id, shapes))
        });
        if self.last_place.as_ref() == Some(&place) {
            return None;
        }
        let m = match &place {
            Some((id, shapes)) => serde_json::json!({"type": "deckcraft:collab-cursor", "region": slide_key(*id), "pos": {"shapes": shapes}}),
            None => serde_json::json!({"type": "deckcraft:collab-cursor", "region": null}),
        };
        self.last_place = Some(place);
        Some(m)
    }

    fn stash(&mut self, id: SlideId, p: Presentation) {
        self.stash.retain(|(i, _)| *i != id);
        self.stash.push((id, p));
        if self.stash.len() > MAX_STASH {
            self.stash.remove(0);
        }
    }

    /// Slide `id`'s data from someone else as a presentation. A picture whose bytes are not here
    /// yet is drawn as a stand-in and remembered so it is replaced when the bytes arrive.
    fn decode(&mut self, id: SlideId, data: &Value) -> Result<Presentation, CodecError> {
        let d = decode_slide(data, &mut self.blobs)?;
        if d.missing.is_empty() {
            self.waiting.remove(&id);
        } else {
            self.waiting.insert(id, (data.clone(), d.missing));
        }
        Ok(d.presentation)
    }

    /// Asks the parent for pictures the slides above are waiting on (a few at a time).
    fn ask(&mut self, out: &mut Out) {
        let mut want: Vec<String> = vec![];
        for (_, missing) in self.waiting.values() {
            for h in missing {
                if !self.asked.contains(h) && !self.gone.contains(h) && !want.contains(h) {
                    want.push(h.clone());
                }
            }
        }
        for h in want.into_iter().take(MAX_ASKED.saturating_sub(self.asked.len())) {
            out.send.push(serde_json::json!({"type": "deckcraft:collab-need-blob", "hash": h}));
            self.asked.insert(h);
        }
    }

    /// The bytes of a picture asked for: kept, and the slides waiting on it are drawn again.
    fn on_blob(&mut self, s: &mut Session, m: &Value, out: &mut Out) {
        let Some(hash) = m.get("hash").and_then(Value::as_str).filter(|h| is_hash(h)) else { return };
        // Only what was asked for.
        if !self.asked.remove(hash) {
            return;
        }
        let b64 = m.get("b64").and_then(Value::as_str).unwrap_or("");
        let data = (b64.len() <= MAX_BIG.div_ceil(3) * 4 + 8).then(|| base64_decode(b64)).flatten();
        match data {
            Some(d) if content_hash(&d) == hash => self.blobs.put(hash.to_string(), Arc::new(d)),
            _ => {
                self.gone.insert(hash.to_string());
                out.notes.push("A picture from the server was damaged and wasn't used.".to_string());
                self.ask(out);
                return;
            }
        }
        let ids: Vec<SlideId> = self.waiting.iter().filter(|(_, (_, missing))| missing.iter().any(|h| h == hash)).map(|(id, _)| *id).collect();
        for id in ids {
            let Some((data, _)) = self.waiting.get(&id).cloned() else { continue };
            let res = self.decode(id, &data).map_err(|e| format!("{e:?}")).and_then(|p| {
                if Self::local(s, id) {
                    s.collab_apply_slide(id, &p).map_err(|e| e.to_string())
                } else {
                    self.stash(id, p);
                    Ok(())
                }
            });
            if let Err(e) = res {
                out.notes.push(format!("Couldn't show a picture that arrived: {e}"));
            }
        }
        self.ask(out);
    }

    /// The server has no such picture: the stand-in stays.
    fn on_blob_missing(&mut self, m: &Value, out: &mut Out) {
        let Some(hash) = m.get("hash").and_then(Value::as_str).filter(|h| is_hash(h)) else { return };
        if self.asked.remove(hash) && self.gone.insert(hash.to_string()) {
            out.notes.push("A picture in this presentation isn't available from the server.".to_string());
        }
        self.ask(out);
    }

    fn local(s: &Session, id: SlideId) -> bool {
        s.active().is_some_and(|d| d.doc.slides.iter().any(|x| x.id == id))
    }

    /// A message from the parent. Anything that is not a well-formed `collab-*` is ignored.
    pub fn on_message(&mut self, s: &mut Session, m: &Value) -> Out {
        let mut out = Out::default();
        match m.get("type").and_then(Value::as_str).and_then(|t| t.strip_prefix("deckcraft:")) {
            Some("collab-locks") => {
                if let Some(g) = self.link.on_locks(m) {
                    self.released.retain(|id| g.slides.contains(id));
                    s.collab_set_locks(g.slides, g.structure, false);
                }
            }
            Some("collab-role") => {
                if self.link.on_role(m).is_some() {
                    let g = self.link.granted().clone();
                    s.collab_set_locks(g.slides, g.structure, false);
                }
            }
            Some("collab-ack") => {
                self.link.on_ack(m);
            }
            Some("collab-reject") => self.on_reject(s, m, &mut out),
            Some("collab-reset") => self.on_reset(s, m, &mut out),
            Some("collab-apply") => self.on_apply(s, m, &mut out),
            Some("collab-blob") => self.on_blob(s, m, &mut out),
            Some("collab-blob-missing") => self.on_blob_missing(m, &mut out),
            Some("collab-cursors") if self.cursors => out.cursors = Some(remote_cursors::parse(m)),
            _ => {}
        }
        out
    }

    fn on_reset(&mut self, s: &mut Session, m: &Value, out: &mut Out) {
        let mut order = None;
        let mut slides = vec![];
        // The server's whole state replaces what was being waited on; pictures sent earlier may be gone from it.
        self.waiting.clear();
        self.uploaded.clear();
        for (key, kind, ver, data) in regions(m, "regions") {
            self.link.note_ver(&key, ver);
            let decoded = match parse_slide_key(&key) {
                Some(id) if kind == KIND_SLIDE => self.decode(id, &data),
                _ => decode_slide(&data, &mut self.blobs).map(|d| d.presentation),
            };
            match (kind.as_str(), decoded, parse_slide_key(&key)) {
                (KIND_SLIDE, Ok(p), Some(_)) => slides.push(p),
                (KIND_ORDER, _, _) if key == KEY_STRUCTURE => match decode_order(&data) {
                    Ok(o) => order = Some(o),
                    Err(e) => out.notes.push(format!("Couldn't read the slide list from the server: {e:?}")),
                },
                (KIND_SLIDE, Err(e), _) => out.notes.push(format!("Couldn't read {key} from the server: {e:?}")),
                _ => {}
            }
        }
        // No list yet: the room is new, and this presentation is what everyone starts from.
        let Some(order) = order else { return };
        let from: Vec<&Presentation> = slides.iter().collect();
        if let Err(e) = s.collab_reset(&order, &from) {
            out.notes.push(format!("Couldn't bring the presentation up to date: {e}"));
        }
        self.ask(out);
    }

    fn on_apply(&mut self, s: &mut Session, m: &Value, out: &mut Out) {
        for (key, kind, ver, data) in regions(m, "changes") {
            self.link.note_ver(&key, ver);
            let res = match (kind.as_str(), parse_slide_key(&key)) {
                (KIND_SLIDE, Some(id)) => self.decode(id, &data).map_err(|e| format!("{e:?}")).and_then(|p| {
                    if Self::local(s, id) {
                        s.collab_apply_slide(id, &p).map_err(|e| e.to_string())
                    } else {
                        self.stash(id, p);
                        Ok(())
                    }
                }),
                (KIND_ORDER, _) if key == KEY_STRUCTURE => decode_order(&data).map_err(|e| format!("{e:?}")).and_then(|order| {
                    let from: Vec<&Presentation> = self.stash.iter().map(|(_, p)| p).collect();
                    s.collab_apply_structure(&order, &from).map_err(|e| e.to_string())
                }),
                _ => Ok(()),
            };
            if let Err(e) = res {
                out.notes.push(format!("Couldn't apply a change from {key}: {e}"));
            }
        }
        self.ask(out);
    }

    fn on_reject(&mut self, s: &mut Session, m: &Value, out: &mut Out) {
        let Some(r) = self.link.on_reject(m) else { return };
        // Only what the server sent back can be put back; without it the change stays here.
        let server = r.current.as_ref().and_then(|c| c.get("data"));
        let back = if server.is_some() { " and was taken back" } else { "" };
        out.notes.push(match r.reason.as_str() {
            "role" => "You can only view this presentation.".to_string(),
            "not-locked" | "stale" => format!("Someone else is editing there; your change wasn't shared{back}."),
            "limit" => format!("That change is too large to share{back}."),
            _ => format!("A change couldn't be shared{back}."),
        });
        if let Some(id) = parse_slide_key(&r.region) {
            if let Some(Ok(p)) = server.map(|d| self.decode(id, d))
                && let Err(e) = s.collab_apply_slide(id, &p)
            {
                out.notes.push(format!("Couldn't take the change back: {e}"));
            }
        } else if r.region == KEY_STRUCTURE
            && let Some(Ok(order)) = server.map(decode_order)
        {
            let from: Vec<&Presentation> = self.stash.iter().map(|(_, p)| p).collect();
            if let Err(e) = s.collab_apply_structure(&order, &from) {
                out.notes.push(format!("Couldn't take the change back: {e}"));
            }
        }
    }

    /// Once per frame: lock requests for what the user tried to edit, the changes made here, and
    /// the locks of slides the user has moved away from.
    pub fn pump(&mut self, s: &mut Session) -> Out {
        let mut out = Out::default();
        if !s.collab_on() {
            return out;
        }
        for region in s.collab_take_wants() {
            if let Some(m) = self.link.want(&region) {
                out.send.push(m);
            }
        }
        let Shared { link, blobs, uploaded, waiting, .. } = self;
        let (send, notes) = (&mut out.send, &mut out.notes);
        let refused = s.collab_send_outgoing(&mut |doc, item| {
            let (key, kind, data) = match item {
                Outgoing::Slide(id) => (slide_key(*id), KIND_SLIDE, encode_slide(doc, *id, blobs).map(|e| (e.data, e.big))),
                Outgoing::Structure(order) => (KEY_STRUCTURE.to_string(), KIND_ORDER, Ok((encode_order(order), vec![]))),
            };
            match data {
                Ok((data, big)) => {
                    // A picture's bytes reach the server before the slide that names it.
                    for (hash, bytes) in big {
                        if uploaded.insert(hash.clone()) {
                            send.push(serde_json::json!({"type": "deckcraft:collab-blob-out", "hash": hash, "b64": base64_encode(&bytes)}));
                        }
                    }
                    // This editor's own edit now stands for the slide; an arriving picture must not undo it.
                    if let Outgoing::Slide(id) = item {
                        waiting.remove(id);
                    }
                    send.push(link.changed(&key, kind, data));
                    true
                }
                Err(CodecError::TooLarge) => {
                    notes.push("That slide is too large to share (a picture over 16 MB) and was taken back.".to_string());
                    false
                }
                Err(e) => {
                    notes.push(format!("That slide couldn't be shared: {e:?}"));
                    false
                }
            }
        });
        if !refused.is_empty() {
            out.notes.push(format!("{} slide(s) were taken back.", refused.len()));
        }
        if self.cursors {
            out.send.extend(self.place_msg(s));
        }
        for failure in s.collab_take_failed() {
            out.notes.push(format!("Couldn't apply {failure}."));
        }
        // Done with a slide: nothing sent is waiting and the user is on another one.
        if self.link.unacked() == 0 {
            let here = s.active().and_then(|d| d.doc.slides.get(d.selection.slide).map(|x| x.id));
            let g = self.link.granted().clone();
            for id in g.slides.iter().filter(|id| Some(**id) != here && self.released.insert(**id)) {
                out.send.push(Link::release(&slide_key(*id)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deck() -> Session {
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({})).unwrap();
        s.execute("slide.new", &json!({})).unwrap();
        s.collab_enable(true);
        s
    }

    fn go(s: &mut Session, i: usize) {
        s.active_mut().unwrap().selection = Default::default();
        s.active_mut().unwrap().selection.slide = i;
    }

    fn ids(s: &Session) -> Vec<SlideId> {
        s.active().unwrap().doc.slides.iter().map(|x| x.id).collect()
    }

    fn lock(ids: &[SlideId]) -> Value {
        json!({"type": "deckcraft:collab-locks", "locks": ids.iter().map(|i| json!({"region": slide_key(*i), "mine": true, "user": "u", "name": "n", "expiresAt": 1})).collect::<Vec<_>>()})
    }

    #[test]
    fn an_edit_outside_the_locks_asks_for_the_slide_and_an_edit_inside_is_sent() {
        let (mut s, mut sh) = (deck(), Shared::new());
        let id = ids(&s)[1];
        go(&mut s, 1);
        let _ = s.execute("insert.textBox", &json!({}));
        let asked = sh.pump(&mut s);
        assert!(asked.send.iter().any(|m| m["type"] == "deckcraft:collab-want" && m["region"] == slide_key(id)), "{asked:?}");
        sh.on_message(&mut s, &lock(&[id]));
        s.execute("insert.textBox", &json!({})).unwrap();
        s.execute("text.insert", &json!({"text": "hi"})).unwrap();
        s.execute("text.exit", &json!({})).unwrap();
        let sent = sh.pump(&mut s);
        let m = sent.send.iter().find(|m| m["type"] == "deckcraft:collab-changed").expect("a change is sent");
        assert_eq!(m["region"], slide_key(id));
        assert_eq!(m["kind"], KIND_SLIDE);
        assert_eq!(m["data"]["v"], 1);
    }

    #[test]
    fn a_slide_from_another_editor_arrives_and_lands_in_the_list() {
        let (mut a, mut sa) = (deck(), Shared::new());
        let (mut b, mut sb) = (deck(), Shared::new());
        // Same presentation to start with; `a` adds a slide and sends the slide, then the list.
        let first = ids(&a)[0];
        sa.on_message(&mut a, &lock(&[first]));
        go(&mut a, 0);
        let sent_list = a.collab_take_outgoing();
        assert!(sent_list.is_empty());
        let mut c = Session::with_new();
        c.collab_enable(false);
        drop(c);
        // `b` gets a slide it does not have, then the list naming it.
        let new_id = SlideId(900);
        let mut src = a.active().unwrap().doc.as_ref().clone();
        let mut sl = (*src.slides[0]).clone();
        sl.id = new_id;
        src.slides.push(std::sync::Arc::new(sl));
        let mut order = ids(&b);
        order.push(new_id);
        let apply = json!({"type": "deckcraft:collab-apply", "changes": [
            {"key": slide_key(new_id), "ver": 1, "kind": KIND_SLIDE, "data": encode_slide(&src, new_id, &mut BlobStore::default()).unwrap().data, "author": {}},
            {"key": KEY_STRUCTURE, "ver": 1, "kind": KIND_ORDER, "data": encode_order(&order), "author": {}},
        ]});
        let dirty = b.active().unwrap().is_dirty();
        let out = sb.on_message(&mut b, &apply);
        assert!(out.notes.is_empty(), "{out:?}");
        assert_eq!(ids(&b), order);
        assert_eq!(b.active().unwrap().is_dirty(), dirty, "someone else's change is not unsaved work here");
    }

    #[test]
    fn a_viewer_asks_for_nothing_and_a_rejected_slide_goes_back() {
        let (mut s, mut sh) = (deck(), Shared::new());
        sh.on_message(&mut s, &json!({"type": "deckcraft:collab-role", "role": "view"}));
        let _ = s.execute("insert.textBox", &json!({}));
        assert!(sh.pump(&mut s).send.is_empty());
        sh.on_message(&mut s, &json!({"type": "deckcraft:collab-role", "role": "edit"}));
        let id = ids(&s)[0];
        sh.on_message(&mut s, &lock(&[id]));
        let before = s.active().unwrap().doc.clone();
        go(&mut s, 0);
        s.execute("insert.textBox", &json!({})).unwrap();
        let sent = sh.pump(&mut s);
        let m = sent.send.iter().find(|m| m["type"] == "deckcraft:collab-changed").unwrap().clone();
        let out = sh.on_message(&mut s, &json!({"type": "deckcraft:collab-reject", "seq": m["seq"], "region": slide_key(id), "reason": "stale", "current": {"key": slide_key(id), "ver": 2, "kind": KIND_SLIDE, "data": encode_slide(&before, id, &mut BlobStore::default()).unwrap().data, "author": {}}}));
        assert_eq!(out.notes.len(), 1, "{out:?}");
        assert_eq!(*s.active().unwrap().doc.slides[0], *before.slides[0]);
    }

    #[test]
    fn garbage_from_the_parent_is_ignored() {
        let (mut s, mut sh) = (deck(), Shared::new());
        for m in [json!(null), json!({"type": "deckcraft:collab-apply"}), json!({"type": "deckcraft:collab-apply", "changes": [{"key": "slide:1"}, {"key": "slide:1", "kind": "slide-replace", "data": {"v": 9}}]}), json!({"type": "deckcraft:collab-reset", "regions": "x"}), json!({"type": "deckcraft:collab-ack"})] {
            let _ = sh.on_message(&mut s, &m);
        }
        assert_eq!(ids(&s).len(), 3);
    }

    /// Gives slide `i` a picture of `bytes`.
    fn add_picture(s: &mut Session, i: usize, bytes: &[u8]) {
        use deckcraft_engine::model::style::PictureFill;
        use deckcraft_engine::model::{Shape, ShapeKind};
        let doc = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let media = doc.add_media("big.jpg", "image/jpeg", bytes.to_vec()).unwrap();
        let sid = deckcraft_engine::model::ShapeId(doc.alloc_id().unwrap());
        std::sync::Arc::make_mut(&mut doc.slides[i]).shapes.push(Shape { id: sid, kind: ShapeKind::Picture { fill: PictureFill { media, ..Default::default() } }, ..Default::default() });
    }

    fn media_bytes(s: &Session, i: usize) -> Vec<u8> {
        let d = &s.active().unwrap().doc;
        let mut found = None;
        deckcraft_engine::model::walk(&d.slides[i].shapes, &mut |sh, _| {
            if let deckcraft_engine::model::ShapeKind::Picture { fill } = &sh.kind {
                found = Some(fill.media);
            }
        });
        d.media(found.expect("a picture")).unwrap().data.as_ref().clone()
    }

    fn of_type<'a>(out: &'a Out, t: &str) -> Vec<&'a Value> {
        out.send.iter().filter(|m| m["type"] == t).collect()
    }

    /// `a` edits slide 0 with a big picture; returns what it sent.
    fn send_big(bytes: &[u8]) -> (Session, Shared, SlideId, Out) {
        let (mut a, mut sa) = (deck(), Shared::new());
        let id = ids(&a)[0];
        go(&mut a, 0);
        sa.on_message(&mut a, &lock(&[id]));
        add_picture(&mut a, 0, bytes);
        a.execute("text.exit", &json!({})).ok();
        let out = sa.pump(&mut a);
        (a, sa, id, out)
    }

    #[test]
    fn a_big_picture_is_uploaded_before_its_slide_and_nothing_is_too_large_under_16_mb() {
        let bytes = vec![3u8; 300 * 1024];
        let (_a, _sa, id, out) = send_big(&bytes);
        let kinds: Vec<&str> = out.send.iter().map(|m| m["type"].as_str().unwrap()).collect();
        let up = kinds.iter().position(|k| *k == "deckcraft:collab-blob-out").expect("uploaded");
        let ch = kinds.iter().position(|k| *k == "deckcraft:collab-changed").expect("slide sent");
        assert!(up < ch, "{kinds:?}");
        assert!(out.notes.is_empty(), "{out:?}");
        let m = of_type(&out, "deckcraft:collab-blob-out")[0];
        assert_eq!(m["hash"], deckcraft_engine::collab_codec::content_hash(&bytes));
        assert_eq!(base64_decode(m["b64"].as_str().unwrap()).unwrap(), bytes);
        let changed = of_type(&out, "deckcraft:collab-changed")[0];
        assert_eq!(changed["region"], slide_key(id));
        assert!(changed["data"].to_string().len() < 100_000, "the region carries only the name");
    }

    #[test]
    fn a_receiver_draws_a_stand_in_asks_for_the_picture_and_swaps_it_in_when_it_arrives() {
        let bytes = vec![4u8; 300 * 1024];
        let (_a, _sa, id, sent) = send_big(&bytes);
        let changed = of_type(&sent, "deckcraft:collab-changed")[0];
        let (mut b, mut sb) = (deck(), Shared::new());
        let apply = json!({"type": "deckcraft:collab-apply", "changes": [{"key": slide_key(id), "ver": 1, "kind": KIND_SLIDE, "data": changed["data"], "author": {}}]});
        let out = sb.on_message(&mut b, &apply);
        assert!(out.notes.is_empty(), "{out:?}");
        let need = of_type(&out, "deckcraft:collab-need-blob");
        assert_eq!(need.len(), 1);
        let hash = need[0]["hash"].as_str().unwrap().to_string();
        assert!(media_bytes(&b, 0).len() < 512, "a stand-in is drawn meanwhile");
        // Asking again changes nothing: the same hash is asked once.
        assert!(of_type(&sb.on_message(&mut b, &apply), "deckcraft:collab-need-blob").is_empty());
        let blob = json!({"type": "deckcraft:collab-blob", "hash": hash, "b64": base64_encode(&bytes)});
        let out = sb.on_message(&mut b, &blob);
        assert!(out.notes.is_empty(), "{out:?}");
        assert_eq!(media_bytes(&b, 0), bytes);
    }

    #[test]
    fn a_picture_nobody_asked_for_or_with_the_wrong_bytes_is_not_used() {
        let bytes = vec![5u8; 300 * 1024];
        let (_a, _sa, id, sent) = send_big(&bytes);
        let changed = of_type(&sent, "deckcraft:collab-changed")[0];
        let hash = deckcraft_engine::collab_codec::content_hash(&bytes);
        let (mut b, mut sb) = (deck(), Shared::new());
        // Before it was asked for: ignored.
        let out = sb.on_message(&mut b, &json!({"type": "deckcraft:collab-blob", "hash": hash, "b64": base64_encode(&bytes)}));
        assert!(out.send.is_empty() && out.notes.is_empty());
        sb.on_message(&mut b, &json!({"type": "deckcraft:collab-apply", "changes": [{"key": slide_key(id), "ver": 1, "kind": KIND_SLIDE, "data": changed["data"], "author": {}}]}));
        let out = sb.on_message(&mut b, &json!({"type": "deckcraft:collab-blob", "hash": hash, "b64": base64_encode(&[1, 2, 3])}));
        assert_eq!(out.notes.len(), 1, "{out:?}");
        assert!(media_bytes(&b, 0).len() < 512);
        // A damaged answer is not asked for again; the stand-in stays.
        let out = sb.on_message(&mut b, &json!({"type": "deckcraft:collab-blob-missing", "hash": hash}));
        assert!(out.send.is_empty());
    }

    #[test]
    fn a_missing_picture_leaves_the_stand_in_and_says_so_once() {
        let (_a, _sa, id, sent) = send_big(&vec![6u8; 300 * 1024]);
        let changed = of_type(&sent, "deckcraft:collab-changed")[0];
        let (mut b, mut sb) = (deck(), Shared::new());
        let out = sb.on_message(&mut b, &json!({"type": "deckcraft:collab-apply", "changes": [{"key": slide_key(id), "ver": 1, "kind": KIND_SLIDE, "data": changed["data"], "author": {}}]}));
        let hash = of_type(&out, "deckcraft:collab-need-blob")[0]["hash"].clone();
        let out = sb.on_message(&mut b, &json!({"type": "deckcraft:collab-blob-missing", "hash": hash}));
        assert_eq!(out.notes.len(), 1);
        assert!(media_bytes(&b, 0).len() < 512);
        assert!(of_type(&out, "deckcraft:collab-need-blob").is_empty());
    }

    #[test]
    fn editing_a_slide_that_still_has_a_stand_in_names_the_real_picture_and_is_not_overwritten_later() {
        let bytes = vec![8u8; 300 * 1024];
        let (_a, _sa, id, sent) = send_big(&bytes);
        let changed = of_type(&sent, "deckcraft:collab-changed")[0];
        let (mut b, mut sb) = (deck(), Shared::new());
        let out = sb.on_message(&mut b, &json!({"type": "deckcraft:collab-apply", "changes": [{"key": slide_key(id), "ver": 1, "kind": KIND_SLIDE, "data": changed["data"], "author": {}}]}));
        let hash = of_type(&out, "deckcraft:collab-need-blob")[0]["hash"].clone();
        sb.on_message(&mut b, &lock(&[id]));
        go(&mut b, 0);
        b.execute("insert.textBox", &json!({})).unwrap();
        let mine = sb.pump(&mut b);
        assert!(of_type(&mine, "deckcraft:collab-blob-out").is_empty(), "the stand-in's bytes are never uploaded");
        let m = of_type(&mine, "deckcraft:collab-changed")[0];
        assert_eq!(m["data"]["media"][0]["hash"], hash);
        assert!(m["data"]["media"][0].get("b64").is_none());
        let shapes = b.active().unwrap().doc.slides[0].shapes.len();
        // The picture arriving later must not put the slide back to what it was.
        sb.on_message(&mut b, &json!({"type": "deckcraft:collab-blob", "hash": hash, "b64": base64_encode(&bytes)}));
        assert_eq!(b.active().unwrap().doc.slides[0].shapes.len(), shapes);
    }

    fn places(out: &Out) -> Vec<&Value> {
        out.send.iter().filter(|m| m["type"] == "deckcraft:collab-cursor").collect()
    }

    #[test]
    fn the_place_goes_out_when_the_slide_or_selection_changes() {
        let (mut s, mut sh) = (deck(), Shared::new().with_cursors(true));
        go(&mut s, 1);
        let id = s.active().unwrap().doc.slides[1].id;
        let first = sh.pump(&mut s);
        let sent = places(&first);
        assert_eq!(sent.len(), 1, "the first place is sent");
        assert_eq!(sent[0]["region"], format!("slide:{}", id.0));
        assert_eq!(sent[0]["pos"]["shapes"], json!([]));
        assert!(places(&sh.pump(&mut s)).is_empty(), "the same place isn't sent twice");
        go(&mut s, 0);
        let id0 = s.active().unwrap().doc.slides[0].id;
        assert_eq!(places(&sh.pump(&mut s))[0]["region"], format!("slide:{}", id0.0));
    }

    #[test]
    fn the_selected_shapes_ride_along_and_unknown_ones_are_dropped() {
        let (mut s, mut sh) = (deck(), Shared::new().with_cursors(true));
        go(&mut s, 0);
        s.execute("shape.add", &json!({"kind": "rect"})).ok();
        let real = s.active().unwrap().doc.slides[0].shapes.first().map(|x| x.id);
        let d = s.active_mut().unwrap();
        d.selection.shapes = real.into_iter().chain([deckcraft_engine::model::ShapeId(987_654)]).collect();
        let sent = sh.pump(&mut s);
        let got: Vec<u64> = places(&sent)[0]["pos"]["shapes"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).collect();
        assert!(real.is_some(), "the slide has a shape");
        assert_eq!(got.len(), 1, "only the shape the slide has");
        assert!(!got.contains(&987_654));
    }

    #[test]
    fn no_place_is_sent_without_the_option() {
        let (mut s, mut sh) = (deck(), Shared::new());
        go(&mut s, 1);
        assert!(places(&sh.pump(&mut s)).is_empty());
        let m = json!({"type": "deckcraft:collab-cursors", "cursors": [{"conn": 1, "region": "slide:1", "name": "A", "color": "#ff0000", "role": "edit"}]});
        assert!(sh.on_message(&mut s, &m).cursors.is_none(), "the others' places are ignored too");
    }

    #[test]
    fn the_others_places_are_taken_whole() {
        let (mut s, mut sh) = (deck(), Shared::new().with_cursors(true));
        let m = json!({"type": "deckcraft:collab-cursors", "cursors": [
            {"conn": 1, "region": "slide:1", "name": "A", "color": "#ff0000", "role": "edit", "pos": {"shapes": [2]}},
            {"conn": 2, "region": "deck:structure", "name": "B", "color": "#00ff00", "role": "view"}]});
        let got = sh.on_message(&mut s, &m).cursors.expect("a list");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "A");
        let empty = json!({"type": "deckcraft:collab-cursors", "cursors": []});
        assert_eq!(sh.on_message(&mut s, &empty).cursors, Some(vec![]), "an empty list clears the others");
    }
}
