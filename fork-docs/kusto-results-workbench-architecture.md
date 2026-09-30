# Kusto Results Workbench for Zed: architecture (part 1)

Status: draft. The spikes in section 6 have run; their results are in [part 2](kusto-results-workbench-architecture-2-spikes.md). Companion to [the design](kusto-results-workbench-design.md) and [the feature spec](kusto-results-workbench-feature-spec.md). This is the first of two architecture notes. It records what is **already decided and built** (the UI-free core crate) and the structure the UI is expected to take. The second note, written after the phase A spikes, records the UI decisions the spikes force. Nothing here depends on a spike's outcome unless it says so.

## 1. Approach

1. Put everything that does not need a window into one crate, `kusto_results`, tested against the fixtures in `fork-docs/samples/`. It is built and passing.
2. Build the UI in separate crates on top of it. Their shape depends on the spikes in section 6.
3. Keep the dependency arrow one way: UI crates depend on the core, never the reverse. The core has no `gpui` dependency, so its tests run in milliseconds and could serve a different front end.

## 2. Crates

```
                 +---------------------------+   reads .ktt / .kqr files
                 |  kusto_results   (built)  |<---------------------------+
                 |  model, file format,      |                            |
                 |  view logic, inspector    |                            |
                 |  logic, export            |                            |
                 +------------^--------------+                            |
                              |                                           |
            +-----------------+------------------+                        |
            |                                    |                        |
  +---------+-----------+            +-----------+----------+    +--------+--------+
  | results UI (planned)|            | execution (later)    |    | Zed project     |
  | grid, panels, tabs, |            | .NET process client, |    | (file opening,  |
  | structured view,    |            | produces ResultSet   |    | settings, docks)|
  | Row Details panel   |            +----------------------+    +-----------------+
  +---------------------+
```

| Crate | Status | Owns | Depends on |
| --- | --- | --- | --- |
| `kusto_results` | Built; 76 unit tests, 12 fixture tests and 1 on-demand scale test; clippy clean | Result model; `.ktt` reading and writing; sort, filter, search, selection and column order; activity projection and severity; call stack, JSON and multipart logic; Copy text forms | `serde`, `serde_json`, `regex`, `anyhow` |
| Results UI | Planned (one crate to start, split later only if it grows) | The grid element, results panel, result tab, structured view, Row Details panel, settings, actions and key bindings | `kusto_results`, `gpui`, `ui`, `workspace`, `project`, settings |
| Execution | Later; out of scope here | Running a query and delivering a `ResultSet` | `kusto_results` |

The results UI reuses the existing registration pattern from `crates/investigation` (a `Panel` for the docks and a project item for files). Whether it also reuses `tabular_data_preview` or the `ui` table is spike S1.

## 3. The core crate

| Module | Responsibility | Main entry points |
| --- | --- | --- |
| `result` | Model and file format. Typed cells that keep values exactly (64-bit integers, decimals as text, dynamic key order). Reads through raw JSON so unknown properties (charts) are written back verbatim. | `ResultSet::from_json`, `to_json`, `Table`, `Cell`, `Column`, `ColumnKind`, `TableView` |
| `typed` | Parsing and ordering by Kusto type: datetime and timespan to 100 ns ticks, exact decimals, natural string order. | `parse_datetime_ticks`, `parse_timespan_ticks`, `Decimal`, `NumberKey`, `natural_cmp` |
| `filter` | Operators per column type, conditions, joins, matching rules (spec FLT-4 to FLT-8). | `operators_for`, `ColumnFilter`, `Condition`, `Join` |
| `view` | The pipeline that turns a table and view state into rows to show; three-state sort clicks; rectangular selection; saved column order; severity tint rule. | `visible_rows`, `ViewState`, `SortState`, `page_range`, `CellSelection`, `display_column_order`, `severity_level` |
| `activity` | Structured view: grouping, parents, anomaly repair, cycle breaking, depth, branch sizes, severity outcome, Deepest. | `build_projection`, `ActivityProjection`, `Activity` |
| `inspector` | Row Details content: subject resolution, JSON detection and pretty printing, JSON token spans, call stack trimming, multipart assembly. | `resolve_subject`, `assemble_multipart`, `format_call_stack`, `highlight_json`, `field_value` |
| `export` | Copy forms: tab-separated, Markdown, HTML, single-cell raw text. | `copy_text`, `tsv`, `markdown`, `html` |

### Properties the core guarantees

- **Source data is never modified.** A view is a list of source row indexes. Sorting, filtering and scoping reorder or drop entries from that list; values, row numbers and ties are untouched.
- **Display logic is separate from data.** Formatting for the inspector returns new text; it is never written back, and `export` always writes source values.
- **Everything is a plain function of its inputs.** No global state, no I/O apart from the two file-format entry points, no UI types.
- **Long work can be abandoned.** `visible_rows` takes a `keep_going` check, called every 4,096 rows, and returns `None` when it turns false.
- **No row is lost.** Hierarchy repair (orphan, conflicting parents, cycles, missing ids) marks activities instead of dropping rows; the projection always contains every source row.
- **Malformed files give an error, not a partial result.** A row with the wrong number of cells, or a file without tables, fails with a message that names the table, row and column.

