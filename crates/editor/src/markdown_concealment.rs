use super::*;
use gpui::{FontStyle, FontWeight, StrikethroughStyle};
use language::Node;

/// Hides inline Markdown syntax (such as the `**` around bold text) unless a
/// selection touches the formatted text, so Markdown reads like rendered text
/// while remaining directly editable.
#[derive(Default)]
pub(crate) struct MarkdownConcealment {
    spans: Vec<ConcealableSpan>,
    computed_for: Option<(BufferId, clock::Global)>,
    syntax_changed: bool,
}

struct MarkdownCodeBlock;

const BULLET: SharedString = SharedString::new_static("•");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpanKind {
    Emphasis,
    StrongEmphasis,
    Strikethrough,
    Code,
    Link,
    ListMarker,
}

#[derive(Debug)]
struct ConcealableSpan {
    kind: SpanKind,
    /// Selections touching this range reveal the span's syntax.
    reveal_range: Range<usize>,
    content: Range<usize>,
    syntax: [Range<usize>; 2],
}

impl Editor {
    pub(crate) fn markdown_syntax_changed(&mut self, cx: &mut Context<Self>) {
        self.markdown_concealment.syntax_changed = true;
        self.refresh_markdown_concealment(cx);
    }

    pub(crate) fn refresh_markdown_concealment(&mut self, cx: &mut Context<Self>) {
        let multibuffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let buffer_snapshot = multibuffer_snapshot.as_singleton().filter(|buffer| {
            self.mode.is_full()
                && EditorSettings::get_global(cx).conceal_markdown_syntax
                && buffer
                    .language()
                    .is_some_and(|language| language.name() == "Markdown")
        });
        let Some(buffer_snapshot) = buffer_snapshot else {
            if self.markdown_concealment.computed_for.take().is_some() {
                self.markdown_concealment = MarkdownConcealment::default();
                self.clear_markdown_highlights(cx);
                self.display_map
                    .update(cx, |map, cx| map.set_concealments(Vec::new(), cx));
                cx.notify();
            }
            return;
        };

        let is_current =
            self.markdown_concealment
                .computed_for
                .as_ref()
                .is_some_and(|(buffer_id, version)| {
                    *buffer_id == buffer_snapshot.remote_id()
                        && version == buffer_snapshot.version()
                });
        let mut changed = false;
        if !is_current || self.markdown_concealment.syntax_changed {
            let (spans, code_blocks) = markdown_structure(buffer_snapshot);
            self.highlight_markdown_spans(&spans, &code_blocks, &multibuffer_snapshot, cx);
            self.markdown_concealment = MarkdownConcealment {
                spans,
                computed_for: Some((
                    buffer_snapshot.remote_id(),
                    buffer_snapshot.version().clone(),
                )),
                syntax_changed: false,
            };
            changed = true;
        }

        let selections = self
            .selections
            .disjoint_anchor_ranges()
            .chain(
                self.selections
                    .pending_anchor()
                    .map(|selection| selection.range()),
            )
            .map(|range| {
                let range = range.to_offset(&multibuffer_snapshot);
                range.start.0..range.end.0
            })
            .collect::<Vec<_>>();
        let concealments = self
            .markdown_concealment
            .spans
            .iter()
            .filter(|span| {
                !selections.iter().any(|selection| {
                    selection.start <= span.reveal_range.end
                        && span.reveal_range.start <= selection.end
                })
            })
            .flat_map(|span| {
                let replacement = (span.kind == SpanKind::ListMarker).then_some(BULLET);
                span.syntax.iter().map(move |range| Concealment {
                    range: MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                    replacement: replacement.clone(),
                })
            })
            .collect();

        changed |= self
            .display_map
            .update(cx, |map, cx| map.set_concealments(concealments, cx));
        if changed {
            cx.notify();
        }
    }

