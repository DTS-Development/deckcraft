//! The shared-editing messages with the parent page, as plain JSON logic (no browser types) so
//! it is tested natively; `host.rs` only moves the JSON in and out of `postMessage`.
//!
//! Wire contract: `packages/craft-bridge/src/messages.ts` (`collabResetMessage` and friends).
//! Editor to parent are flat, one region each: `collab-changed {seq, region, kind, data, baseVer?}`
//! and `collab-want | collab-release | collab-focus {region}`. Parent to editor:
//! `collab-reset {regions, rev}`, `collab-apply {changes}`, `collab-ack {seq, rev, ver}`,
//! `collab-reject {seq, region, reason, current?}`, `collab-locks {locks: [{region, mine, ..}]}`,
//! `collab-role {role}`. With the open option `cursors` the places of people: editor to parent
//! `collab-cursor {region: "slide:<id>" | null, pos?: {shapes: [id..]}}`, parent to editor
//! `collab-cursors {cursors: [{conn, name, color, role, region, pos?}]}` (see `collab_glue`).

use std::collections::{HashMap, HashSet};

use deckcraft_engine::model::SlideId;
use serde_json::{Value, json};

/// `kind` of a slide region and of the slide list.
pub const KIND_SLIDE: &str = "slide-replace";
pub const KIND_ORDER: &str = "slide-order";
/// Region key of the slide list and of the presentation-wide settings.
pub const KEY_STRUCTURE: &str = "deck:structure";
pub const KEY_DESIGN: &str = "deck:design";
/// Most locks and changes taken from one message (a hostile or broken parent cannot make us loop).
const MAX_ITEMS: usize = 10_000;

/// `slide:<id>` for a slide.
pub fn slide_key(id: SlideId) -> String {
    format!("slide:{}", id.0)
}

/// The slide a region key names, if it is `slide:<number>`.
pub fn parse_slide_key(key: &str) -> Option<SlideId> {
    key.strip_prefix("slide:")?.parse::<u32>().ok().map(SlideId)
}

/// What the parent has granted: the slides held and whether the slide list is. `deck:design` is
/// never taken: nothing sends the presentation-wide settings yet, so editing them would diverge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Granted {
    pub slides: Vec<SlideId>,
    pub structure: bool,
}

/// What a `collab-reject` asks of the editor.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejected {
    pub region: String,
    pub reason: String,
    /// The server's value of the region to put back, if it has one.
    pub current: Option<Value>,
}

/// The state of one open presentation's link to the parent.
#[derive(Debug, Default)]
pub struct Link {
    /// Last number used for a `collab-changed` (it starts at 1 for each open).
    seq: u32,
    /// Region of each change still waiting for its ack.
    pending: HashMap<u32, String>,
    /// The server version of each region as of the last ack or apply.
    vers: HashMap<String, u64>,
    /// Regions already asked for, until they are granted or the parent says otherwise.
    wanted: HashSet<String>,
    granted: Granted,
    /// `view` takes every lock away and nothing is asked for.
    view: bool,
}

fn items<'a>(m: &'a Value, key: &str) -> &'a [Value] {
    m.get(key).and_then(Value::as_array).map_or(&[], |a| &a[..a.len().min(MAX_ITEMS)])
}

impl Link {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// `collab-locks`: the regions that are mine. `None` when the message is not a lock list.
    pub fn on_locks(&mut self, m: &Value) -> Option<Granted> {
        m.get("locks")?.as_array()?;
        let mut g = Granted::default();
        for l in items(m, "locks") {
            if l.get("mine").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let Some(region) = l.get("region").and_then(Value::as_str) else { continue };
            if region == KEY_STRUCTURE {
                g.structure = true;
            } else if let Some(id) = parse_slide_key(region) {
                g.slides.push(id);
            }
            self.wanted.remove(region);
        }
        if self.view {
            g = Granted::default();
        }
        self.granted = g.clone();
        Some(g)
    }

    /// `collab-role`: true for `edit`, false for `view`; `None` for anything else.
    pub fn on_role(&mut self, m: &Value) -> Option<bool> {
        let edit = match m.get("role").and_then(Value::as_str)? {
            "edit" => true,
            "view" => false,
            _ => return None,
        };
        self.view = !edit;
        if !edit {
            self.granted = Granted::default();
            self.wanted.clear();
        }
        Some(edit)
    }

    /// What is granted now (nothing for a viewer).
    pub fn granted(&self) -> &Granted {
        &self.granted
    }

