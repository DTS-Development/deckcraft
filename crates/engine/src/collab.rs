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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use deckcraft_model::{Presentation, Slide, SlideId};

use crate::{clamp_selection, DocState, EngineError, Result, Session};

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

/// A remote change that is waiting for an input method's composition to end.
#[derive(Clone, Debug)]
pub enum Remote {
    /// Slide `id` as it is in the sender's presentation.
    Slide { id: SlideId, from: Arc<Presentation> },
    /// The slide list: ids in order; the ones missing here come out of the sender's presentation.
    Structure { order: Vec<SlideId>, from: Arc<Presentation> },
}

/// `p` with its slides in `order`: a slide it has keeps its own copy, one it lacks is taken from
/// `fresh`, one not in `order` is dropped. Sections and custom shows lose the dropped ids and
/// put a new slide with the slide before it. `p` itself when nothing changes.
fn restructured(p: &Arc<Presentation>, order: &[SlideId], fresh: &HashMap<SlideId, Arc<Slide>>, media_from: &Presentation) -> Arc<Presentation> {
    let slides: Vec<Arc<Slide>> = order.iter().filter_map(|id| p.slides.iter().find(|s| s.id == *id).or_else(|| fresh.get(id))).cloned().collect();
    let same = slides.len() == p.slides.len() && slides.iter().zip(&p.slides).all(|(a, b)| Arc::ptr_eq(a, b));
    if same {
        return p.clone();
    }
    let mut d = (**p).clone();
    let gone: HashSet<SlideId> = p.slides.iter().map(|s| s.id).filter(|id| !order.contains(id)).collect();
    for sec in &mut d.sections {
        sec.slides.retain(|id| !gone.contains(id));
    }
    for show in &mut d.custom_shows {
        show.slides.retain(|id| !gone.contains(id));
    }
    if !d.sections.is_empty() {
        for (i, sl) in slides.iter().enumerate() {
            if p.slides.iter().any(|s| s.id == sl.id) {
                continue;
            }
            // The section of the slide before it (the first one for a new first slide).
            let at = i.checked_sub(1).and_then(|j| d.sections.iter().position(|sec| sec.slides.contains(&slides[j].id))).unwrap_or(0);
            if let Some(sec) = d.sections.get_mut(at) {
                sec.slides.push(sl.id);
            }
        }
    }
    d.slides = slides;
    for m in &media_from.media {
        if d.media(m.id).is_none() {
            d.media.push(m.clone());
        }
    }
    d.next_id = d.next_id.max(media_from.next_id);
    Arc::new(d)
}

/// Writes `f` over every copy of the presentation a document keeps: the live one, each undo and
/// redo state, and what an input method's composition or a pointer drag in progress will go back to.
fn map_states(st: &mut DocState, f: &dyn Fn(&Arc<Presentation>) -> Arc<Presentation>) {
    st.doc = f(&st.doc);
    for h in st.history.undo.iter_mut().chain(st.history.redo.iter_mut()) {
        h.doc = f(&h.doc);
    }
    if let Some(c) = st.composition.as_mut() {
        c.before = f(&c.before);
    }
    if let Some(i) = st.interaction.as_mut() {
        i.doc = f(&i.doc);
    }
}

/// What this editor has changed since the last call, to be sent to the others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// Slide `id` is new or its content changed.
    Slide(SlideId),
    /// The slide list (add, delete, reorder): ids in order.
    Structure(Vec<SlideId>),
}

