//! What goes over the wire for a slide region (`slide-replace`) and for the slide list
//! (`slide-order`), as JSON the server never opens.
//!
//! A slide is its own JSON plus the media its shapes use. Each media item carries the SHA-256 of
//! its bytes (identity and integrity: the receiver checks the bytes against it). Up to
//! [`MAX_BLOB`] the bytes ride inline; a bigger picture (up to [`MAX_BIG`]) is named by its hash
//! only and its bytes go over the blob channel (`blob-put` / `blob-get`). A receiver that does not
//! have the bytes yet draws a stand-in picture ([`is_stand_in`]) and asks for them. A slide whose
//! media or JSON would not fit is refused with [`CodecError::TooLarge`] so it is never half-sent.

use std::sync::Arc;

use deckcraft_model::{MediaId, MediaItem, Presentation, Slide, SlideId};
use serde_json::{Value, json};

use crate::cmd::{base64_decode, base64_encode};

/// Most bytes of one media item sent inline.
pub const MAX_BLOB: usize = 256 * 1024;
/// Most bytes of one media item over the blob channel (the server's default `maxBlobBytes`).
pub const MAX_BIG: usize = 16 * 1024 * 1024;
/// Most bytes of one region as JSON (the server's default limit is 1 MiB; some room is left for the envelope).
pub const MAX_REGION: usize = 960 * 1024;
/// Most media items one slide may carry.
const MAX_MEDIA: usize = 256;
/// Most slides in a list.
const MAX_SLIDES: usize = 10_000;
/// Most bytes (and items) the [`BlobStore`] keeps for answering hashes it has seen.
const STORE_BYTES: usize = 96 * 1024 * 1024;
const STORE_ITEMS: usize = 256;

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