    fn highlight_markdown_spans(
        &mut self,
        spans: &[ConcealableSpan],
        code_blocks: &[Range<usize>],
        snapshot: &MultiBufferSnapshot,
        cx: &mut Context<Self>,
    ) {
        let to_anchors = |range: &Range<usize>| {
            snapshot.anchor_after(MultiBufferOffset(range.start))
                ..snapshot.anchor_before(MultiBufferOffset(range.end))
        };
        let content_ranges = |kind: SpanKind| {
            spans
                .iter()
                .filter(|span| span.kind == kind && !span.content.is_empty())
                .map(|span| to_anchors(&span.content))
                .collect::<Vec<_>>()
        };
        // List markers keep their existing syntax highlighting when revealed.
        let syntax_ranges = spans
            .iter()
            .filter(|span| span.kind != SpanKind::ListMarker)
            .flat_map(|span| span.syntax.iter())
            .filter(|range| !range.is_empty())
            .map(to_anchors)
            .collect::<Vec<_>>();

        self.clear_markdown_highlights(cx);
        let styled_ranges = [
            (
                HighlightKey::MarkdownSyntax,
                syntax_ranges,
                HighlightStyle {
                    color: Some(cx.theme().colors().text_muted),
                    ..Default::default()
                },
            ),
            (
                HighlightKey::MarkdownStrongEmphasis,
                content_ranges(SpanKind::StrongEmphasis),
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            ),
            (
                HighlightKey::MarkdownEmphasis,
                content_ranges(SpanKind::Emphasis),
                HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
            ),
            (
                HighlightKey::MarkdownStrikethrough,
                content_ranges(SpanKind::Strikethrough),
                HighlightStyle {
                    strikethrough: Some(StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    }),
                    ..Default::default()
                },
            ),
        ];
        for (key, ranges, style) in styled_ranges {
            if !ranges.is_empty() {
                self.highlight_text(key, ranges, style, cx);
            }
        }

        for code_block in code_blocks {
            self.highlight_rows::<MarkdownCodeBlock>(
                to_anchors(code_block),
                |cx| cx.theme().colors().editor_foreground.opacity(0.04),
                RowHighlightOptions {
                    include_gutter: false,
                    ..Default::default()
                },
                cx,
            );
        }
    }

    fn clear_markdown_highlights(&mut self, cx: &mut Context<Self>) {
        for key in [
            HighlightKey::MarkdownSyntax,
            HighlightKey::MarkdownStrongEmphasis,
            HighlightKey::MarkdownEmphasis,
            HighlightKey::MarkdownStrikethrough,
        ] {
            self.clear_highlights(key, cx);
        }
        self.clear_row_highlights::<MarkdownCodeBlock>();
    }
}

/// Returns the concealable spans and the code block ranges of a Markdown buffer.
fn markdown_structure(buffer: &BufferSnapshot) -> (Vec<ConcealableSpan>, Vec<Range<usize>>) {
    let mut spans = Vec::new();
    let mut code_blocks = Vec::new();
    for layer in buffer.syntax_layers() {
        let is_inline = match layer.language.name().as_ref() {
            "Markdown-Inline" => true,
            "Markdown" => false,
            _ => continue,
        };

        let mut cursor = layer.node().walk();
        'walk: loop {
            let node = cursor.node();
            if is_inline {
                spans.extend(inline_span(node));
            } else if matches!(node.kind(), "fenced_code_block" | "indented_code_block") {
                code_blocks.push(node.byte_range());
            } else {
                spans.extend(list_marker_span(node, buffer));
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    continue 'walk;
                }
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
    }

    // Markdown embedded in a code block (such as a ```markdown fence) is shown verbatim.
    spans.retain(|span| {
        !code_blocks.iter().any(|code_block| {
            code_block.start <= span.reveal_range.start && span.reveal_range.end <= code_block.end
        })
    });
    spans.sort_unstable_by_key(|span| (span.reveal_range.start, Reverse(span.reveal_range.end)));
    code_blocks.sort_unstable_by_key(|range| range.start);
    (spans, code_blocks)
}

fn list_marker_span(node: Node, buffer: &BufferSnapshot) -> Option<ConcealableSpan> {
    if !matches!(
        node.kind(),
        "list_marker_minus" | "list_marker_star" | "list_marker_plus"
    ) {
        return None;
    }
    // Task list items keep their marker so the checkbox syntax stays readable.
    if node.next_sibling().is_some_and(|sibling| {
        matches!(
            sibling.kind(),
            "task_list_marker_checked" | "task_list_marker_unchecked"
        )
    }) {
        return None;
    }

    let marker_start = node.start_byte();
    if !matches!(buffer.chars_at(marker_start).next(), Some('-' | '*' | '+')) {
        return None;
    }
    let row = buffer.offset_to_point(marker_start).row;
    let line_start = buffer.point_to_offset(Point::new(row, 0));
    let line_end = buffer.point_to_offset(Point::new(row, buffer.line_len(row)));
    Some(ConcealableSpan {
        kind: SpanKind::ListMarker,
        reveal_range: line_start..line_end,
        content: marker_start + 1..marker_start + 1,
        syntax: [
            marker_start..marker_start + 1,
            marker_start + 1..marker_start + 1,
        ],
    })
}

