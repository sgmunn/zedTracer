# Kusto Results Workbench: status and handoff

Read this first in a new session. Last updated at the end of the phase A spikes. Nothing from this work has been committed except what the user committed themselves; `git status` shows the rest.

## Goal

Bring the results side of the KustoTraceTools VS Code fork (`/Users/gregm/Projects/Kusto-Explorer-VsCode`, branch `dev/gregm`) into the user's Zed fork (this repo): typed results grid, sorting, filtering, search, Row Details inspector (JSON, exception call stacks, multipart messages), structured activity view, results panel. Charts and `render` are out of scope for now. Execution (running queries) comes later.

## Documents (all in `fork-docs/`)

| File | What it is |
| --- | --- |
| `zed-kusto-design.md` | Earlier handoff: fork boundary, .NET LSP idea, staged plan. |
| `kusto-results-workbench-feature-spec.md` | Functional requirements with IDs (GRD, SEL, SRT, FLT, RDT, MPM, EXC, ACT, ...), acceptance vectors, section 7 (deliberate differences from VS Code), section 8 (questions, all decided except Q-3 check and Q-14 outcome). |
| `kusto-results-workbench-design.md` | Goals, conceptual model, wireframes W-1..W-12, delivery phases A to F. |
| `kusto-results-workbench-architecture.md` | Part 1: crate split, core crate contents, decisions AD-1..AD-9, measured baseline, known gaps. |
| `kusto-results-workbench-architecture-2-spikes.md` | Part 2: spike results S1..S6 and UI decisions UI-1..UI-7. |
| `samples/README.md` | Fixtures and expected-results files; `generate_samples.py` regenerates them. |

## Code

- `crates/kusto_results` (built, tested, clippy clean): UI-free core. Modules `result` (model, `.ktt` read/write, lossless numbers), `typed`, `filter`, `view` (visible rows, sort, selection, column order, severity), `activity`, `inspector`, `export`. 76 unit tests plus `tests/sample_fixtures.rs` (12 tests, checks every expected-results file; 1 ignored scale test).
- `crates/kusto_results_ui` (spike code, tests pass, clippy clean): `ResultGrid` on `ui::Table` (`grid.rs`), `FilterPopover`, `InspectorText` (read-only editor), `RowDetailsPanel` plus shared `ActiveSelection` (`row_details_panel.rs`). Registered in the `zed` binary (`results_viewer.rs`: `.ktt`/`.kqr` project item and `ResultsViewer` tab; `RowDetailsPanel::load` in `initialize_panels`). `cargo check -p zed` and a headless open-fixture test pass; nobody has seen it in a real window yet.
- Workspace changes: `Cargo.toml` members and dependencies for both crates; `Cargo.lock`.
- Samples: `sample1.ktt` and `sample2.ktt` are real telemetry, git-ignored, never commit. `generated/` (200,000-row file) is git-ignored; make it with `python3 fork-docs/samples/generate_samples.py --large`.

## Commands

```sh
cargo test -p kusto_results
cargo test -p kusto_results --release --test sample_fixtures -- --ignored --nocapture   # core scale baseline
cargo test -p kusto_results_ui --profile release-fast --lib
cargo test -p kusto_results_ui --profile release-fast --lib -- --ignored --nocapture frame_time
./script/clippy -p kusto_results -p kusto_results_ui
```

Notes: `--offline` fails on this workspace (missing index entry), so build online. First build of `kusto_results_ui` tests takes many minutes (editor, workspace, project); use `release-fast`. Bash `sleep` is blocked: run long jobs with `run_in_background` and wait for the notification.

## Decisions taken

- Grid on `ui::Table`; do not use `tabular_data_preview` (UI-1). Header: own right-edge resize handle, no change to `ui` (UI-2). Sort, filter, search run in a background task with a generation number (UI-3).
- Row Details is a right-dock panel following one shared `ActiveSelection` (UI-4). The inspector is one read-only editor with a header line per field; keep borrowing existing editor highlight keys (UI-5). Filter popover is a `PopoverMenu` with its own view and its own outside-click handling (UI-6). Text copy is P0; rich HTML copy is P1 and needs clipboard platform work (UI-7).
- Spec decisions: one call-stack frame per line (Q-13), natural string sort ignoring case but not punctuation (Q-12), whole-view column and table selection (Q-11), non-contiguous row selection added as SEL-11 (Q-4), filters not persisted (Q-6), keep `.ktt` (Q-10).

## Working rules for this repo

