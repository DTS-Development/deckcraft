//! "Go to this tag" the host page asks for (`deckcraft:goto`): the slide opens and the n-th
//! occurrence of the text on it is selected, in edit mode.
//!
//! The count follows the host's rules, not `text.find`'s: slides in presentation order, shapes in
//! the order of the slide's shape tree (groups expanded in place, tables row by row, cell by
//! cell), paragraph by paragraph, speaker notes left out. A paragraph's text is all its runs
//! joined (a line break counts as `\n`); matches are exact, left to right, never overlapping and
//! never across paragraphs.

use deckcraft_model::{Presentation, Shape, ShapeId, ShapeKind, TextBody};

use crate::SlideApp;

/// Longest text a request may look for (chars), and the deepest group nesting walked.
pub const MAX_TEXT_CHARS: usize = 2000;
const MAX_DEPTH: usize = 64;

/// Why the jump didn't happen, as the parent hears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NotFound,
    Error,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NotFound => "not-found",
            Reason::Error => "error",
        }
    }
}

/// One match: where its paragraph is and the char range inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub shape: ShapeId,
    pub cell: Option<(usize, usize)>,
    pub para: usize,
    pub start: usize,
    pub end: usize,
}

/// Non-overlapping exact matches of `pat` in `text` as char ranges.
fn matches(text: &[char], pat: &[char]) -> Vec<(usize, usize)> {
    let n = pat.len();
    let mut out = vec![];
    let mut i = 0;
    while n > 0 && i + n <= text.len() {
        if text[i..i + n] == *pat {
            out.push((i, i + n));
            i += n;
        } else {
            i += 1;
        }
    }
    out
}

/// The match that reaches zero in this text body, counting `left` down.
fn in_body(shape: ShapeId, cell: Option<(usize, usize)>, t: &TextBody, pat: &[char], left: &mut usize) -> Option<Found> {
    for (pi, p) in t.paragraphs.iter().enumerate() {
        // `content` makes a break one char, as the selection counts it.
        let chars: Vec<char> = p.runs.iter().flat_map(|r| r.content().chars()).map(|c| if c == '\u{b}' { '\n' } else { c }).collect();
        for (start, end) in matches(&chars, pat) {
            *left -= 1;
            if *left == 0 {
                return Some(Found { shape, cell, para: pi, start, end });
            }
        }
    }
    None
}