    /// A `collab-want` for `region`, once until it is granted; none for a viewer, the settings
    /// region, or a key the parent would refuse.
    pub fn want(&mut self, region: &str) -> Option<Value> {
        if self.view || region == KEY_DESIGN || (region != KEY_STRUCTURE && parse_slide_key(region).is_none()) || !self.wanted.insert(region.to_string()) {
            return None;
        }
        Some(json!({"type": "deckcraft:collab-want", "region": region}))
    }

    /// `collab-release` for a region this editor has done with.
    pub fn release(region: &str) -> Value {
        json!({"type": "deckcraft:collab-release", "region": region})
    }

    /// `collab-changed` for `region`, numbered and carrying the version it was made on.
    pub fn changed(&mut self, region: &str, kind: &str, data: Value) -> Value {
        self.seq += 1;
        self.pending.insert(self.seq, region.to_string());
        let mut m = json!({"type": "deckcraft:collab-changed", "seq": self.seq, "region": region, "kind": kind, "data": data});
        if let Some(v) = self.vers.get(region) {
            m["baseVer"] = json!(v);
        }
        m
    }

    /// `collab-ack`: the change is the server's; its version is the base of the next one.
    pub fn on_ack(&mut self, m: &Value) -> bool {
        let (Some(seq), Some(ver)) = (m.get("seq").and_then(Value::as_u64), m.get("ver").and_then(Value::as_u64)) else { return false };
        let Some(region) = u32::try_from(seq).ok().and_then(|s| self.pending.remove(&s)) else { return false };
        // A duplicate ack answers with version 0: keep the version we already know.
        if ver > 0 {
            self.vers.insert(region, ver);
        }
        true
    }

    /// `collab-reject`: the change did not go through. Its version is forgotten (the server's
    /// value, if sent, carries the real one).
    pub fn on_reject(&mut self, m: &Value) -> Option<Rejected> {
        let region = m.get("region").and_then(Value::as_str)?.to_string();
        let reason = m.get("reason").and_then(Value::as_str).unwrap_or("invalid").to_string();
        if let Some(seq) = m.get("seq").and_then(Value::as_u64).and_then(|s| u32::try_from(s).ok()) {
            self.pending.remove(&seq);
        }
        let current = m.get("current").filter(|c| c.is_object()).cloned();
        if let Some(ver) = current.as_ref().and_then(|c| c.get("ver")).and_then(Value::as_u64) {
            self.vers.insert(region.clone(), ver);
        }
        // A refused edit may be tried again after asking for the lock.
        self.wanted.remove(&region);
        Some(Rejected { region, reason, current })
    }

    /// The versions of regions that came from others (`collab-apply`, `collab-reset`).
    pub fn note_ver(&mut self, region: &str, ver: u64) {
        self.vers.insert(region.to_string(), ver);
    }

    /// Changes sent and not acknowledged yet.
    pub fn unacked(&self) -> usize {
        self.pending.len()
    }
}