/// SHA-256 of `data`.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    // Message, one 0x80, zeros, then the bit length as 64 bits: a whole number of 64-byte blocks.
    let mut tail = data[data.len() - data.len() % 64..].to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    let mut block = |b: &[u8]| {
        let mut w = [0u32; 64];
        for (i, x) in w.iter_mut().enumerate().take(16) {
            *x = u32::from_be_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    };
    for b in data[..data.len() - data.len() % 64].chunks(64) {
        block(b);
    }
    for b in tail.chunks(64) {
        block(b);
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// The SHA-256 of `bytes` as 64 lowercase hex digits: the name of a media blob on the wire.
pub fn content_hash(bytes: &[u8]) -> String {
    sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `h` has the shape of [`content_hash`].
pub fn is_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Media bytes this editor has seen, by hash, so a hash named by a peer can be answered without
/// asking again and an own picture is hashed once. Bounded; the oldest go first.
#[derive(Default)]
pub struct BlobStore {
    items: Vec<(String, Arc<Vec<u8>>)>,
    bytes: usize,
}

impl BlobStore {
    /// The hash of `data`, computed once per allocation.
    pub fn hash_of(&mut self, data: &Arc<Vec<u8>>) -> String {
        if let Some((h, _)) = self.items.iter().find(|(_, a)| Arc::ptr_eq(a, data)) {
            return h.clone();
        }
        let h = content_hash(data);
        self.put(h.clone(), data.clone());
        h
    }

    pub fn get(&self, hash: &str) -> Option<Arc<Vec<u8>>> {
        self.items.iter().find(|(h, _)| h == hash).map(|(_, a)| a.clone())
    }

    /// Remembers `data` under `hash` (the caller has checked they belong together).
    pub fn put(&mut self, hash: String, data: Arc<Vec<u8>>) {
        if self.items.iter().any(|(h, _)| *h == hash) {
            return;
        }
        self.bytes += data.len();
        self.items.push((hash, data));
        while self.items.len() > 1 && (self.bytes > STORE_BYTES || self.items.len() > STORE_ITEMS) {
            let (_, gone) = self.items.remove(0);
            self.bytes -= gone.len();
        }
    }
}

// A 1x1 grey PNG drawn in place of a picture whose bytes have not arrived. A `tEXt` chunk keeps
// what the picture really is (hash, size, type) so a slide edited meanwhile still names it.
const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
const PNG_IHDR: [u8; 25] = [0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x3a, 0x7e, 0x9b, 0x55];
const PNG_IDAT: [u8; 22] = [0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xb8, 0x00, 0x00, 0x00, 0xd2, 0x00, 0xd1, 0xdb, 0xd9, 0x0e, 0xb7];
const PNG_IEND: [u8; 12] = [0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];
const STAND_IN_KEY: &[u8] = b"dtsPending";

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// What a stand-in stands for.
struct StandIn {
    hash: String,
    size: usize,
    mime: String,
}

fn stand_in_bytes(hash: &str, size: usize, mime: &str) -> Vec<u8> {
    let mut text = STAND_IN_KEY.to_vec();
    text.push(0);
    text.extend_from_slice(format!("{hash} {size} {mime}").as_bytes());
    let mut chunk = (text.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(&text);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    [&PNG_SIG[..], &PNG_IHDR, &chunk, &PNG_IDAT, &PNG_IEND].concat()
}

fn stand_in_of(data: &[u8]) -> Option<StandIn> {
    if data.len() > 512 || !data.starts_with(&PNG_SIG) {
        return None;
    }
    let at = data.windows(STAND_IN_KEY.len() + 1).position(|w| w[..STAND_IN_KEY.len()] == *STAND_IN_KEY && w[STAND_IN_KEY.len()] == 0)?;
    // The chunk's length (before its `tEXt` tag) says where the text ends; its CRC follows.
    let len = u32::from_be_bytes(data.get(at.checked_sub(8)?..at.checked_sub(4)?)?.try_into().ok()?) as usize;
    let start = at + STAND_IN_KEY.len() + 1;
    let text = std::str::from_utf8(data.get(start..(at + len).min(data.len()))?).ok()?;
    let mut parts = text.splitn(3, ' ');
    let hash = parts.next().filter(|h| is_hash(h))?.to_string();
    let size = parts.next()?.parse().ok()?;
    Some(StandIn { hash, size, mime: parts.next()?.to_string() })
}

/// Whether `m` is the stand-in drawn for a picture that has not arrived.
pub fn is_stand_in(m: &MediaItem) -> bool {
    stand_in_of(&m.data).is_some()
}

/// A slide ready to send: its region data and the media over [`MAX_BLOB`] that must reach the
/// server first (hash and bytes).
pub struct Encoded {
    pub data: Value,
    pub big: Vec<(String, Arc<Vec<u8>>)>,
}

/// The `data` of slide `id`'s region.
pub fn encode_slide(p: &Presentation, id: SlideId, store: &mut BlobStore) -> Result<Encoded, CodecError> {
    let slide = p.slides.iter().find(|s| s.id == id).ok_or_else(|| bad(format!("slide {} is not here", id.0)))?;
    let mut ids = vec![];
    crate::cmd::edit::media_of_shapes(&slide.shapes, &mut ids);
    ids.sort_by_key(|m| m.0);
    ids.dedup();
    if ids.len() > MAX_MEDIA {
        return Err(CodecError::TooLarge);
    }
    let mut media = vec![];
    let mut big = vec![];
    for mid in ids {
        let Some(m) = p.media(mid) else { continue };
        // A picture still on its way here is passed on by name, never as the stand-in's bytes.
        if let Some(si) = stand_in_of(&m.data) {
            media.push(json!({"id": m.id.0, "name": m.name, "mime": si.mime, "link": m.link, "hash": si.hash, "size": si.size}));
            continue;
        }
        if m.data.len() > MAX_BIG {
            return Err(CodecError::TooLarge);
        }
        let hash = store.hash_of(&m.data);
        let mut item = json!({"id": m.id.0, "name": m.name, "mime": m.content_type, "link": m.link, "hash": hash, "size": m.data.len()});
        if m.data.len() <= MAX_BLOB {
            item["b64"] = Value::String(base64_encode(&m.data));
        } else if !big.iter().any(|(h, _)| *h == hash) {
            big.push((hash, m.data.clone()));
        }
        media.push(item);
    }
    let data = json!({"v": 1, "slide": serde_json::to_value(&**slide).map_err(|e| bad(e.to_string()))?, "media": media});
    // The size check is on the JSON text the bridge will carry.
    if data.to_string().len() > MAX_REGION {
        return Err(CodecError::TooLarge);
    }
    Ok(Encoded { data, big })
}

/// What [`decode_slide`] made.
pub struct Decoded {
    pub presentation: Presentation,
    /// Hashes of pictures whose bytes this editor does not have yet (a stand-in is drawn for each).
    pub missing: Vec<String>,
}

/// A presentation holding just the slide of a `slide-replace` region and its media, as the
/// sender had them (ids and all), for [`crate::Session::collab_apply_slide`].
pub fn decode_slide(v: &Value, store: &mut BlobStore) -> Result<Decoded, CodecError> {
    if v.get("v").and_then(Value::as_u64) != Some(1) {
        return Err(bad("unknown version"));
    }
    let slide: Slide = serde_json::from_value(v.get("slide").cloned().ok_or_else(|| bad("no slide"))?).map_err(|e| bad(e.to_string()))?;
    let list = v.get("media").and_then(Value::as_array).map_or(&[][..], |a| &a[..]);
    if list.len() > MAX_MEDIA {
        return Err(bad("too many media"));
    }
    let mut p = Presentation::default();
    let mut missing = vec![];
    let mut max = slide.id.0;
    deckcraft_model::walk(&slide.shapes, &mut |s, _| max = max.max(s.id.0));
    for m in list {
        let id = m.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad("media id"))?;
        let link = m.get("link").and_then(Value::as_str).map(str::to_string);
        let hash = m.get("hash").and_then(Value::as_str).filter(|h| is_hash(h)).ok_or_else(|| bad("media hash"))?;
        let name = m.get("name").and_then(Value::as_str).unwrap_or("media").to_string();
        let mime = m.get("mime").and_then(Value::as_str).unwrap_or("application/octet-stream").to_string();
        let (data, content_type) = match m.get("b64").and_then(Value::as_str) {
            Some(b64) => {
                if b64.len() > MAX_BLOB.div_ceil(3) * 4 + 4 {
                    return Err(bad("media too large"));
                }
                let data = base64_decode(b64).ok_or_else(|| bad("media base64"))?;
                if content_hash(&data) != hash {
                    return Err(bad("media hash does not match its bytes"));
                }
                let data = Arc::new(data);
                store.put(hash.to_string(), data.clone());
                (data, mime)
            }
            // Named only: the bytes come over the blob channel.
            None => match store.get(hash) {
                Some(data) => (data, mime),
                None => {
                    let size = m.get("size").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).filter(|n| *n <= MAX_BIG).ok_or_else(|| bad("media size"))?;
                    if !missing.iter().any(|h| h == hash) {
                        missing.push(hash.to_string());
                    }
                    (Arc::new(stand_in_bytes(hash, size, &mime)), "image/png".to_string())
                }
            },
        };
        max = max.max(id);
        p.media.push(MediaItem {
            id: MediaId(id),
            name,
            content_type,
            data,
            link,
        });
    }
    p.next_id = max;
    p.slides = vec![Arc::new(slide)];
    Ok(Decoded { presentation: p, missing })
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

    fn enc(s: &Session, id: SlideId, st: &mut BlobStore) -> Result<Encoded, CodecError> {
        encode_slide(&s.doc().unwrap().doc, id, st)
    }

    #[test]
    fn a_slide_with_a_picture_round_trips_through_the_wire_form() {
        let (s, id) = with_picture(&[7u8; 1000]);
        let e = enc(&s, id, &mut BlobStore::default()).unwrap();
        assert!(e.big.is_empty());
        let text = e.data.to_string();
        let back = decode_slide(&serde_json::from_str(&text).unwrap(), &mut BlobStore::default()).unwrap();
        assert!(back.missing.is_empty());
        assert_eq!(*back.presentation.slides[0], *s.doc().unwrap().doc.slides[0]);
        assert_eq!(*back.presentation.media[0].data, vec![7u8; 1000]);
    }

    #[test]
    fn a_big_picture_goes_by_hash_and_a_stand_in_is_drawn_until_its_bytes_arrive() {
        let bytes = vec![9u8; MAX_BLOB + 1];
        let (s, id) = with_picture(&bytes);
        let e = enc(&s, id, &mut BlobStore::default()).unwrap();
        assert!(e.data["media"][0].get("b64").is_none());
        assert!(e.data.to_string().len() < MAX_BLOB / 4, "only the name travels in the region");
        assert_eq!(e.big.len(), 1);
        assert_eq!(e.big[0].0, content_hash(&bytes));
        // A receiver without the bytes gets a stand-in and a list of what it needs.
        let mut rx = BlobStore::default();
        let d = decode_slide(&e.data, &mut rx).unwrap();
        assert_eq!(d.missing, vec![content_hash(&bytes)]);
        assert!(is_stand_in(&d.presentation.media[0]));
        let img = image::load_from_memory(&d.presentation.media[0].data).expect("the stand-in is a real picture");
        assert_eq!((img.width(), img.height()), (1, 1));
        assert_eq!(d.presentation.media[0].content_type, "image/png");
        // Passing on a slide that still holds the stand-in names the real picture, not the stand-in.
        let mut mid = Session::with_new();
        let st = mid.doc_mut().unwrap();
        st.doc = Arc::new(d.presentation.clone());
        let again = encode_slide(&st.doc, id, &mut BlobStore::default()).unwrap();
        assert_eq!(again.data["media"][0]["hash"], e.data["media"][0]["hash"]);
        assert_eq!(again.data["media"][0]["mime"], "image/png");
        assert!(again.big.is_empty());
        assert!(again.data["media"][0].get("b64").is_none());
        // Once the bytes are known the same data decodes whole.
        rx.put(content_hash(&bytes), Arc::new(bytes.clone()));
        let d = decode_slide(&e.data, &mut rx).unwrap();
        assert!(d.missing.is_empty());
        assert_eq!(*d.presentation.media[0].data, bytes);
    }

    #[test]
    fn what_cannot_fit_is_refused_before_anything_is_made() {
        let (s, id) = with_picture(&vec![1u8; MAX_BIG + 1]);
        assert_eq!(enc(&s, id, &mut BlobStore::default()).err(), Some(CodecError::TooLarge));
        // Several inline pictures can still be too much together.
        let (mut s, id) = with_picture(&vec![2u8; MAX_BLOB]);
        let st = s.doc_mut().unwrap();
        let doc = Arc::make_mut(&mut st.doc);
        for n in 3..8u8 {
            let m = doc.add_media("q.png", "image/png", vec![n; MAX_BLOB]).unwrap();
            let sid = ShapeId(doc.alloc_id().unwrap());
            Arc::make_mut(&mut doc.slides[0]).shapes.push(Shape { id: sid, kind: ShapeKind::Picture { fill: PictureFill { media: m, ..Default::default() } }, ..Default::default() });
        }
        assert_eq!(enc(&s, id, &mut BlobStore::default()).err(), Some(CodecError::TooLarge));
        assert!(matches!(enc(&s, SlideId(9_999), &mut BlobStore::default()), Err(CodecError::Bad(_))));
    }

    #[test]
    fn damaged_or_odd_data_is_refused_not_applied() {
        let (s, id) = with_picture(&[5u8; 64]);
        let good = enc(&s, id, &mut BlobStore::default()).unwrap().data;
        let dec = |v: &Value| decode_slide(v, &mut BlobStore::default());
        let mut tampered = good.clone();
        tampered["media"][0]["hash"] = json!("0".repeat(64));
        assert!(matches!(dec(&tampered), Err(CodecError::Bad(_))));
        let mut short = good.clone();
        short["media"][0]["hash"] = json!("00000000000000000000000000000000");
        assert!(matches!(dec(&short), Err(CodecError::Bad(_))));
        let mut bytes = good.clone();
        bytes["media"][0]["b64"] = json!("not base64!!");
        assert!(matches!(dec(&bytes), Err(CodecError::Bad(_))));
        let mut huge = good.clone();
        huge["media"][0]["b64"] = json!("A".repeat(MAX_BLOB * 2));
        assert!(matches!(dec(&huge), Err(CodecError::Bad(_))));
        let mut named = good.clone();
        named["media"][0].as_object_mut().unwrap().remove("b64");
        named["media"][0]["size"] = json!(MAX_BIG + 1);
        assert!(matches!(dec(&named), Err(CodecError::Bad(_))));
        let mut v2 = good.clone();
        v2["v"] = json!(2);
        assert!(dec(&v2).is_err());
        assert!(dec(&json!({"v": 1})).is_err());
        assert!(dec(&json!(null)).is_err());
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
        assert_eq!(content_hash(b"x").len(), 64);
        assert!(is_hash(&content_hash(b"x")));
    }

    #[test]
    fn the_hash_is_sha256() {
        assert_eq!(content_hash(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(content_hash(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // Lengths around the 55/56/64-byte padding edges, against known digests.
        assert_eq!(content_hash(&[b'a'; 56]), "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
        assert_eq!(content_hash(&[b'a'; 64]), "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
        assert_eq!(content_hash(&vec![b'a'; 1_000_000]), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn the_store_hashes_an_allocation_once_and_drops_the_oldest() {
        let mut st = BlobStore::default();
        let a = Arc::new(vec![1u8; 10]);
        assert_eq!(st.hash_of(&a), content_hash(&a));
        assert_eq!(st.items.len(), 1);
        assert_eq!(st.hash_of(&a), content_hash(&a));
        assert_eq!(st.items.len(), 1);
        for n in 0..(STORE_ITEMS + 5) {
            st.put(format!("{n:064x}"), Arc::new(vec![0u8; 1]));
        }
        assert!(st.items.len() <= STORE_ITEMS);
        assert!(st.get(&content_hash(&a)).is_none(), "the oldest went first");
    }
}
