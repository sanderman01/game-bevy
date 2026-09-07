//! The Console tab: captured log events, one line per event, selectable and copyable.
//!
//! The buffer is `ename_engine::log::LogBuffer`, which the engine fills whether or not the
//! editor is linked in. This module only draws it.

use std::{borrow::Cow, collections::BTreeSet, fmt::Write as _};

use bevy::{ecs::world::World, log::Level};
use egui::{Align2, Color32, FontId, Key, Modifiers, Rect, Sense, TextStyle, Visuals, pos2, vec2};
use ename_engine::log::{LogBuffer, LogEntryRef, LogView};

/// Character columns each field starts at. Monospace, so a column is a multiple of one glyph
/// width and the eye can scan a field straight down the page.
const TIMESTAMP_END: f32 = 9.;
const LEVEL_START: f32 = 11.;
const TARGET_START: f32 = 17.;
const TARGET_WIDTH: usize = 24;
const MESSAGE_START: f32 = 42.;

/// Drawn in place of a newline. A real one would lay the entry out as two rows and break the
/// uniform row height `ScrollArea::show_rows` needs. The buffer keeps the true text, so a copied
/// backtrace still pastes with its line breaks.
const NEWLINE_GLYPH: char = '⏎';

pub(crate) struct ConsoleState {
    /// Selected rows by sequence number, not by index: the ring drops from the front, so an index
    /// would slide onto a different row as the buffer wraps. Ordered, so a copy comes out
    /// chronological.
    selected: BTreeSet<u64>,
    /// Where a shift-click measures its range from.
    anchor: Option<u64>,
    /// The row the detail pane shows: the last one clicked, whatever the modifiers did to the
    /// selection around it.
    detail: Option<u64>,
    /// Stick to the newest entry until the user scrolls away.
    follow_tail: bool,
    /// Whether the keyboard shortcuts are this panel's. Tracked here rather than through egui's
    /// focus ring because the console is not a widget, and a press outside it has to hand Ctrl+C
    /// back to the inspector's text fields.
    focused: bool,
}

impl Default for ConsoleState {
    fn default() -> Self {
        Self {
            selected: BTreeSet::new(),
            anchor: None,
            detail: None,
            follow_tail: true,
            focused: false,
        }
    }
}

pub(crate) fn ui(ui: &mut egui::Ui, state: &mut ConsoleState, world: &World) {
    let Some(buffer) = world.get_resource::<LogBuffer>().cloned() else {
        ui.label("No log buffer. `EnginePlugins` installs one.");
        return;
    };

    // `LogBuffer::read` holds the lock for the whole closure, so `Clear` runs after it rather
    // than deadlocking against it.
    let mut clear = false;

    buffer.read(|view| {
        ui.horizontal(|ui| {
            ui.label(format!("{} entries", view.len()));
            if !state.selected.is_empty() {
                ui.label(format!("{} selected", state.selected.len()));
            }
            ui.checkbox(&mut state.follow_tail, "Follow");
            clear = ui.button("Clear").clicked();
        });
        ui.separator();

        // Added before the list, so what it takes is gone from the list's available space.
        detail(ui, state, &view);

        // Focus is the list's, not the whole tab's. A press in the detail pane hands Ctrl+C to
        // the text there, which does its own selection.
        let list = ui.available_rect_before_wrap();
        if ui.input(|i| i.pointer.any_pressed()) {
            state.focused = ui.rect_contains_pointer(list);
        }
        shortcuts(ui, state, &view);
        rows(ui, state, &view);
    });

    if clear {
        buffer.clear();
        state.selected.clear();
        state.anchor = None;
        state.detail = None;
    }
}

/// The full text of one entry, wrapped and selectable, under a resizable splitter.
///
/// The list flattens a message onto one row and clips it at the panel edge, so this is the only
/// place a long message or a backtrace can be read whole.
fn detail(ui: &mut egui::Ui, state: &ConsoleState, view: &LogView<'_>) {
    let line = ui.text_style_height(&TextStyle::Monospace);
    egui::Panel::bottom(ui.id().with("detail"))
        .resizable(true)
        .default_size(line * 3.5)
        .min_size(line)
        .show_inside(ui, |ui| {
            let Some(entry) = state.detail.and_then(|sequence| view.by_sequence(sequence)) else {
                ui.label("Select an entry.");
                return;
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "{:>9.3}  {:<5}  {}\n{}",
                                entry.timestamp,
                                entry.level.as_str(),
                                entry.target_name,
                                entry.message
                            ))
                            .monospace(),
                        )
                        .wrap()
                        .selectable(true),
                    );
                });
        });
}

