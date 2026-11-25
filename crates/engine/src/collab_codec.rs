//! What goes over the wire for a slide region (`slide-replace`) and for the slide list
//! (`slide-order`), as JSON the server never opens.
//!
//! A slide is its own JSON plus the media its shapes use. Each media item carries a content hash
//! (identity and integrity: the receiver checks the bytes against it) and, up to
//! [`MAX_BLOB`], its bytes inline. A slide whose media or total size would not fit a region is
//! refused with [`CodecError::TooLarge`] so it is never half-sent.

use std::sync::Arc;

use deckcraft_model::{MediaId, MediaItem, Presentation, Slide, SlideId};
use serde_json::{Value, json};

use crate::cmd::{base64_decode, base64_encode};

/// Most bytes of one media item sent inline.
pub const MAX_BLOB: usize = 256 * 1024;
/// Most bytes of one region as JSON (the server's default limit is 1 MiB; some room is left for the envelope).
pub const MAX_REGION: usize = 960 * 1024;
/// Most media items one slide may carry.
const MAX_MEDIA: usize = 256;
/// Most slides in a list.
const MAX_SLIDES: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// A media item or the whole region is over its limit; nothing was made.
    TooLarge,
    /// The data is not what a peer would send.
    Bad(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::TooLarge => write!(f, "too-large"),
            CodecError::Bad(m) => write!(f, "bad collab data: {m}"),
        }
    }
}

fn bad(m: impl Into<String>) -> CodecError {
    CodecError::Bad(m.into())
}

/// A 128-bit content hash as 32 hex digits (two FNV-1a runs with different bases, over the bytes
/// and their length). It names the bytes and catches damage; collaborators are signed in, so it is
/// not meant to stand against someone crafting collisions.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut b: u64 = 0x8422_2325_cbf2_9ce4 ^ bytes.len() as u64;
    for &x in bytes {
        a = (a ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3);
        b = (b ^ u64::from(x).rotate_left(3)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{a:016x}{b:016x}")
}

/// The `data` of slide `id`'s region.
pub fn encode_slide(p: &Presentation, id: SlideId) -> Result<Value, CodecError> {
    let slide = p.slides.iter().find(|s| s.id == id).ok_or_else(|| bad(format!("slide {} is not here", id.0)))?;
    let mut ids = vec![];
    crate::cmd::edit::media_of_shapes(&slide.shapes, &mut ids);
    ids.sort_by_key(|m| m.0);
    ids.dedup();
    if ids.len() > MAX_MEDIA {
        return Err(CodecError::TooLarge);
    }
    let mut media = vec![];
    for mid in ids {
        let Some(m) = p.media(mid) else { continue };
        if m.data.len() > MAX_BLOB {
            return Err(CodecError::TooLarge);
        }
        media.push(json!({
            "id": m.id.0, "name": m.name, "mime": m.content_type, "link": m.link,
            "hash": content_hash(&m.data), "size": m.data.len(), "b64": base64_encode(&m.data),
        }));
    }
    let data = json!({"v": 1, "slide": serde_json::to_value(&**slide).map_err(|e| bad(e.to_string()))?, "media": media});
    // The size check is on the JSON text the bridge will carry.
    if data.to_string().len() > MAX_REGION {
        return Err(CodecError::TooLarge);
    }
    Ok(data)
}

/// A presentation holding just the slide of a `slide-replace` region and its media, as the
/// sender had them (ids and all), for [`crate::Session::collab_apply_slide`].
pub fn decode_slide(v: &Value) -> Result<Presentation, CodecError> {
    if v.get("v").and_then(Value::as_u64) != Some(1) {
        return Err(bad("unknown version"));
    }
    let slide: Slide = serde_json::from_value(v.get("slide").cloned().ok_or_else(|| bad("no slide"))?).map_err(|e| bad(e.to_string()))?;
    let list = v.get("media").and_then(Value::as_array).map_or(&[][..], |a| &a[..]);
    if list.len() > MAX_MEDIA {
        return Err(bad("too many media"));
    }
    let mut p = Presentation::default();
    let mut max = slide.id.0;
    deckcraft_model::walk(&slide.shapes, &mut |s, _| max = max.max(s.id.0));
    for m in list {
        let id = m.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad("media id"))?;
        let link = m.get("link").and_then(Value::as_str).map(str::to_string);
        let b64 = m.get("b64").and_then(Value::as_str).unwrap_or("");
        if b64.len() > MAX_BLOB.div_ceil(3) * 4 + 4 {
            return Err(bad("media too large"));
        }
        let data = base64_decode(b64).ok_or_else(|| bad("media base64"))?;
        if m.get("hash").and_then(Value::as_str) != Some(content_hash(&data).as_str()) {
            return Err(bad("media hash does not match its bytes"));
        }
        max = max.max(id);
        p.media.push(MediaItem {
            id: MediaId(id),
            name: m.get("name").and_then(Value::as_str).unwrap_or("media").to_string(),
            content_type: m.get("mime").and_then(Value::as_str).unwrap_or("application/octet-stream").to_string(),
            data: Arc::new(data),
            link,
        });
    }
    p.next_id = max;
    p.slides = vec![Arc::new(slide)];
    Ok(p)
}

