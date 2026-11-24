//! View-only sessions (`Session::set_read_only`): the presentation can be looked at, copied from
//! and searched, but nothing changes it and nothing is saved.
//!
//! Every command is in exactly one of two tables: [`OK`] runs in a view-only session, [`REFUSED`]
//! does not. A command nobody classified is refused at run time, and the drift test below fails
//! until it is put in a table, so a new command is never silently open to edits. The tables sit
//! under the host lock (`hostlock`): a policy can switch a command on, never past this.
//!
//! The same grade covers the gate in [`crate::Session::edit`], which the pointer tools (drag, resize,
//! draw) reach without going through a command id.

/// Commands a view-only session runs: they read, move the selection or the caret, play, or search.
pub const OK: &[&str] = &[
    "animation.get",
    "animation.list",
    "commands.list",
    "comment.list",
    "comment.next",
    "comment.prev",
    "design.ideas",
    "design.themes",
    "design.variants",
    "document.inspect",
    "edit.copy",
    "edit.deselect",
    "edit.select",
    "edit.selectAll",
    "file.render",
    "file.saveBytes",
    "font.used",
    "format.ruler",
    "format.state",
    "guide.list",
    "insert.icons",
    "media.gifPlay",
    "media.info",
    "media.pause",
    "media.play",
    "media.seek",
    "media.stop",
    "media.toggle",
    "picture.recolors",
    "picture.styles",
    "review.accessibility",
    "review.dictionary",
    "review.revisions",
    "review.spelling",
    "review.thesaurus",
    "shape.inspect",
    "shape.points",
    "shape.presets",
    "shape.sites",
    "show.fromCurrent",
    "show.fromStart",
    "slide.colorAt",
    "slide.first",
    "slide.go",
    "slide.inspect",
    "slide.last",
    "slide.next",
    "slide.previous",
    "slide.selectSlides",
    "table.selectCells",
    "text.edit",
    "text.exit",
    "text.find",
    "text.get",
    "text.move",
    "text.select",
    "text.selectAll",
    "text.selectParagraph",
    "text.selectWord",
    "transition.list",
];