Follow `CLAUDE.md` at the repo root: no `unwrap()` in non-test code, no `let _ =` on fallible calls, no `mod.rs`, crate lib path in `Cargo.toml`, full-word names, comments only for why, use `./script/clippy`. The repo `README.md` already carries the required `> [!IMPORTANT]` lines; never remove them. The only intentional panic site is the regex helper in `inspector.rs`.

## After first look in a real window

- Works: opening `.ktt`, the grid, Row Details. Added in response: Ctrl/Cmd+click on row numbers for non-contiguous row selection (SEL-11, in `view::toggle_row`; the rectangle stays for shift-click, the other rows are an added set), and a footer with `N rows` or `M of N rows` plus `Row N selected` or `K rows selected` (`ResultGrid::status_text`).
- Bug in main code, committed on its own (`crates/ui/src/components/data_table.rs`): a sideways wheel also scrolled the rows, because the table's vertical list lacked `restrict_scroll_to_axis`. The regression test is `horizontal_wheel_scrolls_only_horizontally` in `grid.rs`.

## Phase A progress

Built and tested headlessly: search box and Clear all filters (toolbar), severity tint (`severity_tint` in `grid.rs`, default colours only), corner sort and select-all, header shift-click column selection, gutter drag, click-again-to-clear, context menu with Copy, Copy as Markdown, HTML and datatable (Cmd/Ctrl+C bound in context `ResultGrid`), a row-count footer and empty-state messages (no paging: dropped, spec section 7 row 23), two-condition filter popover, content-based initial widths, saved layout restored and written back (`ResultsFile::save_layout`, debounced 300 ms, keeps line endings and encoding; real `sample1.ktt` and `sample2.ktt` round-trip byte for byte), a resizable gutter (minimum 40 px, saved), keyboard navigation and Select all and Escape with the view following sideways as well as vertically, auto-scroll in both directions while drag-selecting, `Invalid result file.` and `No result data found.` shown in the tab, and an open file re-rendered when it is edited outside Zed (our own writes are recognised by content and ignored).

Copy as datatable (`kusto_results::export::datatable`) was ported from `KustoGenerator.cs` in the VS Code fork and checked against the real Kusto.Language 12.4.0 library, using a throwaway .NET console project that referenced the DLL from `~/.nuget/packages/microsoft.azure.kusto.language/12.4.0` and called `KustoFacts.GetStringLiteral`, `KustoFacts.BracketNameIfNecessary` and `KustoGenerator` directly. The project was not kept; the vectors it produced are in the tests in `export.rs`, and the keyword list is in `BRACKETED_NAMES`. After a library upgrade, rebuild such a harness and compare.

Not built: severity colour settings (SET-4) and any other Zed settings for this feature (the settings plumbing is a change to the settings crates, to be decided), shift-drag across headers to select several columns (a drag on a header reorders; shift-click ranges work), HTML on the clipboard (CPY-11).

## Not proven yet

Everything ran on GPUI's test platform: no GPU, no pixels. Looked at in a real window: the grid, Row Details, row picking, the wheel, the filter popover. Not looked at: severity colours, the context menu, Copy as datatable pasted into a query, column saving on a real file, the gutter handle, keyboard and auto-scroll feel, the invalid-file messages, an outside edit, Ctrl+F inside the inspector, panel position persistence.

## Next steps

1. Done in code (see Code above). Remaining: run `zed`, open `samples/synthetic-types.ktt` and `synthetic-trace-edge.ktt`, and review by eye. A failed parse currently surfaces only as Zed's generic open-error notification.
2. Done in code: `kusto_results::inspector::build_document` makes the text and style spans; `RowDetailsPanel` shows it in one read-only editor (`InspectorText::set_document`) with a Find box, a Wrap toggle and a match count. Escape clears Find; Cmd/Ctrl+F focuses it (bindings in the three `default-*.json` keymaps, context `RowDetailsPanel > Editor`). Closing the grid that owns the subject empties the panel (RDT-11). Tested headlessly only. Field headers and nulls borrow `ConsoleAnsiHighlight(5)` and `(6)`. Still to do: look at it in a real window, and a theme change is only picked up on the next selection change.
3. Finish phase A: the items under "Not built yet" above, after looking at the new grid behaviour in a real window.
4. Then phases B to E in `kusto-results-workbench-design.md` section 10 (structured view, results panel, execution). Phase F: query parameters, agent handoff.
5. Open checks: Q-3 (is VS Code multi-word search OR) in a running VS Code; Q-9 (other panels in the bottom dock).