impl Session {
    /// Turns the slide-lock gate on for the active presentation (the parent's `collab` open
    /// option) with nothing held, or off again.
    pub fn collab_enable(&mut self, on: bool) {
        let Some(uid) = self.active().map(|d| d.uid) else { return };
        if on {
            self.collab.entry(uid).or_default();
            if let Some(d) = self.active() {
                let base = d.doc.clone();
                self.collab_base.insert(uid, base);
            }
        } else {
            self.collab.remove(&uid);
            self.collab_held.remove(&uid);
            self.collab_base.remove(&uid);
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

    /// The id of the slide an input method is composing on, if any.
    fn composing_slide(&self) -> Option<SlideId> {
        let st = self.active()?;
        st.composition.as_ref()?;
        st.doc.slides.get(st.selection.slide).map(|s| s.id)
    }

    /// Whether `r` has to wait: something is waiting already (order is kept), or it touches the
    /// slide being composed on (a replace of it, or a slide list without it).
    fn collab_must_wait(&self, uid: u64, r: &Remote) -> bool {
        if self.collab_held.get(&uid).is_some_and(|q| !q.is_empty()) {
            return true;
        }
        let Some(c) = self.composing_slide() else { return false };
        match r {
            Remote::Slide { id, .. } => *id == c,
            Remote::Structure { order, .. } => !order.contains(&c),
        }
    }

    /// Takes in a change of someone else's, now or (when it touches the slide an input method is
    /// composing on) once the composition ends. Not an edit: no undo step, not dirty, no gate.
    pub fn collab_apply(&mut self, r: Remote) -> Result<()> {
        let uid = self.active().map(|d| d.uid).ok_or_else(|| EngineError::Other("no presentation".into()))?;
        if self.collab_must_wait(uid, &r) {
            self.collab_held.entry(uid).or_default().push(r);
            return Ok(());
        }
        self.collab_apply_now(r)
    }

    /// The changes made here since the last call (or since the gate went on): changed or new
    /// slides first, then the list when it differs, so a receiver has a slide before the list names
    /// it. Remote changes applied meanwhile are not among them. The gate keeps these inside what
    /// the user holds. Empty when the gate is off.
    pub fn collab_take_outgoing(&mut self) -> Vec<Outgoing> {
        let Some(st) = self.active() else { return vec![] };
        let uid = st.uid;
        let Some(base) = self.collab_base.get(&uid) else { return vec![] };
        if Arc::ptr_eq(base, &st.doc) {
            return vec![];
        }
        let mut out = vec![];
        for s in &st.doc.slides {
            let changed = match base.slides.iter().find(|b| b.id == s.id) {
                Some(b) => !Arc::ptr_eq(b, s) && **b != **s,
                None => true,
            };
            if changed {
                out.push(Outgoing::Slide(s.id));
            }
        }
        let same_list = base.slides.len() == st.doc.slides.len() && base.slides.iter().zip(&st.doc.slides).all(|(a, b)| a.id == b.id);
        if !same_list {
            out.push(Outgoing::Structure(st.doc.slides.iter().map(|s| s.id).collect()));
        }
        let now = st.doc.clone();
        self.collab_base.insert(uid, now);
        out
    }

    /// How many remote changes are waiting for a composition to end.
    pub fn collab_held_len(&self) -> usize {
        self.active().and_then(|d| self.collab_held.get(&d.uid)).map_or(0, Vec::len)
    }

    /// Applies what waited, in order, once no composition is in progress. Changes that cannot be
    /// applied are listed in [`Session::collab_take_failed`] rather than lost silently.
    pub fn collab_flush(&mut self) {
        if self.collab_held.is_empty() || self.composing_slide().is_some() {
            return;
        }
        let Some(uid) = self.active().map(|d| d.uid) else { return };
        for r in self.collab_held.remove(&uid).unwrap_or_default() {
            if let Err(e) = self.collab_apply_now(r) {
                self.collab_failed.push(e.to_string());
            }
        }
    }

    /// Remote changes that failed after waiting; the parent should ask the server for a reset.
    pub fn collab_take_failed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.collab_failed)
    }

    fn collab_apply_now(&mut self, r: Remote) -> Result<()> {
        match r {
            Remote::Slide { id, from } => self.apply_slide_now(id, &from),
            Remote::Structure { order, from } => self.apply_structure_now(&order, &from),
        }
    }

    /// A slide of someone else's, as it should now be here: `slide` out of `from` (the sender's
    /// presentation) replaces the slide with id `id`. Media and shape ids go through
    /// [`crate::compare::adopt`]. Not an edit: no undo step, not dirty, no gate, and every stored
    /// undo and redo state gets the same slide. Waits while that slide is being composed on.
    pub fn collab_apply_slide(&mut self, id: SlideId, from: &Presentation) -> Result<()> {
        self.collab_apply(Remote::Slide { id, from: Arc::new(from.clone()) })
    }

    /// The slide list as someone else left it (`deck:structure`): slides added, removed and
    /// reordered to match `order`; the ones not here yet are taken out of `from`. Same rules as
    /// [`Session::collab_apply_slide`]; the selection stays on the same slide.
    pub fn collab_apply_structure(&mut self, order: &[SlideId], from: &Presentation) -> Result<()> {
        self.collab_apply(Remote::Structure { order: order.to_vec(), from: Arc::new(from.clone()) })
    }