/// Commands a view-only session refuses: every edit, and everything that opens, closes, saves or
/// exports a file.
pub const REFUSED: &[&str] = &[
    "animation.add",
    "animation.clear",
    "animation.move",
    "animation.options",
    "animation.painter",
    "animation.painterApply",
    "animation.remove",
    "animation.set",
    "animation.timing",
    "arrange.align",
    "arrange.bringForward",
    "arrange.bringToFront",
    "arrange.distribute",
    "arrange.flipHorizontal",
    "arrange.flipVertical",
    "arrange.group",
    "arrange.nudge",
    "arrange.regroup",
    "arrange.reorder",
    "arrange.rotateLeft",
    "arrange.rotateRight",
    "arrange.sendBackward",
    "arrange.sendToBack",
    "arrange.ungroup",
    "chart.data",
    "chart.options",
    "chart.type",
    "comment.add",
    "comment.delete",
    "comment.reply",
    "comment.resolve",
    "design.applyIdea",
    "design.background",
    "design.browseTheme",
    "design.colors",
    "design.effects",
    "design.fonts",
    "design.headerFooter",
    "design.hideBackgroundGraphics",
    "design.slideSize",
    "design.theme",
    "design.variant",
    "draw.inkToShape",
    "edit.cut",
    "edit.delete",
    "edit.duplicate",
    "edit.paste",
    "edit.pasteSpecial",
    "edit.pasteText",
    "edit.redo",
    "edit.undo",
    "file.adopt",
    "file.close",
    "file.export",
    "file.info",
    "file.new",
    "file.open",
    "file.openBytes",
    "file.properties",
    "file.recovery.discard",
    "file.recovery.list",
    "file.recovery.open",
    "file.recovery.save",
    "file.revert",
    "file.save",
    "file.saveAs",
    "file.saveTemplate",
    "file.templates",
    "font.add",
    "font.replace",
    "font.rescan",
    "format.align",
    "format.alignCenter",
    "format.alignLeft",
    "format.alignRight",
    "format.anchor",
    "format.autofit",
    "format.bold",
    "format.bullets",
    "format.caps",
    "format.case",
    "format.clear",
    "format.color",
    "format.columns",
    "format.direction",
    "format.font",
    "format.fontDialog",
    "format.grow",
    "format.highlight",
    "format.indent",
    "format.italic",
    "format.justify",
    "format.language",
    "format.lineSpacing",
    "format.margins",
    "format.numbering",
    "format.outdent",
    "format.painter",
    "format.painterApply",
    "format.paragraph",
    "format.paragraphDialog",
    "format.rtl",
    "format.shrink",
    "format.size",
    "format.spacing",
    "format.strikethrough",
    "format.subscript",
    "format.superscript",
    "format.tabs",
    "format.textOutline",
    "format.textShadow",
    "format.textTransparency",
    "format.textWarp",
    "format.underline",
    "format.wrap",
    "guide.add",
    "guide.delete",
    "guide.move",
    "handout.placeholders",
    "insert.actionButton",
    "insert.audio",
    "insert.chart",
    "insert.dateTime",
    "insert.hyperlink",
    "insert.icon",
    "insert.photoAlbum",
    "insert.photoAlbumEdit",
    "insert.picture",
    "insert.slideNumber",
    "insert.smartArt",
    "insert.symbol",
    "insert.table",
    "insert.textBox",
    "insert.video",
    "insert.wordArt",
    "insert.zoom",
    "master.deleteLayout",
    "master.insertLayout",
    "master.insertPlaceholder",
    "master.placeholders",
    "master.preserve",
    "master.renameLayout",
    "media.options",
    "media.posterFrame",
    "notes.placeholders",
    "picture.adjust",
    "picture.artisticEffect",
    "picture.change",
    "picture.compress",
    "picture.crop",
    "picture.recolor",
    "picture.removeBackground",
    "picture.reset",
    "picture.style",
    "picture.transparentColor",
    "review.accept",
    "review.addToDictionary",
    "review.compare",
    "review.endCompare",
    "review.replaceWord",
    "review.spellingChange",
    "section.add",
    "section.move",
    "section.remove",
    "section.removeAll",
    "section.rename",
    "selection.hideAll",
    "selection.select",
    "selection.showAll",
    "shape.action",
    "shape.addPoint",
    "shape.adjust",
    "shape.altText",
    "shape.change",
    "shape.connect",
    "shape.deletePoint",
    "shape.editPoints",
    "shape.effects",
    "shape.fill",
    "shape.flip",
    "shape.freeform",
    "shape.insert",
    "shape.line",
    "shape.lock",
    "shape.merge",
    "shape.move",
    "shape.moveHandle",
    "shape.movePoint",
    "shape.pointType",
    "shape.quickStyle",
    "shape.rename",
    "shape.resize",
    "shape.rotate",
    "shape.setBounds",
    "shape.setDefault",
    "shape.sizePosition",
    "shape.smoothCorners",
    "shape.visible",
    "show.customShow",
    "show.setup",
    "slide.delete",
    "slide.duplicate",
    "slide.fromOutline",
    "slide.hide",
    "slide.layout",
    "slide.move",
    "slide.new",
    "slide.notes",
    "slide.rename",
    "slide.reset",
    "slide.reuse",
    "smartArt.addNode",
    "smartArt.colors",
    "smartArt.demote",
    "smartArt.fromText",
    "smartArt.layout",
    "smartArt.nodes",
    "smartArt.picture",
    "smartArt.promote",
    "smartArt.removeNode",
    "smartArt.setNodes",
    "smartArt.style",
    "smartArt.toShapes",
    "smartArt.toText",
    "table.borders",
    "table.cellAlign",
    "table.cellFill",
    "table.cellMargins",
    "table.columnWidth",
    "table.deleteColumn",
    "table.deleteRow",
    "table.distributeColumns",
    "table.distributeRows",
    "table.drawLine",
    "table.eraseLine",
    "table.insertColumnLeft",
    "table.insertColumnRight",
    "table.insertRowAbove",
    "table.insertRowBelow",
    "table.merge",
    "table.options",
    "table.rowHeight",
    "table.split",
    "table.style",
    "text.autocorrect",
    "text.commit",
    "text.compose",
    "text.delete",
    "text.insert",
    "text.replace",
    "text.set",
    "transition.applyAll",
    "transition.options",
    "transition.set",
    "transition.sound",
    "transition.timing",
    "view.closeMaster",
    "view.handoutMaster",
    "view.notesMaster",
    "view.slideMaster",
    "window.next",
];