fn shortcuts(ui: &egui::Ui, state: &mut ConsoleState, view: &LogView<'_>) {
    if !state.focused {
        return;
    }
    let (select_all, copy) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::COMMAND, Key::A),
            i.consume_key(Modifiers::COMMAND, Key::C),
        )
    });

    if select_all && let Some((oldest, newest)) = view.sequence_range() {
        state.selected = (oldest..=newest).collect();
        state.anchor = Some(oldest);
    }
    if copy {
        let text = clipboard_text(state, view);
        if !text.is_empty() {
            ui.ctx().copy_text(text);
        }
    }
}

/// Builds the clipboard out of the buffer rather than out of what was drawn.
///
/// egui's own cross-label selection accumulates the clipboard while it draws, which would drop
/// every selected row that is scrolled out of view. Reading by sequence is right at any scroll
/// position. A sequence that has aged out of the buffer is skipped.
fn clipboard_text(state: &ConsoleState, view: &LogView<'_>) -> String {
    let mut text = String::new();
    for sequence in &state.selected {
        let Some(entry) = view.by_sequence(*sequence) else {
            continue;
        };
        let _ = writeln!(
            text,
            "{:>9.3}  {:<5}  {}  {}",
            entry.timestamp,
            entry.level.as_str(),
            entry.target_name,
            entry.message
        );
    }
    text
}

/// Fixed row geometry, measured once per frame.
struct Columns {
    font: FontId,
    char_width: f32,
    row_height: f32,
}

fn rows(ui: &mut egui::Ui, state: &mut ConsoleState, view: &LogView<'_>) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let char_width = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font, ' '));
    let row_height = ui.text_style_height(&TextStyle::Monospace);
    let columns = Columns {
        font,
        char_width,
        row_height,
    };

    ui.spacing_mut().item_spacing.y = 0.;
    // Vertical only. A row is exactly as wide as the panel and the painter clips what runs past
    // the right edge, so a long message costs no horizontal scrollbar. The detail pane below is
    // where such a message is read in full.
    let output = egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .stick_to_bottom(state.follow_tail)
        .show_rows(ui, row_height, view.len(), |ui, visible| {
            for index in visible {
                let Some(entry) = view.get(index) else {
                    continue;
                };
                row(ui, state, entry, &columns);
            }
        });

    // One click target for the whole list, not one per row. egui lays a frame out twice when a
    // size changes, the two passes scroll to different places, and a row's rect then belongs to
    // a different row each pass. Registering a widget per row makes that egui's
    // "changed id between passes" warning; hit-testing the pointer against the scroll offset
    // sidesteps it and costs one widget instead of fifty.
    let clicked = ui.interact(output.inner_rect, ui.id().with("rows"), Sense::click());
    if clicked.clicked()
        && let Some(pointer) = clicked.interact_pointer_pos()
        && let Some((oldest, newest)) = view.sequence_range()
    {
        let y = pointer.y - output.inner_rect.top() + output.state.offset.y;
        let index = (y / row_height) as usize;
        if let Some(entry) = view.get(index) {
            let modifiers = ui.input(|i| i.modifiers);
            click(state, entry.sequence, modifiers, oldest, newest);
        }
    }
}

fn row(ui: &mut egui::Ui, state: &ConsoleState, entry: LogEntryRef<'_>, columns: &Columns) {
    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), columns.row_height));
    if !ui.is_rect_visible(rect) {
        return;
    }
    if state.selected.contains(&entry.sequence) {
        ui.painter()
            .rect_filled(rect, 0., ui.visuals().selection.bg_fill);
    }
    paint(ui, rect, entry, columns);
}

fn paint(ui: &egui::Ui, rect: Rect, entry: LogEntryRef<'_>, columns: &Columns) {
    let painter = ui.painter();
    let visuals = ui.visuals();
    let colour = level_colour(visuals, entry.level);
    let at = |column: f32| pos2(rect.left() + column * columns.char_width, rect.center().y);

    painter.text(
        at(TIMESTAMP_END),
        Align2::RIGHT_CENTER,
        format!("{:.3}", entry.timestamp),
        columns.font.clone(),
        visuals.weak_text_color(),
    );
    painter.text(
        at(LEVEL_START),
        Align2::LEFT_CENTER,
        entry.level.as_str(),
        columns.font.clone(),
        colour,
    );
    painter.text(
        at(TARGET_START),
        Align2::LEFT_CENTER,
        truncate_left(entry.target_name, TARGET_WIDTH),
        columns.font.clone(),
        visuals.weak_text_color(),
    );
    painter.text(
        at(MESSAGE_START),
        Align2::LEFT_CENTER,
        flatten(entry.message),
        columns.font.clone(),
        colour,
    );
}

