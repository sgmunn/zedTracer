//! The text body of a Row Details field: long, wrappable, selectable, with coloured JSON
//! tokens and highlighted find matches.
//!
//! It is a read-only editor, which brings selection, copy, wrapping and scrolling for free.

use std::ops::Range;

use editor::Editor;
use editor::SelectionEffects;
use editor::display_map::HighlightKey;
use editor::scroll::Autoscroll;
use gpui::{
    AppContext as _, Context, Entity, FontStyle, FontWeight, HighlightStyle, Hsla, IntoElement,
    Render, Window, div, prelude::*,
};
use kusto_results::inspector::{InspectorDocument, JsonTokenKind, highlight_json};
use language::language_settings::SoftWrap;
use multi_buffer::MultiBufferOffset;

/// The editor is shared with the tokens that colour it; each token kind has its own key.
fn token_key(kind: JsonTokenKind) -> HighlightKey {
    HighlightKey::ConsoleAnsiHighlight(match kind {
        JsonTokenKind::Key => 0,
        JsonTokenKind::String => 1,
        JsonTokenKind::Number => 2,
        JsonTokenKind::Boolean => 3,
        JsonTokenKind::Null => 4,
    })
}

const HEADER_KEY: HighlightKey = HighlightKey::ConsoleAnsiHighlight(5);
const NULL_KEY: HighlightKey = HighlightKey::ConsoleAnsiHighlight(6);

/// The colours an inspector document is drawn with.
pub struct InspectorPalette {
    pub token: Box<dyn Fn(JsonTokenKind) -> Hsla>,
    pub header: Hsla,
    pub null: Hsla,
}

const TOKEN_KINDS: [JsonTokenKind; 5] = [
    JsonTokenKind::Key,
    JsonTokenKind::String,
    JsonTokenKind::Number,
    JsonTokenKind::Boolean,
    JsonTokenKind::Null,
];

pub struct InspectorText {
    editor: Entity<Editor>,
    wrap: bool,
}

impl InspectorText {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            editor.set_read_only(true);
            editor.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
            editor
        });
        Self { editor, wrap: true }
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    pub fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.wrap = wrap;
        let mode = if wrap {
            SoftWrap::EditorWidth
        } else {
            SoftWrap::None
        };
        self.editor
            .update(cx, |editor, cx| editor.set_soft_wrap_mode(mode, cx));
    }

    /// Shows `text`, colouring JSON tokens. Text and colour changes never touch the source.
    pub fn set_text(
        &mut self,
        text: &str,
        colours: impl Fn(JsonTokenKind) -> Hsla + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let document = InspectorDocument {
            text: text.to_string(),
            json_tokens: highlight_json(text),
            ..Default::default()
        };
        let palette = InspectorPalette {
            token: Box::new(colours),
            header: Hsla::default(),
            null: Hsla::default(),
        };
        self.set_document(&document, &palette, window, cx);
    }

    /// Shows a whole inspector document: its text, with field headers, nulls and JSON tokens
    /// styled. Styling never touches the text.
    pub fn set_document(
        &mut self,
        document: &InspectorDocument,
        palette: &InspectorPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.set_read_only(false);
            editor.set_text(document.text.as_str(), window, cx);
            editor.set_read_only(true);
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let anchors = |ranges: &mut dyn Iterator<Item = &Range<usize>>| {
                ranges
                    .map(|range| {
                        snapshot.anchor_after(MultiBufferOffset(range.start))
                            ..snapshot.anchor_before(MultiBufferOffset(range.end))
                    })
                    .collect::<Vec<_>>()
            };
            for kind in TOKEN_KINDS {
                let ranges = anchors(
                    &mut document
                        .json_tokens
                        .iter()
                        .filter(|token| token.kind == kind)
                        .map(|token| &token.range),
                );
                editor.highlight_text(
                    token_key(kind),
                    ranges,
                    HighlightStyle {
                        color: Some((palette.token)(kind)),
                        ..Default::default()
                    },
                    cx,
                );
            }
            editor.highlight_text(
                HEADER_KEY,
                anchors(&mut document.headers.iter()),
                HighlightStyle {
                    color: Some(palette.header),
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                },
                cx,
            );
            editor.highlight_text(
                NULL_KEY,
                anchors(&mut document.nulls.iter()),
                HighlightStyle {
                    color: Some(palette.null),
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
                cx,
            );
        });
    }

    /// Highlights every case-insensitive match of `query` and returns how many there are.
    ///
    /// The first match is selected and scrolled into view.
    pub fn find(
        &mut self,
        query: &str,
        colour: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let matches = find_matches(&self.editor.read(cx).text(cx), query);
        self.editor.update(cx, |editor, cx| {
            if matches.is_empty() {
                editor.clear_background_highlights(HighlightKey::BufferSearchHighlights, cx);
                return;
            }
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let ranges: Vec<_> = matches
                .iter()
                .map(|range| {
                    snapshot.anchor_after(MultiBufferOffset(range.start))
                        ..snapshot.anchor_before(MultiBufferOffset(range.end))
                })
                .collect();
            editor.highlight_background(
                HighlightKey::BufferSearchHighlights,
                &ranges,
                move |_, _| colour,
                cx,
            );
            if let Some(first) = matches.first() {
                editor.change_selections(
                    SelectionEffects::scroll(Autoscroll::center()),
                    window,
                    cx,
                    |selections| {
                        selections.select_ranges([
                            MultiBufferOffset(first.start)..MultiBufferOffset(first.end)
                        ])
                    },
                );
            }
        });
        matches.len()
    }
}

