use super::*;
use gpui::{FontStyle, FontWeight, StrikethroughStyle};
use language::Node;
use unicode_width::UnicodeWidthStr as _;

/// Hides inline Markdown syntax (such as the `**` around bold text) unless a
/// selection touches the formatted text, so Markdown reads like rendered text
/// while remaining directly editable.
#[derive(Default)]
pub(crate) struct MarkdownConcealment {
    spans: Vec<ConcealableSpan>,
    tables: Vec<MarkdownTable>,
    computed_for: Option<(BufferId, clock::Global)>,
    syntax_changed: bool,
    highlighted_table_column: Vec<Range<usize>>,
}

struct MarkdownCodeBlock;
struct MarkdownTableRows;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableRowKind {
    Header,
    Delimiter,
    Body,
}

#[derive(Debug)]
struct MarkdownTable {
    rows: Vec<TableRow>,
}

#[derive(Debug)]
struct TableRow {
    kind: TableRowKind,
    range: Range<usize>,
    pipes: Vec<usize>,
    /// The row's text split at its pipes, so there is one more segment than pipes.
    segments: Vec<Range<usize>>,
    leading_pipe: bool,
    trailing_pipe: bool,
}

impl MarkdownTable {
    fn range(&self) -> Range<usize> {
        match (self.rows.first(), self.rows.last()) {
            (Some(first), Some(last)) => first.range.start..last.range.end,
            _ => 0..0,
        }
    }

    fn row_containing(&self, offset: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.range.start <= offset && offset <= row.range.end)
    }
}

impl TableRow {
    fn cells(&self) -> &[Range<usize>] {
        let start = usize::from(self.leading_pipe);
        let end = self.segments.len() - usize::from(self.trailing_pipe);
        self.segments.get(start..end).unwrap_or_default()
    }

    /// The pipe following the cell at `cell_index`, if any.
    fn pipe_after_cell(&self, cell_index: usize) -> Option<usize> {
        self.pipes
            .get(cell_index + usize::from(self.leading_pipe))
            .copied()
    }

    fn cell_containing(&self, offset: usize) -> Option<usize> {
        self.cells()
            .iter()
            .position(|cell| cell.start <= offset && offset <= cell.end)
    }
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
            let (spans, code_blocks, tables) = markdown_structure(buffer_snapshot);
            self.highlight_markdown_spans(&spans, &code_blocks, &tables, &multibuffer_snapshot, cx);
            self.markdown_concealment = MarkdownConcealment {
                spans,
                tables,
                computed_for: Some((
                    buffer_snapshot.remote_id(),
                    buffer_snapshot.version().clone(),
                )),
                syntax_changed: false,
                highlighted_table_column: Vec::new(),
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
        let touches_selection = |range: &Range<usize>| {
            selections
                .iter()
                .any(|selection| selection.start <= range.end && range.start <= selection.end)
        };
        let mut concealments = self
            .markdown_concealment
            .spans
            .iter()
            .filter(|span| !touches_selection(&span.reveal_range))
            .flat_map(|span| {
                let replacement = (span.kind == SpanKind::ListMarker).then_some(BULLET);
                span.syntax.iter().map(move |range| Concealment {
                    range: MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                    replacement: replacement.clone(),
                    highlight_padding: false,
                })
            })
            .collect::<Vec<_>>();
        concealments.sort_unstable();

        let newest_head = self
            .selections
            .newest_anchor()
            .head()
            .to_offset(&multibuffer_snapshot)
            .0;
        let mut table_concealments = Vec::new();
        let mut highlighted_column = Vec::new();
        for table in &self.markdown_concealment.tables {
            let editing = touches_selection(&table.range());
            layout_table(
                table,
                editing,
                editing.then_some(newest_head),
                buffer_snapshot,
                &concealments,
                &mut table_concealments,
                &mut highlighted_column,
            );
        }
        concealments.extend(table_concealments);

        if highlighted_column != self.markdown_concealment.highlighted_table_column {
            if highlighted_column.is_empty() {
                self.clear_background_highlights(HighlightKey::MarkdownTableColumn, cx);
            } else {
                let ranges = highlighted_column
                    .iter()
                    .map(|range| {
                        multibuffer_snapshot.anchor_after(MultiBufferOffset(range.start))
                            ..multibuffer_snapshot.anchor_before(MultiBufferOffset(range.end))
                    })
                    .collect::<Vec<_>>();
                self.highlight_background(
                    HighlightKey::MarkdownTableColumn,
                    &ranges,
                    |_, theme| concealment_highlight_background(theme),
                    cx,
                );
            }
            self.markdown_concealment.highlighted_table_column = highlighted_column;
        }

        changed |= self
            .display_map
            .update(cx, |map, cx| map.set_concealments(concealments, cx));
        if changed {
            cx.notify();
        }
    }