/// The `data` of the slide list.
pub fn encode_order(order: &[SlideId]) -> Value {
    json!({"v": 1, "order": order.iter().map(|s| s.0).collect::<Vec<_>>()})
}

pub fn decode_order(v: &Value) -> Result<Vec<SlideId>, CodecError> {
    let list = v.get("order").and_then(Value::as_array).ok_or_else(|| bad("no order"))?;
    if list.len() > MAX_SLIDES {
        return Err(bad("too many slides"));
    }
    list.iter().map(|n| n.as_u64().and_then(|n| u32::try_from(n).ok()).map(SlideId).ok_or_else(|| bad("slide id"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use deckcraft_model::style::PictureFill;
    use deckcraft_model::{Shape, ShapeId, ShapeKind};

    /// Slide 0 of a new deck with a picture of `bytes`.
    fn with_picture(bytes: &[u8]) -> (Session, SlideId) {
        let mut s = Session::with_new();
        let st = s.doc_mut().unwrap();
        let doc = Arc::make_mut(&mut st.doc);
        let media = doc.add_media("p.png", "image/png", bytes.to_vec()).unwrap();
        let sid = ShapeId(doc.alloc_id().unwrap());
        let slide = Arc::make_mut(&mut doc.slides[0]);
        slide.shapes.push(Shape { id: sid, kind: ShapeKind::Picture { fill: PictureFill { media, ..Default::default() } }, ..Default::default() });
        let id = slide.id;
        (s, id)
    }

    #[test]
    fn a_slide_with_a_picture_round_trips_through_the_wire_form() {
        let (s, id) = with_picture(&[7u8; 1000]);
        let data = encode_slide(&s.doc().unwrap().doc, id).unwrap();
        let text = data.to_string();
        let back = decode_slide(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(*back.slides[0], *s.doc().unwrap().doc.slides[0]);
        assert_eq!(back.media.len(), 1);
        assert_eq!(*back.media[0].data, vec![7u8; 1000]);
    }

    #[test]
    fn what_cannot_fit_is_refused_before_anything_is_made() {
        let (s, id) = with_picture(&vec![1u8; MAX_BLOB + 1]);
        assert_eq!(encode_slide(&s.doc().unwrap().doc, id), Err(CodecError::TooLarge));
        // Several allowed pictures can still be too much together.
        let (mut s, id) = with_picture(&vec![2u8; MAX_BLOB]);
        let st = s.doc_mut().unwrap();
        let doc = Arc::make_mut(&mut st.doc);
        for n in 3..8u8 {
            let m = doc.add_media("q.png", "image/png", vec![n; MAX_BLOB]).unwrap();
            let sid = ShapeId(doc.alloc_id().unwrap());
            Arc::make_mut(&mut doc.slides[0]).shapes.push(Shape { id: sid, kind: ShapeKind::Picture { fill: PictureFill { media: m, ..Default::default() } }, ..Default::default() });
        }
        assert_eq!(encode_slide(&s.doc().unwrap().doc, id), Err(CodecError::TooLarge));
        assert!(matches!(encode_slide(&s.doc().unwrap().doc, SlideId(9_999)), Err(CodecError::Bad(_))));
    }

    #[test]
    fn damaged_or_odd_data_is_refused_not_applied() {
        let (s, id) = with_picture(&[5u8; 64]);
        let good = encode_slide(&s.doc().unwrap().doc, id).unwrap();
        let mut tampered = good.clone();
        tampered["media"][0]["hash"] = json!("00000000000000000000000000000000");
        assert!(matches!(decode_slide(&tampered), Err(CodecError::Bad(_))));
        let mut bytes = good.clone();
        bytes["media"][0]["b64"] = json!("not base64!!");
        assert!(matches!(decode_slide(&bytes), Err(CodecError::Bad(_))));
        let mut huge = good.clone();
        huge["media"][0]["b64"] = json!("A".repeat(MAX_BLOB * 2));
        assert!(matches!(decode_slide(&huge), Err(CodecError::Bad(_))));
        let mut v2 = good.clone();
        v2["v"] = json!(2);
        assert!(decode_slide(&v2).is_err());
        assert!(decode_slide(&json!({"v": 1})).is_err());
        assert!(decode_slide(&json!(null)).is_err());
    }

    #[test]
    fn the_slide_list_round_trips_and_refuses_junk() {
        let ids = vec![SlideId(3), SlideId(1), SlideId(2)];
        assert_eq!(decode_order(&encode_order(&ids)).unwrap(), ids);
        for bad in [json!({}), json!({"order": "x"}), json!({"order": [1, -2]}), json!({"order": [1.5]}), json!({"order": [4294967296u64]})] {
            assert!(decode_order(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_hash_tells_different_bytes_apart() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
        assert_ne!(content_hash(b""), content_hash(&[0]));
        assert_eq!(content_hash(b"x").len(), 32);
    }
}