fn inline_span(node: Node) -> Option<ConcealableSpan> {
    let (kind, delimiter_kind) = match node.kind() {
        "emphasis" => (SpanKind::Emphasis, "emphasis_delimiter"),
        "strong_emphasis" => (SpanKind::StrongEmphasis, "emphasis_delimiter"),
        "strikethrough" => (SpanKind::Strikethrough, "emphasis_delimiter"),
        "code_span" => (SpanKind::Code, "code_span_delimiter"),
        "inline_link" | "full_reference_link" | "collapsed_reference_link" => {
            let mut cursor = node.walk();
            let link_text = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "link_text")?;
            let range = node.byte_range();
            return Some(ConcealableSpan {
                kind: SpanKind::Link,
                content: link_text.byte_range(),
                syntax: [
                    range.start..link_text.start_byte(),
                    link_text.end_byte()..range.end,
                ],
                reveal_range: range,
            });
        }
        _ => return None,
    };

    // Only conceal the delimiter runs that open and close the span; unmatched
    // delimiters inside it are literal text.
    let range = node.byte_range();
    let mut cursor = node.walk();
    let children = node.children(&mut cursor).collect::<Vec<_>>();
    let mut content_start = range.start;
    for child in &children {
        if child.kind() != delimiter_kind || child.start_byte() != content_start {
            break;
        }
        content_start = child.end_byte();
    }
    let mut content_end = range.end;
    for child in children.iter().rev() {
        if child.kind() != delimiter_kind
            || child.end_byte() != content_end
            || child.start_byte() < content_start
        {
            break;
        }
        content_end = child.start_byte();
    }
    if content_start == range.start || content_end == range.end {
        return None;
    }

    Some(ConcealableSpan {
        kind,
        content: content_start..content_end,
        syntax: [range.start..content_start, content_end..range.end],
        reveal_range: range,
    })
}

#[cfg(test)]
mod tests {
    use super::MarkdownCodeBlock;
    use crate::{editor_tests::init_test, test::editor_test_context::EditorTestContext};
    use gpui::{BorrowAppContext, TestAppContext};
    use language::{LanguageRegistry, Point};
    use multi_buffer::ToPoint as _;
    use std::sync::Arc;

    async fn markdown_test_context(cx: &mut TestAppContext) -> EditorTestContext {
        init_test(cx, |_| {});
        let mut cx = EditorTestContext::new(cx).await;
        let language_registry = Arc::new(LanguageRegistry::test(cx.executor()));
        language_registry.add(languages::language(
            "markdown-inline",
            tree_sitter_md::INLINE_LANGUAGE.into(),
        ));
        cx.update_buffer(|buffer, cx| {
            buffer.set_language_registry(language_registry);
            buffer.set_language(Some(language::markdown_lang()), cx);
        });
        cx
    }

    #[gpui::test]
    async fn test_markdown_list_markers_and_code_blocks(cx: &mut TestAppContext) {
        let mut cx = markdown_test_context(cx).await;

        cx.set_state(indoc::indoc! {"
            - one
              * nested **bold**
            - ˇtwo
            - [ ] task

            ```md
            - not a bullet
            ```
        "});
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            indoc::indoc! {"
                • one
                  • nested ⋯bold⋯
                - two
                - [ ] task

                ```md
                - not a bullet
                ```
            "}
        );

        let code_block_rows = cx.update_editor(|editor, _, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            editor
                .highlighted_rows::<MarkdownCodeBlock>(cx)
                .map(|(range, _)| range.start.to_point(&snapshot)..range.end.to_point(&snapshot))
                .collect::<Vec<_>>()
        });
        assert_eq!(code_block_rows, vec![Point::new(5, 0)..Point::new(8, 0)]);
    }

    #[gpui::test]
    async fn test_markdown_syntax_concealed_unless_selection_touches(cx: &mut TestAppContext) {
        let mut cx = markdown_test_context(cx).await;

        cx.set_state("ˇSome **bold** and *it* and `code` and [link](https://zed.dev).");
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            "Some ⋯bold⋯ and ⋯it⋯ and ⋯code⋯ and ⋯link⋯."
        );

        cx.set_state("Some **boˇld** and *it* and `code` and [link](https://zed.dev).");
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            "Some **bold** and ⋯it⋯ and ⋯code⋯ and ⋯link⋯."
        );

        cx.set_state("Some **bold**ˇ and *it* and `code` and [link](https://zed.dev).");
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            "Some **bold** and ⋯it⋯ and ⋯code⋯ and ⋯link⋯."
        );

        cx.set_state("Some **bold** and *it* and `code` and [liˇnk](https://zed.dev).");
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            "Some ⋯bold⋯ and ⋯it⋯ and ⋯code⋯ and [link](https://zed.dev)."
        );

        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.editor.conceal_markdown_syntax = Some(false);
                });
            });
        });
        cx.set_state("ˇSome **bold** and *it* and `code` and [link](https://zed.dev).");
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            "Some **bold** and *it* and `code` and [link](https://zed.dev)."
        );
    }
}