    /// Moves to the next or previous Markdown table cell, appending a row when
    /// moving past the last cell. Returns whether the cursor was in a table.
    pub(crate) fn markdown_table_tab(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((table_index, row_index, head)) = self.markdown_table_at_cursor(cx) else {
            return false;
        };
        let table = &self.markdown_concealment.tables[table_index];
        let cells = table
            .rows
            .iter()
            .filter(|row| row.kind != TableRowKind::Delimiter)
            .flat_map(|row| row.cells().iter().cloned())
            .collect::<Vec<_>>();
        let row = &table.rows[row_index];
        let row_first_cell = table.rows[..row_index]
            .iter()
            .filter(|row| row.kind != TableRowKind::Delimiter)
            .map(|row| row.cells().len())
            .sum::<usize>();
        let row_cells = row.cells();
        let (previous, next) = if row.kind == TableRowKind::Delimiter {
            (row_first_cell.checked_sub(1), row_first_cell)
        } else if let Some(cell_index) = row.cell_containing(head) {
            (
                (row_first_cell + cell_index).checked_sub(1),
                row_first_cell + cell_index + 1,
            )
        } else if row_cells.first().is_some_and(|cell| head < cell.start) {
            (row_first_cell.checked_sub(1), row_first_cell)
        } else {
            (
                (row_first_cell + row_cells.len()).checked_sub(1),
                row_first_cell + row_cells.len(),
            )
        };

        let target = if forward { Some(next) } else { previous };
        match target.and_then(|target| cells.get(target)) {
            Some(cell) => {
                let multibuffer_snapshot = self.buffer.read(cx).snapshot(cx);
                let Some(buffer) = multibuffer_snapshot.as_singleton() else {
                    return false;
                };
                let selection = cell_content_range(cell, buffer);
                self.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([
                        MultiBufferOffset(selection.start)..MultiBufferOffset(selection.end)
                    ]);
                });
            }
            None if forward => {
                let last_row = table.rows.len() - 1;
                self.insert_markdown_table_row(table_index, last_row, window, cx);
            }
            None => {}
        }
        true
    }

    /// Adds a row below the current Markdown table row, or leaves the table when
    /// the cursor is on an empty last row. Returns whether the cursor was in a table.
    pub(crate) fn markdown_table_newline(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((table_index, row_index, head)) = self.markdown_table_at_cursor(cx) else {
            return false;
        };
        let multibuffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let Some(buffer) = multibuffer_snapshot.as_singleton() else {
            return false;
        };
        let table = &self.markdown_concealment.tables[table_index];
        let row = &table.rows[row_index];
        let selection = self
            .selections
            .newest_anchor()
            .range()
            .to_offset(&multibuffer_snapshot);
        // Splitting a row isn't meaningful, but a newline at the very start of
        // the table inserts a line above it as usual.
        if (row_index == 0 && head <= row.range.start)
            || selection.start.0 < row.range.start
            || selection.end.0 > row.range.end
        {
            return false;
        }

        let is_last_row = row_index + 1 == table.rows.len();
        if row.kind == TableRowKind::Body
            && is_last_row
            && row
                .cells()
                .iter()
                .all(|cell| cell_content_range(cell, buffer).is_empty())
        {
            let row_point = buffer.offset_to_point(row.range.start);
            let line_end =
                buffer.point_to_offset(Point::new(row_point.row, buffer.line_len(row_point.row)));
            let row_start = MultiBufferOffset(row.range.start);
            self.transact(window, cx, |this, window, cx| {
                this.edit([(row_start..MultiBufferOffset(line_end), "")], cx);
                this.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([row_start..row_start]);
                });
            });
            return true;
        }

        let insert_after = if row.kind == TableRowKind::Body {
            row_index
        } else {
            table
                .rows
                .iter()
                .position(|row| row.kind == TableRowKind::Delimiter)
                .unwrap_or(row_index)
        };
        self.insert_markdown_table_row(table_index, insert_after, window, cx);
        true
    }

    /// Returns the table, row and cursor offset when the editor's only cursor
    /// is in a Markdown table that can be edited.
    fn markdown_table_at_cursor(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(usize, usize, usize)> {
        if self.read_only(cx)
            || self.selections.count() != 1
            || self.selections.pending_anchor().is_some()
        {
            return None;
        }
        self.refresh_markdown_concealment(cx);
        self.markdown_concealment.computed_for.as_ref()?;
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let head = self
            .selections
            .newest_anchor()
            .head()
            .to_offset(&snapshot)
            .0;
        self.markdown_concealment
            .tables
            .iter()
            .enumerate()
            .find_map(|(table_index, table)| Some((table_index, table.row_containing(head)?, head)))
    }

    fn insert_markdown_table_row(
        &mut self,
        table_index: usize,
        after_row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multibuffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let Some(buffer) = multibuffer_snapshot.as_singleton() else {
            return;
        };
        let table = &self.markdown_concealment.tables[table_index];
        let (Some(header), Some(row)) = (table.rows.first(), table.rows.get(after_row)) else {
            return;
        };
        let row_point = buffer.offset_to_point(row.range.start);
        let line_start = buffer.point_to_offset(Point::new(row_point.row, 0));
        let line_end =
            buffer.point_to_offset(Point::new(row_point.row, buffer.line_len(row_point.row)));

        // Repeat whatever precedes the row on its line, such as a block quote marker.
        let mut text = String::from("\n");
        text.extend(buffer.text_for_range(line_start..row.range.start));
        if header.leading_pipe {
            text.push('|');
        }
        let cursor = MultiBufferOffset(line_end + text.len() + 1);
        text.push_str(&vec!["  "; header.cells().len().max(1)].join("|"));
        if header.trailing_pipe {
            text.push('|');
        }

        let insert_at = MultiBufferOffset(line_end);
        self.transact(window, cx, |this, window, cx| {
            this.edit([(insert_at..insert_at, text)], cx);
            this.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([cursor..cursor]);
            });
        });
    }

    fn highlight_markdown_spans(
        &mut self,
        spans: &[ConcealableSpan],
        code_blocks: &[Range<usize>],
        tables: &[MarkdownTable],
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
        let header_ranges = tables
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.kind == TableRowKind::Header)
            .flat_map(|row| row.cells())
            .filter(|cell| !cell.is_empty())
            .map(to_anchors)
            .collect::<Vec<_>>();
        let styled_ranges = [
            (
                HighlightKey::MarkdownTableHeader,
                header_ranges,
                HighlightStyle {
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                },
            ),
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
        for table in tables {
            self.highlight_rows::<MarkdownTableRows>(
                to_anchors(&table.range()),
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
            HighlightKey::MarkdownTableHeader,
            HighlightKey::MarkdownSyntax,
            HighlightKey::MarkdownStrongEmphasis,
            HighlightKey::MarkdownEmphasis,
            HighlightKey::MarkdownStrikethrough,
        ] {
            self.clear_highlights(key, cx);
        }
        self.clear_row_highlights::<MarkdownCodeBlock>();
        self.clear_row_highlights::<MarkdownTableRows>();
        self.clear_background_highlights(HighlightKey::MarkdownTableColumn, cx);
        self.markdown_concealment.highlighted_table_column.clear();
    }
}

