//! Shared editing between the engine and the parent page, as plain JSON in and out (no browser
//! types), so it is tested natively. `host.rs` only moves the JSON through `postMessage`.
//!
//! One [`Shared`] per open presentation. Messages from the parent go through [`Shared::on_message`];
//! once per frame [`Shared::pump`] returns what the editor now wants the parent to hear: lock
//! requests, changed slides and the slide list, locks no longer needed.

use deckcraft_engine::Session;
use deckcraft_engine::collab::Outgoing;
use deckcraft_engine::collab_codec::{CodecError, decode_order, decode_slide, encode_order, encode_slide};
use deckcraft_engine::model::{Presentation, SlideId};
use serde_json::Value;
use std::collections::HashSet;

use crate::collab_wire::{KEY_STRUCTURE, KIND_ORDER, KIND_SLIDE, Link, parse_slide_key, regions, slide_key};

/// Slides that came before the list that names them (a new slide is sent first, then the list).
const MAX_STASH: usize = 64;

/// What one call has to show the user and send to the parent.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    /// Messages for the parent (`deckcraft:collab-*`).
    pub send: Vec<Value>,
    /// Lines for the status bar.
    pub notes: Vec<String>,
}

#[derive(Default)]
pub struct Shared {
    link: Link,
    /// Slides of others not in this presentation yet, oldest first.
    stash: Vec<(SlideId, Presentation)>,
    /// Slides whose lock was given back and the parent has not confirmed yet.
    released: HashSet<SlideId>,
}

impl Shared {
    pub fn new() -> Self {
        Self::default()
    }

    fn stash(&mut self, id: SlideId, p: Presentation) {
        self.stash.retain(|(i, _)| *i != id);
        self.stash.push((id, p));
        if self.stash.len() > MAX_STASH {
            self.stash.remove(0);
        }
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
            _ => {}
        }
        out
    }

    fn on_reset(&mut self, s: &mut Session, m: &Value, out: &mut Out) {
        let mut order = None;
        let mut slides = vec![];
        for (key, kind, ver, data) in regions(m, "regions") {
            self.link.note_ver(&key, ver);
            match (kind.as_str(), decode_slide(&data), parse_slide_key(&key)) {
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
    }

    fn on_apply(&mut self, s: &mut Session, m: &Value, out: &mut Out) {
        for (key, kind, ver, data) in regions(m, "changes") {
            self.link.note_ver(&key, ver);
            let res = match (kind.as_str(), parse_slide_key(&key)) {
                (KIND_SLIDE, Some(id)) => decode_slide(&data).map_err(|e| format!("{e:?}")).and_then(|p| {
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
            if let Some(Ok(p)) = server.map(decode_slide)
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
        let link = &mut self.link;
        let (send, notes) = (&mut out.send, &mut out.notes);
        let refused = s.collab_send_outgoing(&mut |doc, item| {
            let (key, kind, data) = match item {
                Outgoing::Slide(id) => (slide_key(*id), KIND_SLIDE, encode_slide(doc, *id)),
                Outgoing::Structure(order) => (KEY_STRUCTURE.to_string(), KIND_ORDER, Ok(encode_order(order))),
            };
            match data {
                Ok(data) => {
                    send.push(link.changed(&key, kind, data));
                    true
                }
                Err(CodecError::TooLarge) => {
                    notes.push("That slide is too large to share (a picture over 256 KB) and was taken back.".to_string());
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
            {"key": slide_key(new_id), "ver": 1, "kind": KIND_SLIDE, "data": encode_slide(&src, new_id).unwrap(), "author": {}},
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
        let out = sh.on_message(&mut s, &json!({"type": "deckcraft:collab-reject", "seq": m["seq"], "region": slide_key(id), "reason": "stale", "current": {"key": slide_key(id), "ver": 2, "kind": KIND_SLIDE, "data": encode_slide(&before, id).unwrap(), "author": {}}}));
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
}
