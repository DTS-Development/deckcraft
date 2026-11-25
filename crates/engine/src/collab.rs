//! Slide locks for shared editing (prototype of the "region lock" gate).
//!
//! The parent page decides who holds which slide ([`Session::collab_set_locks`]); the engine only
//! enforces it. A command (or a pointer edit) is not asked which slide it will touch: it runs, and
//! once the outermost one is done the slides that differ from before are compared with the held
//! set. A change outside it is rolled back and refused with [`EngineError::CollabLocked`]. The
//! check is a pointer comparison per slide, so it costs nothing next to the edit itself.
//!
//! A change to the slide list (add, delete, reorder) needs the `structure` lock, a change to
//! the masters, layouts, theme or other presentation-wide settings needs `design`. Media and the
//! id counter only grow with an edit, so they are never a reason to refuse.
//!
//! Remote slides come in through [`Session::collab_apply_slide`]: no undo step, not dirty, and
//! written into every stored undo and redo state (as pdfcraft's `annot_sync` does) so undoing
//! never brings back an old copy of someone else's slide.
//!
//! This is not `readOnly`: saving and dirty tracking keep working, which the shared-editing
//! leader needs.

use std::collections::HashSet;
use std::sync::Arc;

use deckcraft_model::{Presentation, Slide, SlideId};

use crate::{DocState, EngineError, Result, Session};

/// What the parent has granted this editor on one presentation.
#[derive(Clone, Debug, Default)]
pub struct Gate {
    /// Slides the user may edit.
    pub held: HashSet<SlideId>,
    /// May add, delete and reorder slides.
    pub structure: bool,
    /// May change masters, layouts, theme and presentation-wide settings.
    pub design: bool,
}

/// Why an edit was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    Slide(SlideId),
    Structure,
    Design,
}

impl Violation {
    /// The region key the parent knows (`slide:<id>`, `deck:structure`, `deck:design`).
    pub fn key(&self) -> String {
        match self {
            Violation::Slide(id) => format!("slide:{}", id.0),
            Violation::Structure => "deck:structure".into(),
            Violation::Design => "deck:design".into(),
        }
    }
}

/// The presentation without what an ordinary edit may grow or what has its own lock: the part
/// that `design` guards.
fn design_part(p: &Presentation) -> Presentation {
    let mut d = p.clone();
    d.slides = Vec::new();
    d.media = Vec::new();
    d.next_id = 0;
    d.sections = Vec::new();
    d.custom_shows = Vec::new();
    d
}

/// The first thing `new` changed against `old` that `gate` does not allow.
pub fn violation(old: &Presentation, new: &Presentation, gate: &Gate) -> Option<Violation> {
    let same_list = old.slides.len() == new.slides.len() && old.slides.iter().zip(&new.slides).all(|(a, b)| a.id == b.id);
    if !same_list && !gate.structure {
        return Some(Violation::Structure);
    }
    for (i, b) in new.slides.iter().enumerate() {
        let a = if same_list { old.slides.get(i) } else { old.slides.iter().find(|s| s.id == b.id) };
        let changed = match a {
            Some(a) => !Arc::ptr_eq(a, b) && **a != **b,
            None => true, // a new slide: allowed by `structure` above
        };
        if changed && a.is_some() && !gate.held.contains(&b.id) {
            return Some(Violation::Slide(b.id));
        }
    }
    if !gate.design && design_part(old) != design_part(new) {
        return Some(Violation::Design);
    }
    None
}

/// `p` with the slide of the same id as `slide` replaced; `p` itself when it has none or already
/// has this copy. Media the slide uses is carried over from `media_from`.
fn patched(p: &Arc<Presentation>, slide: &Arc<Slide>, media_from: &Presentation) -> Arc<Presentation> {
    let Some(i) = p.slides.iter().position(|s| s.id == slide.id) else { return p.clone() };
    if Arc::ptr_eq(&p.slides[i], slide) {
        return p.clone();
    }
    let mut d = (**p).clone();
    d.slides[i] = slide.clone();
    for m in &media_from.media {
        if d.media(m.id).is_none() {
            d.media.push(m.clone());
        }
    }
    d.next_id = d.next_id.max(media_from.next_id);
    Arc::new(d)
}