/// Returns the concealable spans, code block ranges and tables of a Markdown buffer.
fn markdown_structure(
    buffer: &BufferSnapshot,
) -> (Vec<ConcealableSpan>, Vec<Range<usize>>, Vec<MarkdownTable>) {
    let mut spans = Vec::new();
    let mut code_blocks = Vec::new();
    let mut tables = Vec::new();
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
            } else if node.kind() == "pipe_table" {
                tables.extend(markdown_table(node, buffer));
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
    tables.retain(|table| {
        let range = table.range();
        !code_blocks
            .iter()
            .any(|code_block| code_block.start <= range.start && range.end <= code_block.end)
    });
    spans.sort_unstable_by_key(|span| (span.reveal_range.start, Reverse(span.reveal_range.end)));
    code_blocks.sort_unstable_by_key(|range| range.start);
    tables.sort_unstable_by_key(|table| table.range().start);
    (spans, code_blocks, tables)
}

/// Reads a table's rows from the text following its header. Rows are found
/// textually because tree-sitter-md fails to parse a table once one of its rows
/// has only empty cells, which is the case for every newly added row.
fn markdown_table(node: Node, buffer: &BufferSnapshot) -> Option<MarkdownTable> {
    let header_start = node.start_byte();
    let first_row = buffer.offset_to_point(header_start).row;
    let first_line_start = buffer.point_to_offset(Point::new(first_row, 0));
    // Text preceding the rows, such as a block quote marker, which each row repeats.
    let prefix = buffer
        .text_for_range(first_line_start..header_start)
        .collect::<String>();

    let mut rows = Vec::new();
    for row in first_row..=buffer.max_point().row {
        let line_start = buffer.point_to_offset(Point::new(row, 0));
        let line_end = buffer.point_to_offset(Point::new(row, buffer.line_len(row)));
        let line = buffer
            .text_for_range(line_start..line_end)
            .collect::<String>();
        let Some(content) = line.strip_prefix(prefix.as_str()) else {
            break;
        };
        let kind = match rows.len() {
            0 => TableRowKind::Header,
            1 => TableRowKind::Delimiter,
            _ => TableRowKind::Body,
        };
        let Some(row) = table_row(kind, line_start + prefix.len(), content) else {
            break;
        };
        rows.push(row);
    }

    let delimiter = rows.get(1)?;
    let delimiter_text = buffer
        .text_for_range(delimiter.range.clone())
        .collect::<String>();
    let is_delimiter = delimiter_text.contains('-')
        && delimiter_text
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' ' | '\t'));
    is_delimiter.then_some(MarkdownTable { rows })
}