impl Render for InspectorText {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.editor.clone())
    }
}

/// Byte ranges of every non-overlapping, case-insensitive occurrence of `query`.
pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let lower_text = text.to_lowercase();
    let lower_query = query.to_lowercase();
    // Lower-casing can change byte lengths for a few characters; only trust the offsets when
    // it did not.
    if lower_text.len() != text.len() {
        return Vec::new();
    }
    lower_text
        .match_indices(&lower_query)
        .map(|(start, found)| start..start + found.len())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use gpui::{Hsla, TestAppContext, VisualTestContext, hsla, px, size};
    use kusto_results::inspector::format_json_for_display;
    use workspace::AppState;

    use super::*;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
        });
    }

    fn colour(kind: JsonTokenKind) -> Hsla {
        hsla(kind as usize as f32 / 10., 0.8, 0.5, 1.)
    }

    fn sample_json(entries: usize) -> String {
        let value = serde_json::json!({
            "type": "AggregateException",
            "callStack": (0..entries).map(|index| format!("at App.Step{index}() in File{index}.cs:line {index}")).collect::<Vec<_>>().join("\n"),
            "items": (0..entries).map(|index| serde_json::json!({"id": index, "ok": index % 2 == 0, "note": null})).collect::<Vec<_>>(),
        });
        format_json_for_display(&value)
    }

    fn open(cx: &mut TestAppContext) -> (Entity<InspectorText>, &mut VisualTestContext) {
        init_test(cx);
        let (view, cx) = cx.add_window_view(|window, cx| InspectorText::new(window, cx));
        cx.simulate_resize(size(px(420.), px(700.)));
        (view, cx)
    }

    #[gpui::test]
    async fn shows_the_text_read_only_and_keeps_it_selectable(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let text = sample_json(5);
        view.update_in(cx, |view, window, cx| {
            view.set_text(&text, colour, window, cx)
        });
        cx.run_until_parked();
        let editor = view.read_with(cx, |view, _| view.editor().clone());
        editor.update_in(cx, |editor, window, cx| {
            assert_eq!(
                editor.text(cx),
                text,
                "the editor holds exactly the display text"
            );
            assert!(editor.read_only(cx));
            editor.select_all(&editor::actions::SelectAll, window, cx);
        });
        let selected = editor.update_in(cx, |editor, _, cx| {
            editor
                .selections
                .all::<MultiBufferOffset>(&editor.display_snapshot(cx))
                .iter()
                .map(|selection| selection.end.0 - selection.start.0)
                .sum::<usize>()
        });
        assert_eq!(selected, text.len());
    }

    #[gpui::test]
    async fn token_colours_and_find_highlights_are_applied(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let text = sample_json(5);
        view.update_in(cx, |view, window, cx| {
            view.set_text(&text, colour, window, cx)
        });
        let matches = view.update_in(cx, |view, window, cx| {
            view.find("CALLSTACK", hsla(0.1, 1., 0.5, 0.4), window, cx)
        });
        assert_eq!(matches, 1);
        let editor = view.read_with(cx, |view, _| view.editor().clone());
        editor.update(cx, |editor, cx| {
            for kind in TOKEN_KINDS {
                let (style, ranges) = editor
                    .text_highlights(token_key(kind), cx)
                    .unwrap_or_else(|| panic!("no highlights for {kind:?}"));
                assert_eq!(style.color, Some(colour(kind)));
                assert!(!ranges.is_empty(), "{kind:?}");
            }
        });
        assert_eq!(find_matches("Timeout TIMEOUT timeout", "timeout").len(), 3);
        assert!(find_matches("abc", "").is_empty());
    }

    #[gpui::test]
    async fn wrapping_can_be_turned_off(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let long_line = format!("{{\"message\": \"{}\"}}", "word ".repeat(200));
        view.update_in(cx, |view, window, cx| {
            view.set_text(&long_line, colour, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let editor = view.read_with(cx, |view, _| view.editor().clone());
        let rows_wrapped = editor.update_in(cx, |editor, window, cx| {
            editor.snapshot(window, cx).max_point().row().0
        });
        view.update(cx, |view, cx| view.set_wrap(false, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let rows_unwrapped = editor.update_in(cx, |editor, window, cx| {
            editor.snapshot(window, cx).max_point().row().0
        });
        assert!(view.read_with(cx, |view, _| !view.wrap()));
        assert!(
            rows_wrapped > rows_unwrapped,
            "wrapped {rows_wrapped} display rows, unwrapped {rows_unwrapped}"
        );
    }

    /// A 25 KB payload (the largest real value in the samples) and a 1 MB stress value.
    #[gpui::test]
    #[ignore = "benchmark"]
    async fn large_values_stay_responsive(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        for entries in [300, 12_000] {
            let text = sample_json(entries);
            let started = Instant::now();
            view.update_in(cx, |view, window, cx| {
                view.set_text(&text, colour, window, cx)
            });
            let set_in = started.elapsed();
            let started = Instant::now();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let first_draw = started.elapsed();
            let started = Instant::now();
            let matches = view.update_in(cx, |view, window, cx| {
                view.find("id", hsla(0.1, 1., 0.5, 0.4), window, cx)
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            println!(
                "{:>8} bytes: set text {:.1} ms, first frame {:.1} ms, find ({matches} matches) and redraw {:.1} ms",
                text.len(),
                set_in.as_secs_f64() * 1000.0,
                first_draw.as_secs_f64() * 1000.0,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let _ = Arc::new(());
    }
}