/// Whether a view-only session runs the engine command `id`; an id nobody classified is refused.
pub fn permits(id: &str) -> bool {
    OK.contains(&id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{EngineError, Session};
    use serde_json::{Value as Json, json};
    use std::sync::Arc;

    /// A view-only session with a few slides, shapes, a table and text to be edited, as a parent
    /// would hold it.
    fn deck() -> Session {
        let mut s = Session::with_new();
        for _ in 0..2 {
            s.execute("slide.new", &json!({})).unwrap();
        }
        s.execute("insert.textBox", &json!({})).unwrap();
        s.execute("text.insert", &json!({"text": "Hello world"})).unwrap();
        s.execute("text.exit", &json!({})).unwrap();
        s.execute("insert.table", &json!({"rows": 2, "cols": 2})).unwrap();
        let st = s.active_mut().unwrap();
        st.saved_doc = st.doc.clone();
        s.set_read_only(true);
        s
    }

    /// The state an edit would change: the document pointer, the saved pointer, the undo depth.
    fn fingerprint(s: &Session) -> (usize, usize, usize, usize) {
        let st = s.active().unwrap();
        (Arc::as_ptr(&st.doc) as usize, Arc::as_ptr(&st.saved_doc) as usize, st.history.undo.len(), s.documents().len())
    }

    #[test]
    fn every_command_is_in_exactly_one_table() {
        for c in crate::cmd::command_specs() {
            let n = [OK.contains(&c.id), REFUSED.contains(&c.id)].iter().filter(|b| **b).count();
            assert_eq!(n, 1, "command `{}` must be in exactly one of readonly::OK / readonly::REFUSED (view-only sessions)", c.id);
        }
    }

    #[test]
    fn tables_hold_no_stale_ids_and_are_sorted() {
        for list in [OK, REFUSED] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "keep the tables sorted and free of duplicates");
            for id in list {
                assert!(crate::find_command(id).is_some(), "`{id}` is classified but is not a command");
            }
        }
    }

    #[test]
    fn ok_commands_are_not_writes() {
        // Files are never opened, closed, saved or exported, and nothing edits: no OK command is
        // an undoable edit, and none is one of the file commands the host lock grades.
        for id in OK {
            let spec = crate::find_command(id).unwrap();
            assert!(!spec.undoable, "`{id}` is undoable, which is an edit");
            assert!(!matches!(crate::hostlock::grade(id), Some(crate::hostlock::Grade::Locked | crate::hostlock::Grade::Off)), "`{id}` is a file command");
        }
    }

    #[test]
    fn a_view_only_session_refuses_what_is_refused_even_with_junk_parameters() {
        let junk = [json!({}), json!({"text": "x", "id": 1, "ids": [1], "indices": [0], "at": [0, 0], "name": "a", "data": "AAAA"}), json!(null), json!([1, 2])];
        for c in crate::cmd::command_specs() {
            let mut s = deck();
            let before = fingerprint(&s);
            for p in &junk {
                let r = s.execute(c.id, p);
                if REFUSED.contains(&c.id) {
                    assert!(matches!(r, Err(EngineError::HostLocked(_))), "`{}` ran in a view-only session", c.id);
                }
                assert_eq!(fingerprint(&s), before, "`{}` changed the presentation in a view-only session ({p})", c.id);
            }
        }
    }

    #[test]
    fn a_policy_cannot_turn_an_edit_on_in_a_view_only_session() {
        let mut s = Session::with_new();
        s.lock_host();
        s.set_read_only(true);
        s.set_policy(&[("format.bold".into(), true), ("file.saveAs".into(), true), ("slide.new".into(), true)]);
        for id in ["format.bold", "file.saveAs", "slide.new", "edit.undo", "file.save"] {
            assert!(matches!(s.execute(id, &json!({})), Err(EngineError::HostLocked(_))), "{id} ran");
        }
    }

    #[test]
    fn view_only_stays_off_until_asked_and_can_be_lifted() {
        let mut s = deck();
        assert!(s.read_only());
        s.set_read_only(false);
        assert!(!s.read_only());
        s.execute("slide.new", &Json::Null).unwrap();
        s.set_read_only(true);
        assert!(s.execute("slide.new", &Json::Null).is_err());
        assert!(s.execute("slide.next", &Json::Null).is_ok());
    }

    #[test]
    fn the_pointer_tools_cannot_edit_a_view_only_deck() {
        use crate::tools::{PointerEvent, PointerKind, ToolKind};
        let mut s = deck();
        let before = fingerprint(&s);
        // Every tool, a press-drag-release across the slide and a double click: none may change it.
        let tools = [ToolKind::Select, ToolKind::TextBox, ToolKind::Shape { preset: "rect".into() }, ToolKind::Shape { preset: "curve".into() }];
        for kind in tools {
            s.set_tool(kind);
            for (k, x, y) in [
                (PointerKind::Move, 200.0, 150.0),
                (PointerKind::Down, 200.0, 150.0),
                (PointerKind::Drag, 400.0, 300.0),
                (PointerKind::Up, 400.0, 300.0),
                (PointerKind::DoubleClick, 300.0, 200.0),
                (PointerKind::ContextClick, 300.0, 200.0),
            ] {
                let _ = s.pointer(PointerEvent { kind: k, x, y, mods: Default::default(), tol: 4.0, pressure: None });
                assert_eq!(fingerprint(&s), before, "a pointer tool changed the presentation");
            }
        }
    }

    #[test]
    fn the_same_drag_draws_a_text_box_in_an_editable_deck() {
        // The control for the pointer test: the gesture is one that edits when the deck is editable.
        use crate::tools::{PointerEvent, PointerKind, ToolKind};
        let mut s = deck();
        s.set_read_only(false);
        let before = fingerprint(&s);
        s.set_tool(ToolKind::TextBox);
        for (k, x, y) in [(PointerKind::Down, 200.0, 150.0), (PointerKind::Drag, 400.0, 300.0), (PointerKind::Up, 400.0, 300.0)] {
            s.pointer(PointerEvent { kind: k, x, y, mods: Default::default(), tol: 4.0, pressure: None }).unwrap();
        }
        assert_ne!(fingerprint(&s).0, before.0, "the drag edits an editable deck");
    }

    #[test]
    fn typing_and_input_methods_cannot_edit_a_view_only_deck() {
        let mut s = deck();
        let before = fingerprint(&s);
        let _ = s.type_text("abc");
        let _ = s.compose_text("あ", Some((0, 1)));
        let _ = s.commit_text("亜");
        for key in ["Backspace", "Delete", "Enter", "Tab", "Left", "Right"] {
            let _ = s.key(key, Default::default());
        }
        assert_eq!(fingerprint(&s), before);
    }
}