fn table_row(kind: TableRowKind, row_start: usize, content: &str) -> Option<TableRow> {
    if content.trim().is_empty() {
        return None;
    }
    let mut pipes = Vec::new();
    let mut escaped = false;
    for (index, character) in content.char_indices() {
        if character == '|' && !escaped {
            pipes.push(row_start + index);
        }
        escaped = character == '\\' && !escaped;
    }
    if pipes.is_empty() {
        return None;
    }

    let range = row_start..row_start + content.len();
    let mut segments = Vec::with_capacity(pipes.len() + 1);
    let mut segment_start = range.start;
    for &pipe in &pipes {
        segments.push(segment_start..pipe);
        segment_start = pipe + 1;
    }
    segments.push(segment_start..range.end);

    let is_blank = |segment: &Range<usize>| {
        content[segment.start - row_start..segment.end - row_start]
            .trim()
            .is_empty()
    };
    let leading_pipe = segments.first().is_some_and(is_blank);
    let trailing_pipe =
        segments.len() > 1 + usize::from(leading_pipe) && segments.last().is_some_and(is_blank);
    Some(TableRow {
        kind,
        range,
        pipes,
        segments,
        leading_pipe,
        trailing_pipe,
    })
}

/// The cell's text without surrounding whitespace, or a position inside an empty cell.
fn cell_content_range(cell: &Range<usize>, buffer: &BufferSnapshot) -> Range<usize> {
    let text = buffer.text_for_range(cell.clone()).collect::<String>();
    let content_start = cell.start + (text.len() - text.trim_start().len());
    let content = text.trim();
    if content.is_empty() {
        let position = (cell.start + 1).min(cell.end);
        position..position
    } else {
        content_start..content_start + content.len()
    }
}