/// `collab-apply`'s changes (or `collab-reset`'s regions) as `(key, kind, ver, data)`.
pub fn regions(m: &Value, list: &str) -> Vec<(String, String, u64, Value)> {
    items(m, list)
        .iter()
        .filter_map(|r| {
            let key = r.get("key")?.as_str()?.to_string();
            let kind = r.get("kind")?.as_str()?.to_string();
            let ver = r.get("ver").and_then(Value::as_u64).unwrap_or(0);
            Some((key, kind, ver, r.get("data").cloned().unwrap_or(Value::Null)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_keys_round_trip_and_only_slides_parse() {
        assert_eq!(slide_key(SlideId(42)), "slide:42");
        assert_eq!(parse_slide_key("slide:42"), Some(SlideId(42)));
        for bad in ["slide:", "slide:x", "slide:-1", "slide:4294967296", "deck:structure", "slide:3/shape:5"] {
            assert_eq!(parse_slide_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn locks_keep_only_mine_and_never_the_settings() {
        let mut l = Link::new();
        let g = l
            .on_locks(&json!({"locks": [
                {"region": "slide:3", "mine": true, "user": "u", "name": "n", "expiresAt": 1},
                {"region": "slide:4", "mine": false},
                {"region": "deck:structure", "mine": true},
                {"region": "deck:design", "mine": true},
                {"region": 5, "mine": true},
            ]}))
            .unwrap();
        assert_eq!(g, Granted { slides: vec![SlideId(3)], structure: true });
        assert!(l.on_locks(&json!({"locks": "x"})).is_none());
        assert!(l.on_locks(&json!({})).is_none());
    }

    #[test]
    fn a_viewer_holds_nothing_and_asks_for_nothing() {
        let mut l = Link::new();
        assert_eq!(l.on_role(&json!({"role": "view"})), Some(false));
        let g = l.on_locks(&json!({"locks": [{"region": "slide:3", "mine": true}]})).unwrap();
        assert_eq!(g, Granted::default());
        assert!(l.want("slide:3").is_none());
        assert_eq!(l.on_role(&json!({"role": "edit"})), Some(true));
        assert!(l.want("slide:3").is_some());
        assert_eq!(l.on_role(&json!({"role": "owner"})), None);
    }

    #[test]
    fn a_lock_is_asked_for_once_until_it_comes_or_the_edit_is_rejected() {
        let mut l = Link::new();
        let w = l.want("slide:3").unwrap();
        assert_eq!(w, json!({"type": "deckcraft:collab-want", "region": "slide:3"}));
        assert!(l.want("slide:3").is_none(), "no repeat while waiting");
        l.on_locks(&json!({"locks": [{"region": "slide:3", "mine": true}]}));
        assert!(l.want("slide:3").is_some(), "asked again once the answer is in");
        assert!(l.want("deck:design").is_none());
        assert!(l.want("slide:x").is_none());
        assert!(l.want("deck:structure").is_some());
    }

    #[test]
    fn changes_are_numbered_from_one_carry_the_version_and_ack_sets_the_next_base() {
        let mut l = Link::new();
        let a = l.changed("slide:3", KIND_SLIDE, json!({"x": 1}));
        assert_eq!(a["seq"], 1);
        assert!(a.get("baseVer").is_none(), "nothing known yet");
        assert_eq!(l.unacked(), 1);
        assert!(l.on_ack(&json!({"seq": 1, "rev": 9, "ver": 9})));
        assert_eq!(l.unacked(), 0);
        let b = l.changed("slide:3", KIND_SLIDE, json!({"x": 2}));
        assert_eq!((b["seq"].clone(), b["baseVer"].clone()), (json!(2), json!(9)));
        // A duplicate answer (version 0) and an ack for nothing change no version.
        assert!(l.on_ack(&json!({"seq": 2, "rev": 9, "ver": 0})));
        assert_eq!(l.changed("slide:3", KIND_SLIDE, Value::Null)["baseVer"], 9);
        assert!(!l.on_ack(&json!({"seq": 77, "rev": 1, "ver": 1})));
        assert!(!l.on_ack(&json!({"seq": "x"})));
        // A remote change moves the base too.
        l.note_ver("slide:3", 12);
        assert_eq!(l.changed("slide:3", KIND_SLIDE, Value::Null)["baseVer"], 12);
    }

    #[test]
    fn a_reject_forgets_the_change_and_hands_back_the_server_value() {
        let mut l = Link::new();
        l.changed("slide:3", KIND_SLIDE, json!(1));
        l.want("slide:3");
        let r = l
            .on_reject(&json!({"seq": 1, "region": "slide:3", "reason": "not-locked", "current": {"key": "slide:3", "ver": 5, "kind": "slide-replace", "data": {}}}))
            .unwrap();
        assert_eq!((r.region.as_str(), r.reason.as_str()), ("slide:3", "not-locked"));
        assert!(r.current.is_some());
        assert_eq!(l.unacked(), 0);
        assert!(l.want("slide:3").is_some(), "the lock can be asked for again");
        assert_eq!(l.changed("slide:3", KIND_SLIDE, Value::Null)["baseVer"], 5);
        assert!(l.on_reject(&json!({"seq": 1})).is_none(), "no region, no reject");
    }

    #[test]
    fn regions_skips_what_is_malformed_and_caps_the_count() {
        let m = json!({"changes": [
            {"key": "slide:1", "kind": "slide-replace", "ver": 3, "data": {"a": 1}},
            {"key": 4, "kind": "x"},
            {"kind": "x"},
            {"key": "deck:structure", "kind": "slide-order"},
        ]});
        let r = regions(&m, "changes");
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].0.as_str(), r[0].2), ("slide:1", 3));
        assert_eq!(r[1].3, Value::Null);
        assert!(regions(&json!({}), "changes").is_empty());
        let big = json!({"changes": (0..MAX_ITEMS + 5).map(|i| json!({"key": format!("slide:{i}"), "kind": "k"})).collect::<Vec<_>>()});
        assert_eq!(regions(&big, "changes").len(), MAX_ITEMS);
    }
}
