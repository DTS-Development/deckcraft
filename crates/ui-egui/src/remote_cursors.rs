//! Where the other people in a shared presentation are: the slide each one has open and the shapes
//! each has selected, as the parent page sends them (`collab-cursors`). Drawn on top of the slide
//! (an outline and a name tag on each selected shape) and on the slide thumbnails (a coloured dot
//! per person). Draw-only: nothing here takes input.
//!
//! The parent supplies the name and colour (the same ones as its participant list); this side only
//! checks and draws them. A malformed entry is skipped, never an error.

use deckcraft_model::{ShapeId, SlideId};
use egui::{Align2, Color32, CornerRadius, Painter, Pos2, Rect, Stroke, pos2, vec2};
use serde_json::Value;

use crate::theme;

/// Most people drawn: a longer list is cut (a hostile or broken parent can't make a frame slow).
pub const MAX_CURSORS: usize = 200;
/// Most shapes taken from one person's selection.
pub const MAX_SHAPES: usize = 64;
/// Longest name drawn, in characters.
pub const MAX_NAME_CHARS: usize = 40;
/// Name tags stacked on one spot before "+N".
const MAX_TAGS: usize = 4;
/// Grey for a colour that isn't `#rrggbb`.
const FALLBACK: [u8; 3] = [128, 128, 128];

/// One other connection's place in the presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCursor {
    pub conn: u64,
    pub name: String,
    pub color: [u8; 3],
    /// Only looking (role `view`): drawn with a dashed outline.
    pub view: bool,
    pub slide: SlideId,
    pub shapes: Vec<ShapeId>,
}

/// `#rrggbb` as bytes.
fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

/// The people in a `collab-cursors` message that have a deck position (`slide:<id>`), in order.
/// Entries of another region, of a shape the slide doesn't have (not known here) or in a shape we
/// can't read are left out; the rest are kept.
pub fn parse(m: &Value) -> Vec<RemoteCursor> {
    let Some(list) = m.get("cursors").and_then(Value::as_array) else { return vec![] };
    list.iter().take(MAX_CURSORS).filter_map(parse_one).collect()
}

fn parse_one(c: &Value) -> Option<RemoteCursor> {
    let slide = c.get("region")?.as_str()?.strip_prefix("slide:")?.parse::<u32>().ok().map(SlideId)?;
    let conn = c.get("conn")?.as_u64()?;
    let name: String = c.get("name").and_then(Value::as_str).unwrap_or_default().chars().filter(|ch| !ch.is_control()).take(MAX_NAME_CHARS).collect();
    let color = c.get("color").and_then(Value::as_str).and_then(parse_color).unwrap_or(FALLBACK);
    let view = c.get("role").and_then(Value::as_str) == Some("view");
    let shapes = c
        .get("pos")
        .and_then(|p| p.get("shapes"))
        .and_then(Value::as_array)
        .map(|a| a.iter().take(MAX_SHAPES).filter_map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok())).map(ShapeId).collect())
        .unwrap_or_default();
    Some(RemoteCursor { conn, name, color, view, slide, shapes })
}

fn color(c: &RemoteCursor, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.color[0], c.color[1], c.color[2], alpha)
}

/// Black or white, whichever reads on `c`.
fn on(c: [u8; 3]) -> Color32 {
    let l = 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
    if l > 150.0 { Color32::BLACK } else { Color32::WHITE }
}

/// The people who have slide `id` open.
pub fn on_slide(all: &[RemoteCursor], id: SlideId) -> impl Iterator<Item = &RemoteCursor> {
    all.iter().filter(move |c| c.slide == id)
}

/// A name tag whose top-left corner is at `at`, drawn on the slide or its screen. `row` stacks tags
/// that share a spot upward.
fn tag(p: &Painter, at: Pos2, c: &RemoteCursor, row: usize) {
    let font = theme::font(11.0);
    let text = if c.name.is_empty() { "?" } else { c.name.as_str() };
    let galley = p.layout_no_wrap(text.to_string(), font, on(c.color));
    let size = galley.size() + vec2(8.0, 4.0);
    let r = Rect::from_min_size(pos2(at.x, at.y - size.y * (row as f32 + 1.0)), size);
    p.rect_filled(r, CornerRadius::same(3), color(c, 255));
    if c.view {
        // A viewer's tag has a dashed border (no icon): it is told apart from an editor's by shape.
        let b = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        p.extend(egui::Shape::dashed_line(&b, Stroke::new(1.0, on(c.color)), 3.0, 2.0));
    }
    p.galley(r.min + vec2(4.0, 2.0), galley, on(c.color));
}