impl Session {
    /// Turns the slide-lock gate on for the active presentation (the parent's `collab` open
    /// option) with nothing held, or off again.
    pub fn collab_enable(&mut self, on: bool) {
        let Some(uid) = self.active().map(|d| d.uid) else { return };
        if on {
            self.collab.entry(uid).or_default();
        } else {
            self.collab.remove(&uid);
        }
    }

    /// Replaces what the user holds. Nothing happens when the gate is off.
    pub fn collab_set_locks(&mut self, held: impl IntoIterator<Item = SlideId>, structure: bool, design: bool) {
        let Some(uid) = self.active().map(|d| d.uid) else { return };
        if let Some(g) = self.collab.get_mut(&uid) {
            *g = Gate { held: held.into_iter().collect(), structure, design };
        }
    }

    /// Is the gate on for the active presentation?
    pub fn collab_on(&self) -> bool {
        self.active().is_some_and(|d| self.collab.contains_key(&d.uid))
    }

    /// Checks the change from `old` to the active document's current content.
    pub(crate) fn collab_check(&self, uid: u64, old: &Presentation, new: &Presentation) -> Option<Violation> {
        self.collab.get(&uid).and_then(|g| violation(old, new, g))
    }

    /// A slide of someone else's, as it should now be here: `slide` out of `from` (the sender's
    /// presentation) replaces the slide with id `id`. Media and shape ids go through
    /// [`crate::compare::adopt`]. Not an edit: no undo step, not dirty, no gate, and every stored
    /// undo and redo state gets the same slide.
    pub fn collab_apply_slide(&mut self, id: SlideId, from: &Presentation) -> Result<()> {
        let src = from
            .slides
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| EngineError::Other(format!("slide {} is not in the sent presentation", id.0)))?;
        let st: &mut DocState = self.doc_mut()?;
        let mut doc = (*st.doc).clone();
        if !doc.slides.iter().any(|s| s.id == id) {
            return Err(EngineError::Other(format!("slide {} is not in this presentation", id.0)));
        }
        let adopted = Arc::new(crate::compare::adopt(&mut doc, from, &src, Some(id))?);
        let was_dirty = st.is_dirty();
        let live = patched(&st.doc, &adopted, &doc);
        st.doc = live;
        for h in st.history.undo.iter_mut().chain(st.history.redo.iter_mut()) {
            h.doc = patched(&h.doc, &adopted, &doc);
        }
        st.saved_doc = if was_dirty { patched(&st.saved_doc, &adopted, &doc) } else { st.doc.clone() };
        st.revision += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Three slides, each with a text box, saved as the baseline.
    fn deck() -> Session {
        let mut s = Session::with_new();
        for _ in 0..2 {
            s.execute("slide.new", &json!({})).unwrap();
        }
        for i in 0..3 {
            s.active_mut().unwrap().selection.slide = i;
            s.execute("insert.textBox", &json!({})).unwrap();
            s.execute("text.insert", &json!({"text": format!("slide {i}")})).unwrap();
            s.execute("text.exit", &json!({})).unwrap();
        }
        let st = s.active_mut().unwrap();
        st.saved_doc = st.doc.clone();
        st.history = Default::default();
        st.history.limit = 500;
        s
    }
    fn ids(s: &Session) -> Vec<SlideId> {
        s.active().unwrap().doc.slides.iter().map(|x| x.id).collect()
    }
    fn go(s: &mut Session, i: usize) {
        s.active_mut().unwrap().selection = Default::default();
        s.active_mut().unwrap().selection.slide = i;
    }
    fn fp(s: &Session) -> (usize, usize, usize) {
        let st = s.active().unwrap();
        (Arc::as_ptr(&st.doc) as usize, Arc::as_ptr(&st.saved_doc) as usize, st.history.undo.len())
    }
    fn text_of(s: &Session, i: usize) -> String {
        format!("{:?}", s.active().unwrap().doc.slides[i].shapes)
    }