    /// The server's whole state (`collab-reset`) laid over this editor without undoing what it
    /// has sent and not had acknowledged: those edits are all inside what the user holds, so the
    /// slides held (and the slide list, when `structure` is held) are left as they are and the rest
    /// is written as in [`Session::collab_apply_slide`] and [`Session::collab_apply_structure`].
    pub fn collab_reset(&mut self, order: &[SlideId], from: &Presentation) -> Result<()> {
        let gate = self.active().and_then(|d| self.collab.get(&d.uid)).cloned().unwrap_or_default();
        // With the list held, slides this editor added are not in `order` yet: keep the list.
        if !gate.structure {
            self.collab_apply_structure(order, from)?;
        }
        let local: HashSet<SlideId> = self.active().map(|d| d.doc.slides.iter().map(|s| s.id).collect()).unwrap_or_default();
        // A slide that arrives with the list is already whole; only the ones here need a write.
        for id in order.iter().filter(|id| local.contains(id) && !gate.held.contains(id) && from.slides.iter().any(|s| s.id == **id)) {
            self.collab_apply_slide(*id, from)?;
        }
        Ok(())
    }

    fn apply_slide_now(&mut self, id: SlideId, from: &Presentation) -> Result<()> {
        let src = from
            .slides
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| EngineError::Other(format!("slide {} is not in the sent presentation", id.0)))?;
        let st: &mut DocState = self.doc_mut()?;
        let mut doc = (*st.doc).clone();
        let Some(here) = doc.slides.iter().find(|s| s.id == id) else {
            return Err(EngineError::Other(format!("slide {} is not in this presentation", id.0)));
        };
        // Already the same: nothing to write (a reset sends every slide).
        if Arc::ptr_eq(here, &src) || **here == *src {
            return Ok(());
        }
        let adopted = Arc::new(crate::compare::adopt(&mut doc, from, &src, Some(id))?);
        let was_dirty = st.is_dirty();
        map_states(st, &|p| patched(p, &adopted, &doc));
        st.saved_doc = if was_dirty { patched(&st.saved_doc, &adopted, &doc) } else { st.doc.clone() };
        st.revision += 1;
        let uid = st.uid;
        if let Some(b) = self.collab_base.get_mut(&uid) {
            *b = patched(b, &adopted, &doc);
        }
        Ok(())
    }

    fn apply_structure_now(&mut self, order: &[SlideId], from: &Presentation) -> Result<()> {
        let st: &mut DocState = self.doc_mut()?;
        let mut seen = HashSet::new();
        if order.is_empty() || !order.iter().all(|id| seen.insert(*id)) {
            return Err(EngineError::Other("the slide list is empty or repeats a slide".into()));
        }
        let mut doc = (*st.doc).clone();
        // Slides this document does not have yet.
        let mut fresh: HashMap<SlideId, Arc<Slide>> = HashMap::new();
        let missing: Vec<SlideId> = order.iter().copied().filter(|id| !doc.slides.iter().any(|s| s.id == *id)).collect();
        for id in &missing {
            let src = from
                .slides
                .iter()
                .find(|s| s.id == *id)
                .ok_or_else(|| EngineError::Other(format!("slide {} is not in the sent presentation", id.0)))?;
            // Keep the id: it is the region key the other side knows it by.
            let mut adopted = crate::compare::adopt(&mut doc, from, src, Some(*id))?;
            adopted.id = *id;
            fresh.insert(*id, Arc::new(adopted));
        }
        let was_dirty = st.is_dirty();
        let current = st.doc.slides.get(st.selection.slide).map(|s| s.id);
        map_states(st, &|p| restructured(p, order, &fresh, &doc));
        st.saved_doc = if was_dirty { restructured(&st.saved_doc, order, &fresh, &doc) } else { st.doc.clone() };
        // The selection stays on its slide, or on the nearest index when that slide is gone.
        if let Some(i) = current.and_then(|id| st.doc.slides.iter().position(|s| s.id == id)) {
            st.selection.slide = i;
        }
        clamp_selection(st);
        st.revision += 1;
        let uid = st.uid;
        if let Some(b) = self.collab_base.get_mut(&uid) {
            *b = restructured(b, order, &fresh, &doc);
        }
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

    /// `a`'s deck copied to a second session `b` (same ids).
    fn peer(a: &Session) -> Session {
        let mut b = Session::new();
        b.add_document(DocState::new((*a.active().unwrap().doc).clone(), None, "b".into()));
        b
    }

    #[test]
    fn a_remote_slide_list_adds_deletes_and_reorders_without_undo_or_dirty() {
        let mut a = deck();
        let mut b = peer(&a);
        let old = ids(&a);
        // b adds a slide, moves it first, deletes the old middle one.
        go(&mut b, 2);
        b.execute("slide.new", &json!({})).unwrap();
        b.execute("insert.textBox", &json!({})).unwrap();
        b.execute("text.insert", &json!({"text": "new from b"})).unwrap();
        b.execute("text.exit", &json!({})).unwrap();
        go(&mut b, 1);
        b.execute("slide.delete", &json!({})).unwrap();
        let order = ids(&b);
        assert_eq!(order.len(), 3);
        assert!(!order.contains(&old[1]));

        go(&mut a, 2);
        let undo0 = a.active().unwrap().history.undo.len();
        a.collab_apply_structure(&order, &b.active().unwrap().doc).unwrap();
        assert_eq!(ids(&a), order);
        assert!(text_of(&a, 2).contains("new from b"));
        assert_eq!(a.active().unwrap().history.undo.len(), undo0, "no undo step for a remote list");
        assert!(!a.active().unwrap().is_dirty());
        assert_eq!(a.active().unwrap().doc.slides[a.active().unwrap().selection.slide].id, old[2], "the selection stays on its slide");
        // Undoing our own edits afterwards never brings a deleted slide back or drops the new one.
        go(&mut a, 0);
        a.execute("insert.textBox", &json!({})).unwrap();
        a.execute("text.exit", &json!({})).unwrap();
        while a.active().unwrap().history.undo.last().is_some() {
            a.execute("edit.undo", &json!({})).unwrap();
        }
        assert_eq!(ids(&a), order);
    }

    #[test]
    fn a_remote_list_missing_a_slide_it_cannot_supply_or_repeating_one_is_refused() {
        let mut a = deck();
        let b = peer(&a);
        let mut order = ids(&a);
        order.push(SlideId(987_654));
        assert!(a.collab_apply_structure(&order, &b.active().unwrap().doc).is_err());
        let o = ids(&a);
        assert!(a.collab_apply_structure(&[o[0], o[0]], &b.active().unwrap().doc).is_err());
        assert!(a.collab_apply_structure(&[], &b.active().unwrap().doc).is_err());
        assert_eq!(ids(&a), o, "a refused list changes nothing");
    }

    /// Starts an input method's composition on slide `i`.
    fn compose_on(s: &mut Session, i: usize) {
        go(s, i);
        s.execute("insert.textBox", &json!({})).unwrap();
        s.execute("text.compose", &json!({"text": "한"})).unwrap();
        assert!(s.active().unwrap().composition.is_some());
    }

    #[test]
    fn a_remote_change_to_the_slide_being_composed_on_waits_for_the_composition_to_end() {
        let mut a = deck();
        let mut b = peer(&a);
        let old = ids(&a);
        for i in [0, 1] {
            go(&mut b, i);
            b.execute("insert.textBox", &json!({})).unwrap();
            b.execute("text.insert", &json!({"text": format!("b{i}")})).unwrap();
            b.execute("text.exit", &json!({})).unwrap();
        }
        let from = b.active().unwrap().doc.clone();
        compose_on(&mut a, 0);
        let composing = text_of(&a, 0);
        let undo0 = a.active().unwrap().history.undo.len();

        // Another slide goes straight in; the composed one waits, and so does what comes after it.
        a.collab_apply_slide(old[2], &from).unwrap();
        assert_eq!(a.collab_held_len(), 0);
        a.collab_apply_slide(old[0], &from).unwrap();
        assert_eq!(a.collab_held_len(), 1);
        assert_eq!(text_of(&a, 0), composing, "the composed slide is untouched meanwhile");
        a.collab_apply_slide(old[1], &from).unwrap();
        assert_eq!(a.collab_held_len(), 2, "order is kept: later changes queue behind the held one");
        assert!(!text_of(&a, 1).contains("b1"));

        a.execute("text.commit", &json!({"text": "한글"})).unwrap();
        assert_eq!(a.collab_held_len(), 0);
        assert!(text_of(&a, 0).contains("b0") && text_of(&a, 1).contains("b1"), "both went in at the end");
        assert!(a.collab_take_failed().is_empty());
        assert_eq!(a.active().unwrap().history.undo.len(), undo0 + 1, "the composition is still one undo step and the remote ones add none");
    }

    #[test]
    fn a_cancelled_composition_lets_the_waiting_change_in_and_a_list_without_the_slide_waits() {
        let mut a = deck();
        let mut b = peer(&a);
        let old = ids(&a);
        go(&mut b, 0);
        b.execute("slide.delete", &json!({})).unwrap();
        let (order, from) = (ids(&b), b.active().unwrap().doc.clone());
        compose_on(&mut a, 0);
        a.collab_apply_structure(&order, &from).unwrap();
        assert_eq!(a.collab_held_len(), 1, "the composed slide is being deleted: wait");
        assert_eq!(ids(&a), old);
        // An empty preedit cancels; that path never calls end_composition.
        a.execute("text.compose", &json!({"text": ""})).unwrap();
        assert_eq!(a.collab_held_len(), 0);
        assert_eq!(ids(&a), order);
        assert!(a.collab_take_failed().is_empty());
    }

    #[test]
    fn a_reset_leaves_the_held_slides_and_a_held_list_alone() {
        let mut a = deck();
        let mut b = peer(&a);
        let old = ids(&a);
        // a holds slide 1 and has an edit there the server has not acknowledged.
        a.collab_enable(true);
        a.collab_set_locks([old[1]], false, false);
        go(&mut a, 1);
        a.execute("insert.textBox", &json!({})).unwrap();
        a.execute("text.insert", &json!({"text": "unacked"})).unwrap();
        a.execute("text.exit", &json!({})).unwrap();
        // The server's state: slides 0 and 2 changed by others, slide 1 as it was.
        for i in [0, 2] {
            go(&mut b, i);
            b.execute("insert.textBox", &json!({})).unwrap();
            b.execute("text.insert", &json!({"text": format!("server{i}")})).unwrap();
            b.execute("text.exit", &json!({})).unwrap();
        }
        let undo0 = a.active().unwrap().history.undo.len();
        a.collab_reset(&old, &b.active().unwrap().doc).unwrap();
        assert!(text_of(&a, 1).contains("unacked"), "an unacknowledged edit survives a reset");
        assert!(text_of(&a, 0).contains("server0") && text_of(&a, 2).contains("server2"));
        assert_eq!(a.active().unwrap().history.undo.len(), undo0);

        // With the list held, a slide of ours that the server has not seen stays.
        a.collab_set_locks([old[1]], true, false);
        go(&mut a, 1);
        a.execute("slide.new", &json!({})).unwrap();
        let mine = ids(&a);
        assert_eq!(mine.len(), 4);
        a.collab_reset(&old, &b.active().unwrap().doc).unwrap();
        assert_eq!(ids(&a), mine);
    }

    #[test]
    fn what_is_sent_is_what_changed_here_and_never_what_came_from_others() {
        let mut a = deck();
        a.collab_enable(true);
        let old = ids(&a);
        a.collab_set_locks([old[1]], true, false);
        assert!(a.collab_take_outgoing().is_empty());
        go(&mut a, 1);
        a.execute("insert.textBox", &json!({})).unwrap();
        assert_eq!(a.collab_take_outgoing(), vec![Outgoing::Slide(old[1])]);
        assert!(a.collab_take_outgoing().is_empty(), "sent once");

        // A remote slide and a remote list change are not echoed back.
        let mut b = peer(&a);
        go(&mut b, 2);
        b.execute("insert.textBox", &json!({})).unwrap();
        a.collab_apply_slide(old[2], &b.active().unwrap().doc).unwrap();
        assert!(a.collab_take_outgoing().is_empty());
        go(&mut b, 0);
        b.execute("slide.delete", &json!({})).unwrap();
        a.collab_apply_structure(&ids(&b), &b.active().unwrap().doc).unwrap();
        assert!(a.collab_take_outgoing().is_empty());

        // A new slide: the slide first, then the list.
        go(&mut a, 0);
        a.execute("slide.new", &json!({})).unwrap();
        let now = ids(&a);
        let new = *now.iter().find(|id| !ids(&b).contains(id)).unwrap();
        assert_eq!(a.collab_take_outgoing(), vec![Outgoing::Slide(new), Outgoing::Structure(now)]);
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