/// Walk the shapes in order, counting matches down from `left`; the one that reaches zero wins.
fn walk(shapes: &[Shape], pat: &[char], left: &mut usize, depth: usize) -> Option<Found> {
    if depth > MAX_DEPTH {
        return None;
    }
    for sh in shapes {
        if let Some(f) = sh.text.as_ref().and_then(|t| in_body(sh.id, None, t, pat, left)) {
            return Some(f);
        }
        match &sh.kind {
            ShapeKind::Table(t) => {
                for (r, row) in t.rows.iter().enumerate() {
                    for (c, cell) in row.cells.iter().enumerate() {
                        if let Some(f) = in_body(sh.id, Some((r, c)), &cell.text, pat, left) {
                            return Some(f);
                        }
                    }
                }
            }
            ShapeKind::Group { children, .. } => {
                if let Some(f) = walk(children, pat, left, depth + 1) {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `occurrence`-th (from 1) match of `text` on `slide` (from 1, presentation order).
pub fn locate(doc: &Presentation, slide: usize, text: &str, occurrence: usize) -> Result<Found, Reason> {
    let pat: Vec<char> = text.chars().collect();
    if pat.is_empty() || pat.len() > MAX_TEXT_CHARS || slide == 0 || occurrence == 0 {
        return Err(Reason::Error);
    }
    let sl = doc.slides.get(slide - 1).ok_or(Reason::NotFound)?;
    let mut left = occurrence;
    walk(&sl.shapes, &pat, &mut left, 0).ok_or(Reason::NotFound)
}

/// Open the slide and select the match, editing its text. An `Err` leaves the selection wherever
/// it got to, but the presentation itself is never changed.
pub fn goto(app: &mut SlideApp, slide: usize, text: &str, occurrence: usize) -> Result<(), Reason> {
    let found = {
        let st = app.session.doc().map_err(|_| Reason::Error)?;
        locate(&st.doc, slide, text, occurrence)?
    };
    let fail = |e: String| {
        log::warn!("goto: {e}");
        Reason::Error
    };
    app.run("slide.go", serde_json::json!({ "index": slide - 1 })).map_err(fail)?;
    let mut edit = serde_json::json!({ "id": found.shape.0, "at": [found.para, found.start] });
    if let Some((r, c)) = found.cell {
        edit["cell"] = serde_json::json!([r, c]);
    }
    app.run("text.edit", edit).map_err(fail)?;
    app.run("text.select", serde_json::json!({ "anchor": [found.para, found.start], "caret": [found.para, found.end] })).map_err(fail)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> SlideApp {
        SlideApp::new(deckcraft_engine::Session::with_new(), Default::default())
    }

    #[test]
    fn matches_are_exact_and_do_not_overlap() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(matches(&c("{{a}}x{{a}}"), &c("{{a}}")), vec![(0, 5), (6, 11)]);
        assert_eq!(matches(&c("aaaa"), &c("aa")), vec![(0, 2), (2, 4)]);
        assert!(matches(&c("{{A}}"), &c("{{a}}")).is_empty());
    }

    #[test]
    fn bad_requests_are_errors_and_missing_ones_not_found() {
        let mut a = app();
        let doc = a.session.doc().unwrap().doc.clone();
        assert_eq!(locate(&doc, 0, "x", 1), Err(Reason::Error));
        assert_eq!(locate(&doc, 1, "", 1), Err(Reason::Error));
        assert_eq!(locate(&doc, 1, "x", 0), Err(Reason::Error));
        assert_eq!(locate(&doc, 99, "x", 1), Err(Reason::NotFound));
        assert_eq!(locate(&doc, 1, "{{nope}}", 1), Err(Reason::NotFound));
        assert_eq!(goto(&mut a, 1, "{{nope}}", 1), Err(Reason::NotFound));
        let mut none = SlideApp::new(deckcraft_engine::Session::new(), Default::default());
        assert_eq!(goto(&mut none, 1, "x", 1), Err(Reason::Error));
    }

    #[test]
    fn the_nth_tag_is_selected_in_shapes_then_table_cells() {
        let mut a = app();
        a.run("insert.textbox", json!({})).ok();
        // A text shape, then a table, on slide 1: shape tree order is shape first, then table.
        let id = a.session.doc().unwrap().doc.slides[0].shapes.first().map(|s| s.id);
        let id = id.expect("the new deck has a shape to type into");
        a.run("text.edit", json!({ "id": id.0 })).unwrap();
        a.run("text.insert", json!({ "text": "{{a}} and {{a}}" })).unwrap();
        a.run("text.exit", json!({})).unwrap();
        crate::host_table::insert_table(&mut a, &[vec!["{{a}}".into(), "x".into()], vec!["y".into(), "{{a}}".into()]]).unwrap();
        a.run("slide.new", json!({})).unwrap();

        let doc = a.session.doc().unwrap().doc.clone();
        let f = |n| locate(&doc, 1, "{{a}}", n).unwrap();
        assert_eq!((f(1).cell, f(1).start, f(1).end), (None, 0, 5));
        assert_eq!((f(2).cell, f(2).start), (None, 10));
        assert_eq!((f(3).cell, f(3).para), (Some((0, 0)), 0));
        assert_eq!(f(4).cell, Some((1, 1)));
        assert_eq!(locate(&doc, 1, "{{a}}", 5), Err(Reason::NotFound));

        // The jump leaves the app on that slide with the 4th match selected for editing.
        goto(&mut a, 1, "{{a}}", 4).unwrap();
        let st = a.session.doc().unwrap();
        assert_eq!(st.selection.slide, 0);
        let t = st.selection.text.clone().unwrap();
        assert_eq!((t.cell, t.anchor, t.caret), (Some((1, 1)), (0, 0), (0, 5)));
        assert!(!st.selection.text.as_ref().unwrap().notes);
    }

    /// The shared fixture: slide files are numbered differently from the presentation order, and
    /// each probe says which paragraph the n-th match must be in.
    #[test]
    fn the_hosts_probes_point_at_the_same_paragraphs() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tag-jump");
        let bytes = std::fs::read(format!("{dir}/jump.pptx")).unwrap();
        let expected: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{dir}/expected.json")).unwrap()).unwrap();
        let doc = deckcraft_engine::cmd::file::open_presentation("jump.pptx", &bytes).unwrap();
        let para_text = |f: &Found, slide: usize| -> String {
            let mut texts = vec![];
            fn collect(shapes: &[Shape], out: &mut Vec<(ShapeId, Option<(usize, usize)>, TextBody)>) {
                for sh in shapes {
                    out.extend(sh.text.clone().map(|t| (sh.id, None, t)));
                    match &sh.kind {
                        ShapeKind::Table(t) => {
                            for (r, row) in t.rows.iter().enumerate() {
                                out.extend(row.cells.iter().enumerate().map(|(c, x)| (sh.id, Some((r, c)), x.text.clone())));
                            }
                        }
                        ShapeKind::Group { children, .. } => collect(children, out),
                        _ => {}
                    }
                }
            }
            collect(&doc.slides[slide - 1].shapes, &mut texts);
            let (_, _, body) = texts.iter().find(|(id, cell, _)| *id == f.shape && *cell == f.cell).unwrap();
            body.paragraphs[f.para].text()
        };
        let probes = expected["pptx"]["probes"].as_array().unwrap();
        assert!(!probes.is_empty());
        for probe in probes {
            let l = &probe["location"];
            let (slide, text, n) = (l["slide"].as_u64().unwrap() as usize, l["text"].as_str().unwrap(), l["occurrence"].as_u64().unwrap() as usize);
            let f = locate(&doc, slide, text, n).unwrap_or_else(|e| panic!("{l}: {e:?}"));
            assert_eq!(para_text(&f, slide), probe["paragraph"].as_str().unwrap(), "{l}");
            // The match is where the host says: the slice at the range is the text itself.
            let chars: Vec<char> = para_text(&f, slide).chars().collect();
            assert_eq!(chars[f.start..f.end].iter().collect::<String>(), text);
        }
        // One past the last occurrence is not found, so a stale count can't land on a neighbour.
        assert_eq!(locate(&doc, 2, "{{name}}", 5), Err(Reason::NotFound));
        // Error and warning locations of the fixture all resolve.
        for item in ["errors", "warnings"] {
            for e in expected["pptx"][item].as_array().unwrap() {
                let l = &e["location"];
                let (slide, text, n) =
                    (l["slide"].as_u64().unwrap() as usize, l["text"].as_str().unwrap(), l["occurrence"].as_u64().unwrap() as usize);
                assert!(locate(&doc, slide, text, n).is_ok(), "{l}");
            }
        }
    }
}