### Decisions made in the core

| # | Decision | Why |
| --- | --- | --- |
| AD-1 | Cells are a typed enum (`Null`, `Bool`, `Int`, `Real`, `Decimal`, `Text`, `Dynamic`), not `serde_json::Value`. | Typed comparison and exact numbers (spec NFR-9). Also 32 bytes per cell instead of 72 (measured), which matters at 4 million cells. |
| AD-2 | A number keeps the form it had in the file: `5` reads as an integer, `5.0` as a real, a decimal keeps its digits as text. | Lossless round trip when a layout change rewrites a saved result (PER-6). |
| AD-3 | The file is read through raw JSON for each cell and top-level property. | Exact numbers, verbatim unknown properties, and no intermediate value tree for 200,000 rows. |
| AD-4 | Sort keys are derived per column type at sort time; nothing is cached on the table. | Keeps the model immutable and shareable; measured cost is acceptable (section 5). |
| AD-5 | Null sorts smallest, unreadable values sort last in both directions, ties break by source row in both directions. | Spec SRT-3, SRT-6. The last-in-both-directions choice extends SRT-6, which did not say. |
| AD-6 | Selection is a rectangle of visible positions in the core, resolved to source rows and original column indexes for the inspector and Copy. | One definition shared by grid, inspector and export. |
| AD-7 | The call stack formatter shortens source paths before treating `\n` escape sequences as breaks, and only treats an escape as a break when it precedes a frame or ends the text. | Fixes the VS Code path corruption (spec section 7, row 2) without losing real line breaks. |
| AD-8 | Activity hierarchy repair is iterative, not recursive. | A 50,000-deep chain is covered by a test; a recursive version overflows the stack. |
| AD-9 | The fixed regular expressions are built in one helper that panics if a constant pattern were invalid. | They are compile-time constants exercised by the first call stack test. This is the crate's only intentional panic site; every other failure is returned as an error. |

## 4. The UI structure (provisional)

This is the expected shape, to be confirmed or changed by the spikes. It describes responsibilities and message flow, not types.

### Ownership

```
ResultDocument  (one per run or per opened .ktt file)
   owns: Arc<ResultSet>, run identity, save path
   |
   +-- TableView  (one per table tab, one per structured tab)
   |      owns: ViewState (search, filters, sort), page, column layout,
   |            CellSelection, current visible rows (Arc<[usize]>), busy state
   |
   +-- publishes: "selection changed" (table identity + selected source rows)

RowDetailsPanel  (one per window)
   subscribes to the most recently active TableView's selection
   resolves it with kusto_results::inspector, renders it
```

- Results, badge, tabs, selection and inspector content belong to the document that produced them. A late result from a cancelled or replaced run never touches another document (spec RUN-5).
- The panel and result tab are two placements of the same document and table views, so panel and tab cannot drift apart.
- The inspector never reads from a grid directly. It receives a selection event and looks the rows up in the document's `ResultSet`.
- The structured tab is a second `TableView` over the same `ResultSet` with the selected activity's event rows as its scope. Rows are shared, not copied.

### Recompute flow

```
user action (type in search, change filter, click header)
   -> update ViewState, bump a generation number, show busy after 250 ms
   -> background task: clone (Arc<ResultSet>, ViewState), call visible_rows
        keep_going = "generation is still the latest"
   -> when done on the foreground: if the generation is still latest,
        store the rows, clear selection, reset to page 1, redraw
        otherwise drop the result
```

The `keep_going` check is needed even though GPUI cancels a task that is dropped: dropping stops a task at its next await, but the sort and filter loops are synchronous and would run to the end.

### What the UI crate adds that the core does not have

Theme colours and the severity palette; settings (page size, severity colours, result location); actions and key bindings; clipboard access; drawing, hit testing and scrolling; the filter popover, tab bars and splitter; the mapping from selection to the inspector panel.

## 5. Measured baseline

The spec asks for baselines on named hardware before numeric targets are set (NFR-1). These are for the **core only** (no drawing), release build, from `cargo test -p kusto_results --release --test sample_fixtures -- --ignored --nocapture` on the generated 200,000-row by 20-column file (230 MB).

Hardware: Apple M5 Pro, 48 GB, macOS 27.0.1.

| Operation | Time |
| --- | --- |
| Parse the file | 330 ms |
| Sort a `long` column | 11 ms |
| Sort a `datetime` column | 37 ms |
| Sort a `timespan` column | 46 ms |
| Sort a string column (natural order) | 250 to 290 ms |
| Filter `Level <= 3` (12,192 rows left) | 16 ms |
| Search `timeout retry` (107,985 rows left) | 190 ms |
| Activity projection (16,666 activities, depth 153) | 52 ms |
| Write the file | 170 ms |
| Peak memory while loading | 1.08 GB |
| Size of one cell | 32 bytes |