fn level_colour(visuals: &Visuals, level: Level) -> Color32 {
    if level == Level::ERROR {
        visuals.error_fg_color
    } else if level == Level::WARN {
        visuals.warn_fg_color
    } else if level == Level::INFO {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    }
}

/// Keeps the tail of a module path, which is the part that identifies it.
fn truncate_left(text: &str, width: usize) -> Cow<'_, str> {
    let count = text.chars().count();
    if count <= width {
        return Cow::Borrowed(text);
    }
    let tail: String = text.chars().skip(count - width + 1).collect();
    Cow::Owned(format!("…{tail}"))
}

fn flatten(message: &str) -> Cow<'_, str> {
    if message.contains(['\n', '\r']) {
        Cow::Owned(
            message
                .replace('\r', "")
                .replace('\n', &NEWLINE_GLYPH.to_string()),
        )
    } else {
        Cow::Borrowed(message)
    }
}

/// Applies one click to the selection. Separate from drawing so it can be tested without a
/// buffer or an egui context.
fn click(state: &mut ConsoleState, sequence: u64, modifiers: Modifiers, oldest: u64, newest: u64) {
    state.detail = Some(sequence);
    if modifiers.command {
        if !state.selected.remove(&sequence) {
            state.selected.insert(sequence);
        }
        return;
    }
    if modifiers.shift
        && let Some(anchor) = state.anchor
    {
        let (from, to) = if anchor <= sequence {
            (anchor, sequence)
        } else {
            (sequence, anchor)
        };
        // Clamped, because the anchor can have aged out of the buffer since it was set.
        state.selected = (from.max(oldest)..=to.min(newest)).collect();
        return;
    }
    state.selected.clear();
    state.selected.insert(sequence);
    state.anchor = Some(sequence);
}

#[cfg(test)]
mod tests {
    use super::{ConsoleState, click, flatten, truncate_left};
    use egui::Modifiers;

    #[test]
    fn a_plain_click_replaces_the_selection_and_moves_the_anchor() {
        let mut state = ConsoleState::default();
        click(&mut state, 5, Modifiers::NONE, 0, 10);
        click(&mut state, 7, Modifiers::NONE, 0, 10);

        assert_eq!(state.selected.iter().copied().collect::<Vec<_>>(), [7]);
        assert_eq!(state.anchor, Some(7));
        assert_eq!(state.detail, Some(7));
    }

    #[test]
    fn a_shift_click_range_is_clamped_to_what_the_buffer_still_holds() {
        let mut state = ConsoleState::default();
        click(&mut state, 2, Modifiers::NONE, 0, 10);
        // Entries 0..=3 have since aged out.
        click(&mut state, 6, Modifiers::SHIFT, 4, 10);

        assert_eq!(
            state.selected.iter().copied().collect::<Vec<_>>(),
            [4, 5, 6]
        );
    }

    #[test]
    fn a_shift_click_backwards_selects_the_same_range() {
        let mut state = ConsoleState::default();
        click(&mut state, 8, Modifiers::NONE, 0, 10);
        click(&mut state, 6, Modifiers::SHIFT, 0, 10);

        assert_eq!(
            state.selected.iter().copied().collect::<Vec<_>>(),
            [6, 7, 8]
        );
    }

    #[test]
    fn a_ctrl_click_toggles_one_row_and_leaves_the_anchor_alone() {
        let mut state = ConsoleState::default();
        click(&mut state, 3, Modifiers::NONE, 0, 10);
        click(&mut state, 9, Modifiers::COMMAND, 0, 10);
        assert_eq!(state.selected.iter().copied().collect::<Vec<_>>(), [3, 9]);
        assert_eq!(state.anchor, Some(3));

        click(&mut state, 9, Modifiers::COMMAND, 0, 10);
        assert_eq!(state.selected.iter().copied().collect::<Vec<_>>(), [3]);
        assert_eq!(state.anchor, Some(3));
        // The detail pane follows the click, not the anchor.
        assert_eq!(state.detail, Some(9));
    }

    #[test]
    fn a_module_path_keeps_its_tail() {
        assert_eq!(truncate_left("bevy::render", 24), "bevy::render");
        assert_eq!(truncate_left("abcdefghij", 5), "…ghij");
    }

    #[test]
    fn a_multiline_message_draws_as_one_row() {
        assert_eq!(flatten("one\r\ntwo"), "one⏎two");
        assert_eq!(flatten("plain"), "plain");
    }
}
