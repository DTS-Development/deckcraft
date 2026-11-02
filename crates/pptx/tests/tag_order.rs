//! The order the host page counts tags in (`deckcraft:goto`): slides as `sldIdLst` lists them, not
//! as the slide files are numbered, and shapes as the slide's shape tree lists them.

use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use deckcraft_geom::Xfrm;
use deckcraft_model::table::Table;
use deckcraft_model::{Presentation, Shape, ShapeId, ShapeKind, Slide, TextBody, defaults};

fn text_shape(p: &mut Presentation, text: &str) -> Shape {
    let id = ShapeId(p.alloc_id().expect("ids"));
    Shape { id, name: text.into(), xfrm: Some(Xfrm::new(10.0, 10.0, 80.0, 40.0)), text: Some(TextBody::from_text(text)), ..Default::default() }
}

/// Every text on a slide in the order the host counts them.
fn texts(shapes: &[Shape], out: &mut Vec<String>) {
    for s in shapes {
        if let Some(t) = &s.text {
            out.push(t.text());
        }
        match &s.kind {
            ShapeKind::Table(t) => out.extend(t.rows.iter().flat_map(|r| r.cells.iter().map(|c| c.text.text()))),
            ShapeKind::Group { children, .. } => texts(children, out),
            _ => {}
        }
    }
}

fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).expect("entry");
            let mut d = vec![];
            f.read_to_end(&mut d).expect("read");
            (f.name().to_string(), d)
        })
        .collect()
}

fn rezip(es: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (n, d) in es {
        z.start_file(n.as_str(), o).expect("start");
        z.write_all(d).expect("write");
    }
    z.finish().expect("finish").into_inner()
}

/// Reverse the `sldId` entries of `presentation.xml`, so slide1.xml is the last slide shown.
fn reverse_slide_list(es: &mut [(String, Vec<u8>)]) {
    let (_, d) = es.iter_mut().find(|(n, _)| n == "ppt/presentation.xml").expect("presentation.xml");
    let xml = String::from_utf8(d.clone()).expect("utf8");
    let (a, b) = (xml.find("<p:sldIdLst>").expect("list") + "<p:sldIdLst>".len(), xml.find("</p:sldIdLst>").expect("end"));
    let mut ids: Vec<&str> = xml[a..b].split_inclusive("/>").collect();
    ids.reverse();
    *d = format!("{}{}{}", &xml[..a], ids.concat(), &xml[b..]).into_bytes();
}

#[test]
fn slides_come_in_presentation_order_not_file_order() {
    let mut p = defaults::new_presentation(None);
    let first = Arc::clone(&p.slides[0]);
    let mut slides = vec![first];
    for _ in 0..2 {
        let id = p.alloc_id().expect("ids");
        slides.push(Arc::new(Slide { id: deckcraft_model::SlideId(id), ..(*slides[0]).clone() }));
    }
    for (i, sl) in slides.iter_mut().enumerate() {
        let sh = text_shape(&mut p, &format!("S{}", i + 1));
        Arc::make_mut(sl).shapes = vec![sh];
    }
    p.slides = slides;
    let mut es = entries(&deckcraft_pptx::export(&p).expect("export"));
    reverse_slide_list(&mut es);
    let q = deckcraft_pptx::import(&rezip(&es)).expect("import");
    let order: Vec<String> = q.slides.iter().map(|s| s.shapes[0].text.as_ref().expect("text").text()).collect();
    assert_eq!(order, ["S3", "S2", "S1"]);
}

#[test]
fn shapes_come_in_shape_tree_order_groups_and_tables_expanded() {
    let mut p = defaults::new_presentation(None);
    let a = text_shape(&mut p, "A");
    let (b, c) = (text_shape(&mut p, "B"), text_shape(&mut p, "C"));
    let gx = Xfrm::new(10.0, 10.0, 200.0, 40.0);
    let group = Shape {
        id: ShapeId(p.alloc_id().expect("ids")),
        name: "G".into(),
        xfrm: Some(gx),
        kind: ShapeKind::Group { children: vec![b, c], child: gx },
        ..Default::default()
    };
    let mut table = Table::new(2, 2, 200.0, 30.0);
    for (r, row) in table.rows.iter_mut().enumerate() {
        for (c, cell) in row.cells.iter_mut().enumerate() {
            cell.text = TextBody::from_text(&format!("T{r}{c}"));
        }
    }
    let tbl = Shape {
        id: ShapeId(p.alloc_id().expect("ids")),
        name: "T".into(),
        xfrm: Some(Xfrm::new(10.0, 100.0, 200.0, 60.0)),
        kind: ShapeKind::Table(table),
        ..Default::default()
    };
    let d = text_shape(&mut p, "D");
    Arc::make_mut(&mut p.slides[0]).shapes = vec![a, group, tbl, d];
    let q = deckcraft_pptx::import(&deckcraft_pptx::export(&p).expect("export")).expect("import");
    let mut out = vec![];
    texts(&q.slides[0].shapes, &mut out);
    assert_eq!(out, ["A", "B", "C", "T00", "T01", "T10", "T11", "D"]);
}
