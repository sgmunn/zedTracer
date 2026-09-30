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

- `crates/kusto_results` (built, tested, clippy clean): UI-free core. Modules `result` (model, `.ktt` read/write, lossless numbers), `typed`, `filter`, `view` (visible rows, sort, paging, selection, column order, severity), `activity`, `inspector`, `export`. 76 unit tests plus `tests/sample_fixtures.rs` (12 tests, checks every expected-results file; 1 ignored scale test).
- `crates/kusto_results_ui` (spike code, tests pass, clippy clean): `ResultGrid` on `ui::Table` (`grid.rs`), `FilterPopover`, `InspectorText` (read-only editor), `RowDetailsPanel` plus shared `ActiveSelection` (`row_details_panel.rs`). Not yet registered in the `zed` binary.
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

## Not proven yet

Everything ran on GPUI's test platform: no GPU, no pixels, nothing seen in a real window. Not spiked: auto-scroll while drag-selecting, keyboard navigation, header and corner selection gestures, Ctrl+F inside the inspector, choosing an operator from the popover dropdown, two-condition filters, panel position persistence, key bindings.

## Next steps

1. Register in the `zed` binary following `crates/investigation` (see `crates/zed/src/main.rs` and `zed.rs` for its three registration lines): a `.ktt` project item hosting `ResultGrid`, and `RowDetailsPanel`. Open `samples/synthetic-types.ktt` and `synthetic-trace-edge.ktt` and review by eye.
2. Build the inspector renderer: one read-only editor, header line per field, using `kusto_results::inspector` (`resolve_subject`, `field_value`, `merged_field_value`, `highlight_json`); add Wrap and Find controls.
3. Phase A backlog from the spec P0 list: paging and footer, column layout load and save (`TableView`), Copy as Markdown and HTML text, context menu, severity tint, search box, filter popover with two conditions, header and corner selection, keyboard, auto-scroll.
4. Then phases B to E in `kusto-results-workbench-design.md` section 10 (structured view, results panel, execution). Phase F: query parameters, agent handoff.
5. Open checks: Q-3 (is VS Code multi-word search OR) in a running VS Code; Q-9 (other panels in the bottom dock).
