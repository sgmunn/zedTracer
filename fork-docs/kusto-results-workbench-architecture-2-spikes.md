# Kusto Results Workbench for Zed: architecture (part 2, spike results)

Status: all six spikes done. Records what the phase A spikes found and which UI decisions they settle. Part 1 is [kusto-results-workbench-architecture.md](kusto-results-workbench-architecture.md); the spike plan is its section 6.

All spikes ran in the new `crates/kusto_results_ui` crate, headless, using GPUI's test platform (real layout, real event dispatch, no GPU). That proves behaviour and measures CPU cost; it does not show pixels. Every result below comes from a test that you can re-run:

```sh
cargo test -p kusto_results_ui --profile release-fast --lib
cargo test -p kusto_results_ui --profile release-fast --lib -- --ignored --nocapture frame_time
```

`release-fast` is the repo's optimised profile without link-time optimisation, so it builds in minutes.

## Summary

| Spike | Question | Answer |
| --- | --- | --- |
| S1 | Reuse `tabular_data_preview` and `ui::Table`, or build the grid? | **Build on `ui::Table`. Do not build on `tabular_data_preview`.** |
| S2 | Can a header carry independent click targets? | **Yes**, including drag-to-reorder. One gap: `ui::Table`'s resize handles are in the body only, so header-edge resize is ours (about 40 lines, no change to `ui`). |
| S3 | Rectangular cell selection | **Yes**: click, shift+click, drag, gutter row select, clear on sort, publishes source rows. Auto-scroll during a drag and keyboard navigation are not yet spiked. |
| S4 | Long, wrapped, selectable, coloured text | **Yes, with a read-only `Editor`.** Selection, copy, wrap on and off, token colours and find highlights all work; 1.3 MB of text is set in 30 ms. One small upstream question: the editor's highlight keys are a closed list. |
| S5 | Clipboard and popover | **Yes.** Text copy works. A `PopoverMenu` with our own view filters as you type, closes on Escape, on an outside click and on Clear, and stays on screen. **Rich HTML copy is not possible** with today's clipboard type. |
| S6 | Right dock coexistence | **Yes.** Row Details docks beside the Investigation panel, each shows in turn, and it follows the selection a grid publishes. |

## S1. Reuse or build

### The two candidates

**`crates/tabular_data_preview`** (upstream CSV, TSV and JSON Lines preview).

- Every cell is a `TableCell` holding two text anchors into an editor buffer plus a cached string. Its content is parsed from an open `Editor`, and the preview pane is a view of that editor.
- Its engine works on strings: sort compares the displayed text (with a TODO for nulls), so numbers sort as text, which is the likely cause of the reversed-CSV observation in `zed-kusto-design.md`.
- It precomputes the list of distinct values for every column when content loads, to feed its filter menus. For 200,000 rows by 20 columns that is 20 large hash sets built up front.
- It has no cell selection. The code marks the place for mouse handlers with a comment, and offers right-click copy of one cell.
- What it does well: its header (label, funnel popover trigger, sort button, shown on hover with a fade) and its distinct-value checklist popover are exactly the patterns we want for the header and for the P2 "value picker" (spec FLT-13).

**`ui::Table`** (the shared table element both it and other Zed views use).

- Virtualised: rows come from a closure that is given the visible range and returns elements for those rows only.
- Cells and headers are arbitrary elements, so typed values can be rendered straight from `kusto_results::Cell` with no intermediate strings table.
- Pinned columns (the row-number gutter), independently resizable columns with horizontal scroll, column visibility mask, scrollbars, an empty-table callback.
- Has no sort, filter, selection or reorder of its own, which is what we want from a layout element: those live in our grid and in `kusto_results`.

### Measurements

A `ResultGrid` built on `ui::Table` with our typed rows, the row-number gutter pinned, resizable columns, and the three header controls. Headless frame cost (build elements, lay out, paint into the scene) on the 200,000-row by 20-column fixture, 1600 by 900 window, Apple M5 Pro, `release-fast`:

| Measure | Result |
| --- | --- |
| First frame | 4.7 ms |
| Scrolling, 300 frames, each a jump of 613 rows | p50 5.0 ms, p95 6.2 ms, max 8.6 ms |
| Redraw with no scrolling | p50 4.5 ms |
| Rows built per frame | 32 for a 900 px window (a test also asserts fewer than 200 for a 100,000-row table) |
| Sort a `long` column, end to end (background task, result applied) | 25 ms |
| Sort a `datetime` column | 47 ms |
| Sort a long string column (`MessageText`, natural order) | 286 ms |
| Search two words, 200,000 rows | 202 ms |
| First frame after a sort | 4.4 to 4.6 ms |

Run-to-run variation is about 10 to 15 percent; a run with a build going on in the background was slower (scrolling p50 4.7 ms, text sort 321 ms), so measure on a quiet machine.

The frame budget at 60 fps is 16.7 ms, so the CPU side has roughly three times headroom, and the cost does not depend on the number of rows. Not measured: GPU rasterisation and presentation (needs a real window), and scrollbar drag feel.

### Decision

**Build the grid as a `ResultGrid` view on `ui::Table`.** Reuse from `tabular_data_preview` the header composition pattern and, later, its distinct-value picker; do not depend on the crate. It keeps `tabular_data_preview` free to keep serving CSV, and it keeps our typed model (`Cell`, `ColumnKind`) end to end.

Consequences:

- Spec Q-14 is answered: no reuse of its engine or view.
- No changes to `ui` or `tabular_data_preview` are needed so far.
- The grid owns: view state, sort and filter recomputation in the background, selection, column order and widths, and everything drawn in the header.

## S2. Header targets

A header cell holds: the column name (click sorts, drag reorders), a sort button, a funnel button, and a right-edge handle (drag resizes).

Tests that pass:

- Clicking the name sorts once (ascending), again (descending), again (back to original order). The sort button does the same as the name. The funnel opens the filter and does not sort.
- Dragging a header onto another reorders columns, and the dragged column's width goes with it. A drag is never treated as a click.
- Dragging the header's right edge resizes the column by the distance dragged, stops at a 48 px minimum, leaves the neighbours alone, and never sorts.

**Finding.** `ui::Table` draws its resize dividers as an overlay over the body only. A drag starting in the header's band (about the top 35 px) misses them, which is the opposite of what spreadsheets and the spec (COL-2) expect. Two ways to close the gap:

1. **Our own header handle (chosen, implemented).** A small absolutely-positioned element on each header with its own drag payload, and a drag-move handler on the grid that calls the table's public `set_column_configuration`. About 40 lines, no change to `ui`.
2. A small change to `ui::Table` to extend the existing overlay over the header. Cleaner, but changes a shared crate.

The body dividers still work too (tested), so dragging near a column edge anywhere works.

One limitation of the width setter: `set_column_configuration` also resets that column's "initial width", so the table's built-in double-click-to-reset returns to the new width. The grid should track the original widths itself for a proper reset (spec COL-5).

## S3. Selection

The grid holds a `CellSelection` from `kusto_results` (a rectangle of visible positions) and publishes `SelectionChanged { rows }` with source row indexes in display order.

Tests that pass:

- Click selects a cell. Shift+click extends the rectangle. Click and drag selects a rectangle, and moving the mouse after the button is released does not extend it.
- Clicking a gutter cell selects the row; shift+click on another gutter cell extends to a row range.
- After the user sorts, the selection is cleared and an empty selection is published (spec SEL-6). Before that, the published rows are source rows: selecting the first visible row of a descending sort reports row 199, not 0.
- Selected cells are highlighted with the theme's selection colour.

Not yet spiked, with the expected approach:

| Item | Approach |
| --- | --- |
| Auto-scroll while dragging near an edge | Drag-move handler on the grid root compares the pointer with the table bounds and calls the scroll handle. The scroll handle API (`scroll_to_item`, offsets) is available. |
| Keyboard selection and Ctrl/Cmd+C | Key context and actions on the grid, reading `CellSelection` and `kusto_results::export`. |
| Header and corner selection (spec SEL-4, SEL-5) | `CellSelection::columns` and `everything` already exist in the core; only the gestures remain. |
| Non-contiguous row selection (SEL-11) | Extends the model from one rectangle to a row set plus column range. |

## S4. Long text in the inspector

**Approach tested: a read-only `Editor` holding the display text.** The text comes from `kusto_results::inspector` (pretty JSON, trimmed call stacks, assembled multipart), and the colours come from its `highlight_json` spans.

Tests that pass:

- The editor holds exactly the display text and is read-only. Select all covers all of it, so copy works as in any editor.
- The five token kinds (property names, strings, numbers, booleans, null) each carry their own colour. Find matches are drawn as background highlights and the match count is returned.
- Turning wrap off reduces a long line from many display rows to one, and back.

Measured, headless, `release-fast`:

| Value size | Set text and colour | First frame | Find (every match) and redraw |
| --- | --- | --- | --- |
| 32 KB (larger than the largest real value, 25 KB) | 1.3 ms | 0.4 ms | 0.8 ms (300 matches) |
| 1.3 MB | 30 ms | 0.3 ms | 4.4 ms (12,000 matches) |

An editor virtualises its rows, so a very long value costs what it shows. The first-frame figures are small partly because soft wrapping is computed in the background; the wrap test waits for it.

Why an editor, and what it costs:

| | |
| --- | --- |
| Gains | Selection, copy, wrapping, scrolling, find highlighting and a scrollbar with no text layout code of our own. Selecting across several fields works. |
| Cost | Every editor has settings, gutter, minimap and keymap behaviour to switch off. Its dependency tree is large, although Zed already builds it. |
| Gap | `HighlightKey` is a closed list of variants. The spike colours tokens under `ConsoleAnsiHighlight(0..4)` and find matches under `BufferSearchHighlights`, which names things it is not. Options: accept that, or add one variant such as `ResultsToken(usize)` to the editor crate (a one-line change in a shared crate). |

**Decision (recommended):** show the whole inspector as **one read-only editor**, with each field as a header line (name and type, coloured through highlights) followed by its value. That keeps selection, find and wrap uniform across all fields, instead of one editor per field (heavy with 27 fields) or selectable labels (GPUI labels are not selectable). The cost is that the boxed JSON blocks in wireframe W-5 become header lines plus a tinted background range; that is a visual detail to settle with a prototype in a real window.

Not yet spiked: Ctrl/Cmd+F focusing a find box inside the panel (needs a key context), the `Wrap lines` and find controls in the header, dimmed italic `null`, and how the editor looks in the right dock.

## S5. Clipboard and popover

**Clipboard.** Tests pass for the text forms:

- One selected cell copies as its bare value; a larger selection copies as tab-separated text with a header row; no selection copies the whole table (spec CPY intro, CPY-1).
- Rich HTML is not possible today. The clipboard item type holds only text, images and file paths (`ClipboardEntry::{String, Image, ExternalPaths}`), so spec CPY-11 (P1) needs platform work, exactly as predicted. Text-only copy is the P0 and is done.

**Filter popover.** A `PopoverMenu` around a `FilterPopover` view (operator dropdown, value input, Clear).

- Clicking the funnel opens it, anchored under the funnel, and focus goes to the value input.
- Typing `message 17 ` filters the grid live to one row (the sort and filter run through the same background recompute as sort, with stale results dropped).
- Escape closes it and keeps the filter. A click anywhere else closes it. Clear removes the filter and closes it.
- For the first column, where a popover anchored at the funnel's right edge is wider than the space on its left, the window keeps it on screen.