What this says:

- Sorting, filtering and projection are far inside an interactive budget even before any optimisation, in release mode. Debug builds are much slower, so developers should measure in release.
- **Memory is the pressure point.** 1.08 GB peaks while loading a 230 MB file, because the file text, the borrowed cell list and the finished cells coexist. After loading, the cost is the cells (about 130 MB) plus their strings. Options, in order of effort: drop the file text right after parsing (already done in the baseline); read the file in a streaming fashion; store repeated strings once. None is needed before real use shows a problem.
- **Search and string sort are the slowest operations** (about 190 to 290 ms for 200,000 rows), because they lower-case text on every call. A cached lower-case text per cell, or a per-column cache, would cut that. Worth doing only if the UI spike shows typing in the search box feels slow.
- Real drawing, scrolling and the clipboard are not included; those are what the spikes measure.

## 6. Spikes (decide the UI half)

Each spike is a small experiment with a yes or no answer, run against the fixtures. Do S1 first, because it may remove work from S2 to S5.

| # | Question | Builds | Passes when | If it fails |
| --- | --- | --- | --- | --- |
| S1 | Reuse `tabular_data_preview` and the `ui` table, or build the grid? | Open `synthetic-types.ktt` and the 200,000-row file in a prototype using each | Scrolls at 60 fps with 20 columns; header can carry sort, funnel and resize as separate targets; rows can be selected as rectangles; typed values can be supplied without converting to strings | Build a results grid element on the `ui` table's pieces or from scratch; keep `tabular_data_preview` for CSV |
| S2 | Can a header cell carry independent click targets (sort, funnel, resize, drag-reorder)? | A header with all four on one column | Each target fires alone; a resize drag never triggers a sort | Move the funnel to a header menu |
| S3 | Rectangular cell selection with drag, auto-scroll and shift-extend | Selection over the prototype grid | Matches spec SEL-1 to SEL-5a; `CellSelection` maps to the right source rows after a sort | Row-only selection first, cell ranges later |
| S4 | Long, wrapped, selectable text with coloured runs and find highlights | Row Details body for the multipart and call stack fixtures | Wrap on and off; select and copy; `highlight_json` spans coloured; find highlights drawn across coloured runs; 25 KB value scrolls smoothly | Plain text colour segments; find over the raw text |
| S5 | Clipboard and popover | Copy of a selection as text; an anchored filter popover with a select and a text input | Text copies correctly on macOS; popover stays on screen and closes on Escape and outside click | Rich HTML copy deferred (CPY-11); popover replaced by a header menu |
| S6 | Right dock coexistence | A Row Details panel beside the Investigation panel prototype | Both dock and persist; the panel follows the selection from a panel grid and an editor-tab grid | Place Row Details inside the results surface |

Output of the spikes: the second architecture note, recording which option won and why.

## 7. Testing approach

- **Core:** unit tests per module, plus `tests/sample_fixtures.rs`, which loads every committed fixture and compares the crate with the expected-results files (activities, severity, multipart, call stacks, sorts, filters, round trip). Adding a case means adding it to the generator and the expected file.
- **Real captures:** when the git-ignored real files are present locally, the same suite loads them as a smoke test.
- **Scale:** the ignored `large_fixture_baseline` test records the timings above.
- **UI (later):** GPUI's test support for state and event flow (filter change leads to the right rows and a cleared selection; a stale generation is dropped), and Computer Use in the running app for gestures and drawing.
- **Parity:** differences from VS Code are intentional and listed in spec section 7; the fixture notes say which expected results differ and why.

## 8. Known gaps in the core

| Gap | Spec | Plan |
| --- | --- | --- |
| Noise frame list cannot be extended by the user | EXC-9 (P1) | Accept an extra list as a parameter when settings exist. |
| A `dynamic` value with an integer above 64 bits is read as a floating point number | NFR-9 | Only affects numbers inside dynamic objects; decimals in `decimal` columns are exact. Revisit if real data needs it. |
| `Copy as datatable` | CPY-6 | Needs a KQL expression generator; VS Code gets it from its language server. Capture vectors from VS Code output first. |
| Search and string sort allocate lower-case text per cell | SRC, SRT | Cache if the UI shows lag. |
| Sorting cannot be interrupted once it starts comparing | NFR-2 | Key building and filtering check `keep_going`; the final comparison sort does not. It is tens of milliseconds at 200,000 rows. |
| History, settings, severity palette defaults, `.ktt` path handling | PER-5, SET | Belong to the UI and Zed project layer. |
| Indexing inside `activity` uses indexes it built itself | n/a | In bounds by construction; covered by tests including a 50,000-deep chain. |

## 9. Next steps

1. Run S1 (reuse or build) and record the answer.
2. In parallel, run S2 to S6 against the core crate's public API.
3. Write architecture part 2 from the results, then create the results UI crate.
4. Turn the remaining spec P0 items into the phase A backlog: viewer for `.ktt`, typed grid with gutter, selection, Copy, context menu.
