//! What the host asked of one opened presentation (`readOnly`, `chrome`, `zoomControls` of the
//! `open` message) and the app commands a view-only app still runs. The engine's side is
//! `deckcraft_engine::readonly`: it refuses every edit and every save. This side hides what is
//! left of the editor and refuses the app commands that open an editing dialog or pane.
//!
//! Every app command is in exactly one of [`UI_OK`] and [`UI_REFUSED`]; the drift test fails until
//! a new one is classified, and an unclassified one is refused.

/// How one opened presentation is shown. The default is the editor as it always was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewOptions {
    /// Nothing edits the presentation and nothing is saved.
    pub read_only: bool,
    /// The ribbon, title bar, status bar, task panes, notes pane and ruler. Off leaves the slide,
    /// the thumbnails and (with `zoom_controls`) zoom buttons.
    pub chrome: bool,
    /// Zoom buttons over the slide; only drawn when `chrome` is off (the status bar has the slider).
    pub zoom_controls: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        ViewOptions { read_only: false, chrome: true, zoom_controls: false }
    }
}

/// App commands a view-only app runs: they only change what is shown, start the show, or search.
pub const UI_OK: &[&str] = &[
    "animation.preview",
    "app.about",
    "app.backstage",
    "app.find",
    "app.print",
    "show.end",
    "show.presenter",
    "show.start",
    "view.dark",
    "view.fit",
    "view.grayscale",
    "view.gridlines",
    "view.normal",
    "view.reading",
    "view.ruler",
    "view.sorter",
    "view.zoom",
    "view.zoomDialog",
    "view.zoomIn",
    "view.zoomOut",
];

/// App commands a view-only app refuses: dialogs and panes that edit, files, printing, settings.
pub const UI_REFUSED: &[&str] = &[
    "app.browseThemeDialog",
    "app.changePictureDialog",
    "app.closePresentation",
    "app.compareDialog",
    "app.dialog",
    "app.exportDialog",
    "app.home",
    "app.insertAudioDialog",
    "app.insertPictureDialog",
    "app.insertVideoDialog",
    "app.links",
    "app.openDialog",
    "app.palette",
    "app.pasteSpecial",
    "app.preferences",
    "app.qat",
    "app.replace",
    "app.reuseSlidesDialog",
    "app.saveAsDialog",
    "app.smartArtPictureDialog",
    "review.thesaurusPane",
    "view.collapseRibbon",
    "view.gridOptions",
    "view.guides",
    "view.newWindow",
    "view.notes",
    "view.notesPage",
    "view.outline",
    "view.pane",
    "view.tab",
];

/// May a view-only app run the app command `id`? An id nobody classified is refused.
pub fn permits(id: &str) -> bool {
    UI_OK.binary_search(&id).is_ok()
}

/// Is `id` an app command a view-only app refuses?
pub fn refused(id: &str) -> bool {
    UI_REFUSED.binary_search(&id).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_command_is_classified_for_view_only() {
        for (id, ..) in crate::UI_COMMANDS {
            let n = [UI_OK.contains(id), UI_REFUSED.contains(id)].iter().filter(|b| **b).count();
            assert_eq!(n, 1, "app command `{id}` must be in exactly one of view_only::UI_OK / UI_REFUSED");
        }
        for list in [UI_OK, UI_REFUSED] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "keep the lists sorted and free of duplicates");
            for id in list {
                assert!(crate::UI_COMMANDS.iter().any(|c| c.0 == *id), "`{id}` is classified but is not an app command");
            }
        }
    }

    #[test]
    fn no_command_that_opens_or_saves_a_file_is_ok() {
        // Printing follows the host's policy, which the lock checks after this; the File tab a
        // read-only app shows has only the Print page (see `SlideApp::open_backstage`).
        for id in UI_OK.iter().filter(|id| !matches!(**id, "app.print" | "app.backstage")) {
            assert!(!matches!(crate::host_lock::grade(id), Some(deckcraft_engine::hostlock::Grade::Locked | deckcraft_engine::hostlock::Grade::Off)), "`{id}` is a file command");
        }
    }

    #[test]
    fn the_default_is_the_editor_as_it_was() {
        let v = ViewOptions::default();
        assert!(!v.read_only && v.chrome && !v.zoom_controls);
    }
}