    #[test]
    fn an_edit_inside_the_held_slide_goes_through_and_outside_it_is_rolled_back() {
        let mut s = deck();
        s.collab_enable(true);
        let ids = ids(&s);
        s.collab_set_locks([ids[1]], false, false);
        go(&mut s, 1);
        s.execute("insert.textBox", &json!({})).unwrap();
        s.execute("text.insert", &json!({"text": "mine"})).unwrap();
        s.execute("text.exit", &json!({})).unwrap();
        assert!(text_of(&s, 1).contains("mine"));

        go(&mut s, 0);
        let before = fp(&s);
        let r = s.execute("insert.textBox", &json!({}));
        assert!(matches!(&r, Err(EngineError::CollabLocked(k)) if *k == format!("slide:{}", ids[0].0)), "{r:?}");
        assert_eq!(fp(&s), before, "the refused edit left the document, the baseline and the undo stack as they were");
    }

    #[test]
    fn one_command_over_several_slides_needs_all_of_them() {
        let mut s = deck();
        s.collab_enable(true);
        let ids = ids(&s);
        s.collab_set_locks([ids[0]], false, false);
        go(&mut s, 0);
        let before = fp(&s);
        // Theme changes every slide's look, and the masters.
        let themes = s.execute("design.themes", &json!({})).unwrap();
        let name = themes["themes"][1]["name"].as_str().unwrap().to_string();
        let r = s.execute("design.theme", &json!({ "name": name }));
        assert!(matches!(r, Err(EngineError::CollabLocked(_))), "{r:?}");
        assert_eq!(fp(&s), before);
    }

    #[test]
    fn the_slide_list_and_the_design_have_their_own_locks() {
        let mut s = deck();
        s.collab_enable(true);
        let ids = ids(&s);
        s.collab_set_locks(ids.clone(), false, false);
        let before = fp(&s);
        assert!(matches!(s.execute("slide.new", &json!({})), Err(EngineError::CollabLocked(k)) if k == "deck:structure"));
        assert!(matches!(s.execute("design.slideSize", &json!({"preset": "4:3"})), Err(EngineError::CollabLocked(k)) if k == "deck:design"));
        assert_eq!(fp(&s), before);

        s.collab_set_locks(ids.clone(), true, false);
        s.execute("slide.new", &json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.slides.len(), 4);
        s.collab_set_locks(ids, true, true);
        s.execute("design.slideSize", &json!({"preset": "4:3"})).unwrap();
    }

    #[test]
    fn with_nothing_held_everything_that_edits_is_refused_but_looking_and_saving_work() {
        let mut s = deck();
        s.collab_enable(true);
        go(&mut s, 2);
        assert!(s.execute("insert.textBox", &json!({})).is_err());
        // Selecting and moving around is not an edit.
        s.execute("edit.selectAll", &json!({})).unwrap();
        // The leader saves: no `read-only`, and the dirty flag is the honest one.
        let out = s.execute("file.saveBytes", &json!({"format": "pptx"})).unwrap();
        assert!(out["data"].as_str().is_some_and(|d| !d.is_empty()));
        assert!(!s.read_only());
    }