**Finding.** `PopoverMenu` dismisses only when its own trigger is clicked. Closing on a click elsewhere is the popover view's job: it needs an `on_mouse_down_out` handler. (`ContextMenu` and pickers have it built in; a custom view does not.)

Not yet spiked: choosing a different operator from the dropdown, the second condition and the match-all or match-any selector, and the 150 ms debounce (the spike filters on every keystroke and relies on cancelling stale work, which is cheap at the measured speeds).

## S6. Right dock coexistence

A `RowDetailsPanel` (a `Panel` positioned on the right, with a toggle action) was added to a test workspace together with the existing `InvestigationPanel`.

Tests that pass:

- The right dock holds both panels. Focusing each in turn shows it as the dock's visible panel, in either order.
- The panel follows a shared `ActiveSelection`. Each `ResultGrid` publishes its selection (table and source rows) to it whenever the selection changes, so the inspector follows whichever grid was used last, whether that grid is in the bottom panel or an editor tab. The panel's text matches the wireframes: the empty message, `Row N · M fields`, `N selected rows · multi-part message assembled` with the merged field, and `N rows selected - showing the first`.

Not yet spiked: panel position and size persisting across restarts (the panel uses `panel_key`, but the test workspace does not exercise the persistence database), key bindings for the toggle action, and registering in the `zed` binary. None is expected to be a problem; the Investigation prototype already does all three.

## Decisions these spikes settle

| # | Decision | Basis |
| --- | --- | --- |
| UI-1 | The grid is a `ResultGrid` on `ui::Table`, with our own view state, background recompute, selection and header. `tabular_data_preview` is not a dependency. | S1, S2, S3 |
| UI-2 | Header: name (click sorts, drag reorders), sort button, funnel (popover), and our own right-edge resize handle. No change to `ui`. | S2 |
| UI-3 | Sort, filter and search changes bump a generation number and run in a background task; results for an older generation are dropped; the selection clears when new rows arrive. | S1, S3, S5 |
| UI-4 | The grid publishes its selection to one shared state; Row Details is a right-dock panel that follows it. | S6 |
| UI-5 | **Decided.** Row Details renders the whole inspector as one read-only editor, with a header line per field. Token colours keep borrowing the editor's existing highlight keys (`ConsoleAnsiHighlight(0..4)`, `BufferSearchHighlights`); no change to the editor crate for now. Revisit if the borrowing causes a clash. | S4 |
| UI-6 | The filter popover is a `PopoverMenu` with our own view, which handles outside-click dismissal itself. | S5 |
| UI-7 | Text copy is P0 and works. Rich HTML copy stays P1 and needs clipboard platform work. | S5 |

## What the spikes did not prove

- **Real rendering.** Everything ran on GPUI's test platform: real layout and event dispatch, but no GPU. Frame cost is the CPU side only. The next step is to host `ResultGrid` in a workspace item and open a real file in the `zed` binary, then look at it.
- **Theme and look.** Colours, the selection tint, the header's hover behaviour and the inspector's appearance have not been looked at.
- **Interaction feel.** Scrollbar dragging, auto-scroll during a drag selection, keyboard navigation, and drag-and-drop previews.
- **A `.ktt` project item and a bottom-dock Results panel.** Only the grid, inspector text, popover and Row Details panel exist, each in isolation.

## Next steps

1. Wire the pieces into the `zed` binary behind the same pattern as `investigation`: a `.ktt` project item hosting `ResultGrid`, and the Row Details panel, then open `synthetic-types.ktt` and `synthetic-trace-edge.ktt` and review them by eye.
2. Replace the inspector placeholder with the one-editor renderer using `kusto_results::inspector`.
3. Fill in the not-yet-spiked gestures (auto-scroll, keyboard, header and corner selection) as part of phase A rather than as more spikes.
4. UI-5 is decided (see the table above).