fn display_width(
    range: &Range<usize>,
    buffer: &BufferSnapshot,
    sorted_concealments: &[Concealment],
) -> usize {
    let text_width = |range: Range<usize>| -> usize {
        buffer
            .text_for_range(range)
            .map(|chunk| chunk.width())
            .sum()
    };
    let mut width = text_width(range.clone());
    let first =
        sorted_concealments.partition_point(|concealment| concealment.range.start.0 < range.start);
    for concealment in &sorted_concealments[first..] {
        if concealment.range.start.0 >= range.end {
            break;
        }
        if concealment.range.end.0 <= range.end {
            width = width.saturating_sub(text_width(
                concealment.range.start.0..concealment.range.end.0,
            ));
            width += concealment
                .replacement
                .as_ref()
                .map_or(0, |replacement| replacement.width());
        }
    }
    width
}

/// Aligns a table's columns by replacing each pipe with padding followed by the
/// pipe. Outside of editing, pipes and the delimiter row are drawn as box lines.
fn layout_table(
    table: &MarkdownTable,
    editing: bool,
    cursor: Option<usize>,
    buffer: &BufferSnapshot,
    sorted_concealments: &[Concealment],
    concealments: &mut Vec<Concealment>,
    highlighted_column: &mut Vec<Range<usize>>,
) {
    let cell_widths = table
        .rows
        .iter()
        .map(|row| {
            row.cells()
                .iter()
                .map(|cell| display_width(cell, buffer, sorted_concealments))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut column_widths = Vec::<usize>::new();
    for widths in &cell_widths {
        for (column, &width) in widths.iter().enumerate() {
            match column_widths.get_mut(column) {
                Some(column_width) => *column_width = (*column_width).max(width),
                None => column_widths.push(width),
            }
        }
    }

    let current_column = cursor.and_then(|cursor| {
        let row = &table.rows[table.row_containing(cursor)?];
        row.cell_containing(cursor)
    });
    let to_offsets =
        |range: Range<usize>| MultiBufferOffset(range.start)..MultiBufferOffset(range.end);

    for (row, widths) in table.rows.iter().zip(&cell_widths) {
        if !editing && row.kind == TableRowKind::Delimiter {
            let mut text = String::new();
            if row.leading_pipe {
                let leading_width = display_width(&row.segments[0], buffer, sorted_concealments);
                text.extend(std::iter::repeat_n(' ', leading_width));
                text.push('├');
            }
            for (column, _) in row.cells().iter().enumerate() {
                let width = column_widths.get(column).copied().unwrap_or_default();
                text.extend(std::iter::repeat_n('─', width));
                if row.pipe_after_cell(column).is_some() {
                    let is_last = column + 1 == row.cells().len();
                    text.push(if is_last { '┤' } else { '┼' });
                }
            }
            concealments.push(Concealment {
                range: to_offsets(row.range.clone()),
                replacement: Some(text.into()),
                highlight_padding: false,
            });
            continue;
        }

        let pipe_char = if editing { '|' } else { '│' };
        if row.leading_pipe {
            concealments.push(Concealment {
                range: to_offsets(row.pipes[0]..row.pipes[0] + 1),
                replacement: Some(pipe_char.to_string().into()),
                highlight_padding: false,
            });
        }
        for (column, cell) in row.cells().iter().enumerate() {
            let is_current_column = current_column == Some(column);
            if is_current_column {
                highlighted_column.push(cell.clone());
            }
            let Some(pipe) = row.pipe_after_cell(column) else {
                continue;
            };
            let padding = column_widths
                .get(column)
                .copied()
                .unwrap_or_default()
                .saturating_sub(widths.get(column).copied().unwrap_or_default());
            let padding_char = if row.kind == TableRowKind::Delimiter
                && buffer.reversed_chars_at(cell.end).next() == Some('-')
            {
                '-'
            } else {
                ' '
            };
            let mut text = String::with_capacity(padding + pipe_char.len_utf8());
            text.extend(std::iter::repeat_n(padding_char, padding));
            text.push(pipe_char);
            concealments.push(Concealment {
                range: to_offsets(pipe..pipe + 1),
                replacement: Some(text.into()),
                highlight_padding: is_current_column && padding > 0,
            });
        }
    }
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
    use crate::actions::{Backtab, Newline, Tab};
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

    #[gpui::test]
    async fn test_markdown_table_alignment(cx: &mut TestAppContext) {
        let mut cx = markdown_test_context(cx).await;

        cx.set_state(indoc::indoc! {"
            | a | bb |
            |---|:--|
            | ccc | d |

            ˇtext
        "});
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            indoc::indoc! {"
                │ a   │ bb │
                ├─────┼────┤
                │ ccc │ d  │

                text
            "}
        );

        cx.set_state(indoc::indoc! {"
            | a | bb |
            |---|:--|
            | cˇcc | d |

            text
        "});
        cx.run_until_parked();
        assert_eq!(
            cx.display_text(),
            indoc::indoc! {"
                | a   | bb |
                |-----|:---|
                | ccc | d  |

                text
            "}
        );
        let highlighted_column = cx.update_editor(|editor, _, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            editor
                .markdown_concealment
                .highlighted_table_column
                .iter()
                .map(|range| {
                    snapshot
                        .text_for_range(
                            multi_buffer::MultiBufferOffset(range.start)
                                ..multi_buffer::MultiBufferOffset(range.end),
                        )
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(highlighted_column, [" a ", "---", " ccc "]);
    }

    #[gpui::test]
    async fn test_markdown_table_tab_and_newline(cx: &mut TestAppContext) {
        let mut cx = markdown_test_context(cx).await;

        cx.set_state(indoc::indoc! {"
            | a | bb |
            |---|---|
            | cˇcc | d |
        "});
        cx.run_until_parked();
        cx.update_editor(|editor, window, cx| editor.tab(&Tab, window, cx));
        cx.assert_editor_state(indoc::indoc! {"
            | a | bb |
            |---|---|
            | ccc | «dˇ» |
        "});

        cx.update_editor(|editor, window, cx| editor.tab(&Tab, window, cx));
        cx.run_until_parked();
        cx.assert_editor_state(indoc::indoc! {"
            | a | bb |
            |---|---|
            | ccc | d |
            | ˇ |  |
        "});

        cx.update_editor(|editor, window, cx| editor.backtab(&Backtab, window, cx));
        cx.update_editor(|editor, window, cx| editor.backtab(&Backtab, window, cx));
        cx.update_editor(|editor, window, cx| editor.backtab(&Backtab, window, cx));
        cx.assert_editor_state(indoc::indoc! {"
            | a | «bbˇ» |
            |---|---|
            | ccc | d |
            |  |  |
        "});

        cx.update_editor(|editor, window, cx| editor.newline(&Newline, window, cx));
        cx.run_until_parked();
        cx.assert_editor_state(indoc::indoc! {"
            | a | bb |
            |---|---|
            | ˇ |  |
            | ccc | d |
            |  |  |
        "});

        cx.set_state(indoc::indoc! {"
            | a | bb |
            |---|---|
            | ccc | d |
            | ˇ |  |
        "});
        cx.run_until_parked();
        cx.update_editor(|editor, window, cx| editor.newline(&Newline, window, cx));
        cx.assert_editor_state(indoc::indoc! {"
            | a | bb |
            |---|---|
            | ccc | d |
            ˇ
        "});
    }
}