    #[test]
    fn the_pointer_path_edit_is_gated_too() {
        let mut s = deck();
        s.collab_enable(true);
        let ids = ids(&s);
        s.collab_set_locks([ids[0]], false, false);
        go(&mut s, 1);
        let before = fp(&s);
        let r = s.edit(|d, _| {
            let mut sl = (*d.slides[1]).clone();
            sl.name = "dragged".into();
            d.slides[1] = Arc::new(sl);
            Ok(())
        });
        assert!(matches!(r, Err(EngineError::CollabLocked(_))));
        assert_eq!(fp(&s), before);
        go(&mut s, 0);
        s.edit(|d, _| {
            let mut sl = (*d.slides[0]).clone();
            sl.name = "dragged".into();
            d.slides[0] = Arc::new(sl);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn the_gate_is_off_until_asked_and_can_be_turned_off() {
        let mut s = deck();
        assert!(!s.collab_on());
        s.execute("insert.textBox", &json!({})).unwrap();
        s.collab_enable(true);
        assert!(s.execute("insert.textBox", &json!({})).is_err());
        s.collab_enable(false);
        s.execute("insert.textBox", &json!({})).unwrap();
    }

    #[test]
    fn a_remote_slide_replaces_ours_without_undo_or_dirty_and_undo_never_brings_the_old_one_back() {
        let mut a = deck();
        // `b` is what the other user has: the same slides (same ids), slide 2 edited.
        let mut b = Session::new();
        b.add_document(DocState::new((*a.active().unwrap().doc).clone(), None, "b".into()));
        let ids = ids(&a);
        go(&mut b, 2);
        b.execute("insert.textBox", &json!({})).unwrap();
        b.execute("text.insert", &json!({"text": "from b"})).unwrap();
        b.execute("text.exit", &json!({})).unwrap();

        // `a` edits slide 0 (undoable), then receives slide 2.
        go(&mut a, 0);
        a.execute("insert.textBox", &json!({})).unwrap();
        a.execute("text.insert", &json!({"text": "from a"})).unwrap();
        a.execute("text.exit", &json!({})).unwrap();
        let undo_before = a.active().unwrap().history.undo.len();
        let from = b.active().unwrap().doc.clone();
        a.collab_apply_slide(ids[2], &from).unwrap();
        assert!(text_of(&a, 2).contains("from b"));
        assert_eq!(a.active().unwrap().history.undo.len(), undo_before, "no undo step for a remote slide");
        assert!(a.active().unwrap().is_dirty(), "a's own edit is still unsaved");

        // Undo all of a's own edits: slide 2 stays b's.
        while a.active().unwrap().history.undo.last().is_some() {
            a.execute("edit.undo", &json!({})).unwrap();
        }
        assert!(text_of(&a, 2).contains("from b"));
        assert!(!text_of(&a, 0).contains("from a"));
    }

    #[test]
    fn a_remote_slide_does_not_make_a_clean_document_dirty() {
        let mut a = deck();
        let mut b = Session::new();
        b.add_document(DocState::new((*a.active().unwrap().doc).clone(), None, "b".into()));
        go(&mut b, 1);
        b.execute("insert.textBox", &json!({})).unwrap();
        let id = ids(&a)[1];
        let from = b.active().unwrap().doc.clone();
        assert!(!a.active().unwrap().is_dirty());
        a.collab_apply_slide(id, &from).unwrap();
        assert!(!a.active().unwrap().is_dirty());
        assert!(a.collab_apply_slide(SlideId(999_999), &from).is_err());
    }
}

#[cfg(test)]
mod cost {
    use super::*;
    use serde_json::json;

    /// What the gate adds to one edit on a big deck (prints the figure; asserts only a loose bound).
    #[test]
    fn the_check_is_cheap_on_a_big_deck() {
        let mut s = Session::with_new();
        for _ in 0..299 {
            s.execute("slide.new", &json!({})).unwrap();
        }
        s.collab_enable(true);
        let ids: Vec<_> = s.active().unwrap().doc.slides.iter().map(|x| x.id).collect();
        let old = s.active().unwrap().doc.clone();
        let mut new = (*old).clone();
        let mut sl = (*new.slides[7]).clone();
        sl.name = "x".into();
        new.slides[7] = Arc::new(sl);
        let gate = Gate { held: [ids[7]].into_iter().collect(), structure: false, design: false };
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            assert!(violation(&old, &new, &gate).is_none());
        }
        let per = t.elapsed() / 1000;
        eprintln!("collab check on 300 slides: {per:?} per edit");
        assert!(per < std::time::Duration::from_millis(5));
    }
}