/// An outline and a name tag on each shape of the current slide that someone else has selected.
/// `shape_box` gives a shape's four screen corners, or `None` for a shape this slide doesn't have.
pub fn paint_on_slide(p: &Painter, clip: Rect, all: &[RemoteCursor], slide: SlideId, mut shape_box: impl FnMut(ShapeId) -> Option<[Pos2; 4]>) {
    // Tags on the same shape stack; count them by the shape's top-left corner.
    let mut stacked: Vec<(ShapeId, usize)> = vec![];
    let mut hidden: Vec<(ShapeId, usize)> = vec![];
    for c in on_slide(all, slide) {
        for id in &c.shapes {
            let Some(q) = shape_box(*id) else { continue };
            let bounds = Rect::from_points(&q);
            if !bounds.intersects(clip) {
                continue;
            }
            let stroke = Stroke::new(2.0, color(c, 255));
            if c.view {
                let mut pts = q.to_vec();
                pts.push(q[0]);
                p.extend(egui::Shape::dashed_line(&pts, stroke, 6.0, 4.0));
            } else {
                for i in 0..4 {
                    p.line_segment([q[i], q[(i + 1) % 4]], stroke);
                }
            }
            let row = stacked.iter().find(|(s, _)| s == id).map_or(0, |(_, n)| *n);
            if row < MAX_TAGS {
                tag(p, bounds.left_top(), c, row);
            } else if let Some(h) = hidden.iter_mut().find(|(s, _)| s == id) {
                h.1 += 1;
            } else {
                hidden.push((*id, 1));
            }
            match stacked.iter_mut().find(|(s, _)| s == id) {
                Some(s) => s.1 += 1,
                None => stacked.push((*id, 1)),
            }
        }
    }
    for (id, n) in hidden {
        if let Some(q) = shape_box(id) {
            let r = Rect::from_points(&q);
            p.text(pos2(r.left_top().x, r.left_top().y - 16.0 * (MAX_TAGS as f32 + 1.0)), Align2::LEFT_BOTTOM, format!("+{n}"), theme::font(11.0), Color32::GRAY);
        }
    }
}

/// A coloured dot per person on slide `id` along the bottom-right edge of its thumbnail `img`
/// (at most [`MAX_TAGS`], then "+N"); the name shows in the thumbnail's tooltip via the returned text.
pub fn paint_on_thumb(p: &Painter, img: Rect, all: &[RemoteCursor], id: SlideId) -> Option<String> {
    let here: Vec<&RemoteCursor> = on_slide(all, id).collect();
    if here.is_empty() {
        return None;
    }
    for (i, c) in here.iter().take(MAX_TAGS).enumerate() {
        let at = pos2(img.right() - 8.0 - 12.0 * i as f32, img.bottom() - 8.0);
        p.circle(at, 5.0, color(c, 255), Stroke::new(1.5, Color32::WHITE));
        if c.view {
            p.circle_stroke(at, 7.0, Stroke::new(1.0, color(c, 255)));
        }
    }
    if here.len() > MAX_TAGS {
        p.text(pos2(img.right() - 8.0 - 12.0 * MAX_TAGS as f32, img.bottom() - 8.0), Align2::RIGHT_CENTER, format!("+{}", here.len() - MAX_TAGS), theme::font(10.0), Color32::GRAY);
    }
    Some(here.iter().map(|c| if c.name.is_empty() { "?" } else { c.name.as_str() }).collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(conn: u64, region: &str, pos: Value) -> Value {
        json!({"conn": conn, "user": "u", "name": "Kim", "color": "#ff8800", "role": "edit", "region": region, "pos": pos})
    }

    #[test]
    fn reads_slide_positions_and_skips_the_rest() {
        let m = json!({"cursors": [
            entry(1, "slide:7", json!({"shapes": [3, 4]})),
            entry(2, "deck:structure", json!({})),
            entry(3, "slide:x", json!({})),
            json!({"conn": 4, "region": "slide:8"}),
            json!("junk"),
        ]});
        let got = parse(&m);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].conn, got[0].slide, got[0].shapes.clone()), (1, SlideId(7), vec![ShapeId(3), ShapeId(4)]));
        assert_eq!(got[0].color, [255, 136, 0]);
        assert!(!got[0].view);
        // No position and no name: still drawn on the thumbnail, grey.
        assert_eq!((got[1].slide, got[1].shapes.len(), got[1].name.as_str(), got[1].color), (SlideId(8), 0, "", FALLBACK));
    }

    #[test]
    fn guards_the_size_name_colour_and_shapes() {
        let many: Vec<Value> = (0..300).map(|i| entry(i, "slide:1", json!({}))).collect();
        assert_eq!(parse(&json!({"cursors": many})).len(), MAX_CURSORS);
        let shapes: Vec<u32> = (0..500).collect();
        let long = json!({"conn": 1, "name": "x".repeat(500), "color": "red", "role": "view", "region": "slide:2", "pos": {"shapes": shapes}});
        let got = parse(&json!({"cursors": [long]}));
        assert_eq!(got[0].name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(got[0].color, FALLBACK);
        assert!(got[0].view);
        assert_eq!(got[0].shapes.len(), MAX_SHAPES);
        // Not a list, wrong shapes: nothing, never a panic.
        assert!(parse(&json!({"cursors": 5})).is_empty());
        assert!(parse(&json!({})).is_empty());
        let odd = json!({"conn": 1, "region": "slide:2", "pos": {"shapes": [-1, "a", 1.5, 4294967296u64, 9]}});
        assert_eq!(parse(&json!({"cursors": [odd]}))[0].shapes, vec![ShapeId(9)]);
        // Control characters can't ride in a name.
        let ctl = json!({"conn": 1, "name": "a\u{0}b\nc", "region": "slide:2"});
        assert_eq!(parse(&json!({"cursors": [ctl]}))[0].name, "abc");
    }

    #[test]
    fn colours_parse_only_as_hex() {
        assert_eq!(parse_color("#00ff7F"), Some([0, 255, 127]));
        for bad in ["00ff7f", "#fff", "#gggggg", "#00ff7f00", ""] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_people_on_the_slide_count() {
        let m = json!({"cursors": [entry(1, "slide:1", json!({})), entry(2, "slide:2", json!({})), entry(3, "slide:1", json!({}))]});
        let all = parse(&m);
        assert_eq!(on_slide(&all, SlideId(1)).count(), 2);
        assert_eq!(on_slide(&all, SlideId(9)).count(), 0);
    }
}
