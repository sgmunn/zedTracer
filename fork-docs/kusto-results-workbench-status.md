# Kusto Results Workbench: status and handoff

Read this first in a new session. Last updated after the code lenses and per-query analysis. Everything is committed on `feature/kusto-syntax-spike`; `git log` shows it in logical commits.

## Goal

Bring the results side of the KustoTraceTools VS Code fork (`/Users/gregm/Projects/Kusto-Explorer-VsCode`, branch `dev/gregm`) into the user's Zed fork (this repo): typed results grid, sorting, filtering, search, Row Details inspector (JSON, exception call stacks, multipart messages), structured activity view, results panel. Charts and `render` are out of scope for now. Since then the work has grown to cover running queries (native Rust over REST), the Results panel, schema-aware editing in the .NET language server, and per-query code lenses. See "Running queries", "Code lenses" and "Known issue" below.

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

- `crates/kusto_results` (built, tested, clippy clean): UI-free core. Modules `result` (model, `.ktt` read/write, lossless numbers, `Table::from_raw_rows`), `typed`, `filter`, `view` (visible rows, sort, selection, column order, severity), `activity`, `inspector`, `export`. About 95 unit tests plus `tests/sample_fixtures.rs` (12 tests, checks every expected-results file; 1 ignored scale test).
- `crates/kusto_results_ui`: `ResultGrid` on `ui::Table`, `FilterPopover`, `InspectorText`, `RowDetailsPanel` with the shared `ActiveSelection`, `ActivityTree` and `StructuredView`, `ResultsViewer` (the `.ktt`/`.kqr` project item and tab), `ResultsPanel` (`results_panel.rs`, the bottom-dock panel), `history.rs` (the history picker, `kusto::ShowHistory`, and deleting old results), `query_parameters.rs` (finding and reading the profile files, the profile picker, `kusto::SelectParameterProfile`, `OpenParameters`, `OpenQueryParameters`), and `run_query.rs` (the `kusto::RunQuery`, `CancelQuery`, `ShowResult` and `CopyClientRequestId` actions, the run log, the settings `kusto.cluster`, `kusto.database`, `kusto.results_location`). Registered in the `zed` binary: `kusto_results_ui::init` and both panels in `initialize_panels`.
- `crates/kusto_client` (UI-free): REST client (`KustoClient`), token providers (`AzureCliTokenProvider` behind `TokenProvider`), v2 response parsing, `query_range_at` (the query around a cursor), and the run log record format (`RunRecord`, `append_record`). Ignored live tests in `tests/live.rs` need a network and `az login`.
- `extensions/kusto`: the dev extension. Tree-sitter grammar for highlighting, a small Rust glue (`src/kusto.rs`, which also tells the server where Zed's data folder is), and the .NET language server in `server/` (`Program.cs` is the LSP loop; `KustoRest.cs` and `SchemaManager.cs` and `SchemaCache.cs` load schema, from disk first (`<data dir>/kusto/schema/<cluster>/<db>.json`, `@databases.json` for a cluster's database list, written by moving a finished file into place); `SignatureHelp.cs`; `QueryBlocks.cs` splits a file into queries; `ConnectionDirectives.cs` reads the directives; `CodeLenses.cs` and `RunLog.cs` build the lenses). Tests: `python3 server/test_server.py` (72 tests, including one that puts a stand-in `az` on the path against a fake cluster, no network).
- Changes outside the new crates: `crates/editor/src/code_lens.rs` (the `zed.dispatchAction` lens command and its test), `crates/settings_content` and `crates/settings/src/vscode_import.rs` (the `kusto` settings), `assets/settings/default.json` and the three `assets/keymaps/default-*.json` (settings defaults and F5/Shift+Enter in `.kql` editors), `crates/zed/src/zed.rs` (panel registration), workspace `Cargo.toml` and `Cargo.lock`. `crates/ui/src/components/data_table.rs` has one earlier bug fix (sideways wheel).
- Samples: `sample1.ktt` and `sample2.ktt` are real telemetry, git-ignored, never commit. `generated/` (200,000-row file) is git-ignored; make it with `python3 fork-docs/samples/generate_samples.py --large`.

## Commands

```sh
cargo test -p kusto_results
cargo test -p kusto_results --release --test sample_fixtures -- --ignored --nocapture   # core scale baseline
cargo test -p kusto_results_ui --profile release-fast --lib
cargo test -p kusto_results_ui --profile release-fast --lib -- --ignored --nocapture frame_time
./script/clippy -p kusto_results -p kusto_results_ui -p kusto_client
cargo test -p kusto_client                       # fake service, no network
cargo test -p editor --lib test_code_lens         # includes the dispatchAction test
cargo test -p zed --bin zed keymap                # built-in keymaps name real actions
dotnet build extensions/kusto/server/KustoLanguageServer.csproj --no-restore && python3 extensions/kusto/server/test_server.py
./extensions/kusto/install-server.sh              # then restart the language server in Zed
```

Notes: `--offline` fails on this workspace (missing index entry), so build online. First build of `kusto_results_ui` tests takes many minutes (editor, workspace, project); use `release-fast`. Bash `sleep` is blocked: run long jobs with `run_in_background` and wait for the notification.

## Decisions taken

- Grid on `ui::Table`; do not use `tabular_data_preview` (UI-1). Header: own right-edge resize handle, no change to `ui` (UI-2). Sort, filter, search run in a background task with a generation number (UI-3).
- Row Details is a right-dock panel following one shared `ActiveSelection` (UI-4). The inspector is one read-only editor with a header line per field; keep borrowing existing editor highlight keys (UI-5). Filter popover is a `PopoverMenu` with its own view and its own outside-click handling (UI-6). Text copy is P0; rich HTML copy is P1 and needs clipboard platform work (UI-7).
- Execution is native Rust over the Kusto v2 REST API, not a .NET process and not the language server; tokens come from `az account get-access-token` (decided with the user). The language server loads its own schema over REST, also signing in with `az`.
- The editor and the language server share no channel: the editor appends run records to `kusto/history/runs.jsonl` and the server reads it (code lenses). A lens that must act in the editor names a Zed action in the `zed.dispatchAction` lens command.
- Every run is saved to the history folder first and the Results panel opens that saved file, so a panel result keeps its layout and can be shown again from history (the user's decision).
- A file is a set of queries separated by blank lines; the run command, the lenses and every language feature use the same rule (`QueryBlocks`, `query_range_at`).
- Where a query runs comes from comments in the file, `// :setDefaultCluster("…")` and `// :setDefaultDb("…")` (the user's decision, after another Kusto extension): the form is `// :setDefaultCluster("…")` because Zed continues a comment with `// ` when Enter is pressed, and `//:` is accepted too; a directive applies to every query below it until a later one overrides it, setting the cluster clears the database, and the settings `kusto.cluster` and `kusto.database` are the defaults before any directive. A block of only comments is not a query. Rust (`crates/kusto_client/src/directives.rs`) and the server (`ConnectionDirectives.cs`) implement it twice and are tested against one file, `fork-docs/samples/connection-directives.json`. A lens shows each query's connection, and the run log's last run is keyed by cluster, database and query text.
- Judge speed in a release build; see "Known issue" below.
- Spec decisions: one call-stack frame per line (Q-13), natural string sort ignoring case but not punctuation (Q-12), whole-view column and table selection (Q-11), non-contiguous row selection added as SEL-11 (Q-4), filters not persisted (Q-6), keep `.ktt` (Q-10).

## Working rules for this repo

Follow `CLAUDE.md` at the repo root: no `unwrap()` in non-test code, no `let _ =` on fallible calls, no `mod.rs`, crate lib path in `Cargo.toml`, full-word names, comments only for why, use `./script/clippy`. The repo `README.md` already carries the required `> [!IMPORTANT]` lines; never remove them. The only intentional panic site is the regex helper in `inspector.rs`.

## After first look in a real window

- Works: opening `.ktt`, the grid, Row Details. Added in response: Ctrl/Cmd+click on row numbers for non-contiguous row selection (SEL-11, in `view::toggle_row`; the rectangle stays for shift-click, the other rows are an added set), and a footer with `N rows` or `M of N rows` plus `Row N selected` or `K rows selected` (`ResultGrid::status_text`).
- Bug in main code, committed on its own (`crates/ui/src/components/data_table.rs`): a sideways wheel also scrolled the rows, because the table's vertical list lacked `restrict_scroll_to_axis`. The regression test is `horizontal_wheel_scrolls_only_horizontally` in `grid.rs`.

## Phase A progress

Built and tested headlessly: search box and Clear all filters (toolbar), severity tint (`severity_tint` in `grid.rs`, default colours only), corner sort and select-all, header shift-click column selection, gutter drag, click-again-to-clear, context menu with Copy, Copy as Markdown, HTML and datatable (Cmd/Ctrl+C bound in context `ResultGrid`), a row-count footer and empty-state messages (no paging: dropped, spec section 7 row 23), two-condition filter popover, content-based initial widths, saved layout restored and written back (`ResultsFile::save_layout`, debounced 300 ms, keeps line endings and encoding; real `sample1.ktt` and `sample2.ktt` round-trip byte for byte), a resizable gutter (minimum 40 px, saved), keyboard navigation and Select all and Escape with the view following sideways as well as vertically, auto-scroll in both directions while drag-selecting, `Invalid result file.` and `No result data found.` shown in the tab, and an open file re-rendered when it is edited outside Zed (our own writes are recognised by content and ignored).

Copy as datatable (`kusto_results::export::datatable`) was ported from `KustoGenerator.cs` in the VS Code fork and checked against the real Kusto.Language 12.4.0 library, using a throwaway .NET console project that referenced the DLL from `~/.nuget/packages/microsoft.azure.kusto.language/12.4.0` and called `KustoFacts.GetStringLiteral`, `KustoFacts.BracketNameIfNecessary` and `KustoGenerator` directly. The project was not kept; the vectors it produced are in the tests in `export.rs`, and the keyword list is in `BRACKETED_NAMES`. After a library upgrade, rebuild such a harness and compare.

Phase B, beyond what phase A already gave (sort, filter, search, severity tint): a busy indicator in the footer for work still running after 250 ms (`Sorting 200000 rows…`, with a progress-indicator accessibility role and the same text as its label; GPUI has no busy property, so this is the closest signal), a search toggle (hidden by default, focuses on show, clears on hide, Escape clears the text, Cmd/Ctrl+F) with the Toggle Search and Clear All Filters actions, and severity colours as a setting (`kusto_results.severity_colors` in `settings_content`, `assets/settings/default.json` and `ResultsSettings`; an empty or unreadable colour leaves that level untinted).

Not built: shift-drag across headers to select several columns (a drag on a header reorders; shift-click ranges work), HTML on the clipboard (CPY-11), a settings-UI page for the new setting.

Phase C (Row Details) was mostly done by the time phase A ended, so what was left: JSON values are set apart as code with a tinted background (`InspectorDocument::code_blocks`, `CODE_BACKGROUND_KEY`, which borrows `ConsoleAnsiHighlight(7)` like the other inspector highlights), the panel docks where the new `kusto_results.dock` setting says and can be moved (`set_position` writes the setting), and an open panel restyles itself when the theme changes. The inspector logic was also run over every row of the real `sample1.ktt` and `sample2.ktt` (7,415 rows): every row builds a document, and all 7 real multi-part groups in `sample2.ktt` assemble (4 of them into JSON). That was a throwaway scan; `real_captures_load_when_present` still only checks loading and the round trip.

Still open in the inspector: EXC-9 and SET-5 (a user list of call-stack frames to hide, P1, phase F), RDT-9 (previous and next among several selected rows, P2).

Phase D (structured activity view), built and tested headlessly. Core: `kusto_results::activity_tree::ActivityTreeState` (which branches are open, the selection, Left and Right, Up and Down, Deepest and its cycling; a 20,000-deep chain is tested). UI, in `kusto_results_ui`: `ActivityTree` (nodes with disclosure, warning triangle, marker name, id, event count, depth badge, severity colour at full or 30 % strength, tooltips including the hierarchy problem lines of ACT-17, tree roles for assistive technology, Enter, Space, arrows, double-click), `StructuredView` (tree, splitter and a scoped `ResultGrid`; splitter default 340 px, minimum 180 px, events pane keeps 280 px, arrows move 20 px or 80 with shift, Home and End, double-click resets, separator role with min, max and current values), and a Data and Structured tab switch in `ResultsViewer` that appears when the table has both activity columns and builds the projection on first use. The grid gained `GridOptions` (a row scope and a view name), so the structured grid saves its layout as `<table>::activity-structured:<index>` (ACT-14) and `set_scope` clears and republishes the selection (ACT-13). Key bindings are in the three default keymaps (contexts `ActivityTree` and `StructuredSplitter`).

Not done for phase D: ACT-16 (the structured tab in the bottom results panel, which arrives with phase E), the Query tab of PER-4 (phase E), and nobody has looked at any of it in a real window.

## Running queries (first slice of phase E)

Decided with the user: execution is native Rust over the Kusto v2 REST API (not a .NET process, not the LSP), and the first version gets tokens from the Azure CLI.

- `crates/kusto_client` (UI-free, tested): `KustoClient::execute` posts to `/v2/rest/query` and returns a `kusto_results::ResultSet` (primary tables only; repeated `PrimaryResult` names become `PrimaryResult_2`, and so on, because saved table views are found by name). `KustoClient::cancel` sends `.cancel query "<client request id>"` to `/v1/rest/mgmt`. The token audience is read from `/v1/rest/auth/metadata` (`KustoServiceResourceId`, `https://kusto.kusto.windows.net` for every cluster tried, Fabric eventhouses included) and falls back to the cluster address. `TokenProvider` is the seam; `AzureCliTokenProvider` runs `az account get-access-token` with the project's shell environment, because a desktop app does not inherit the `PATH` where `az` lives. Error bodies are reduced to the innermost service message. `query_range_at` finds the query around the cursor: the run of non-blank lines, with a cursor on a blank line belonging to the query above.
- Results panel (`results_panel.rs`, `ResultsPanel`): a bottom-dock panel, registered in `initialize_panels` in `zed.rs`, that shows the latest run: an empty state (`No results`), the result in the same `ResultsViewer` a tab uses (with a one-line summary: cluster / database, rows, duration, start time) or the error of a failed run. Its badge is the row count, or `!` after an error. `kusto::ToggleResults` shows and hides it (no key binding yet). The setting `kusto.results_location` is `panel` (default) or `editor` (a tab per run, as before); without the panel registered, results fall back to tabs. A run opens the panel without taking focus, so the query stays in front and F5 works again. The panel does not read the response directly: every run is saved to `<data dir>/kusto/history/*.ktt` first and the panel opens that file the way a tab does (`ResultsFile::try_open`), so a panel result keeps its column layout, follows outside edits, and any history file can be shown again with `ResultsPanel::show_result`. Not built: a table tab for each table of a multi-table result (PNL-4; the viewer shows the first table), a Query tab (PNL-6), Save As, a history list to pick an older result, moving the panel to another dock.
- `crates/kusto_results_ui/src/run_query.rs`: the `kusto::RunQuery` action (F5 and Shift+Enter in `.kql` editors, in the three default keymaps) runs the selection, or the query at the cursor, on `kusto.cluster` and `kusto.database` from settings (a project's `.zed/settings.json` works). A "Running query on <cluster>…" toast offers Cancel. A successful run is written to `<data dir>/kusto/history/<UTC time>-<uuid>.ktt` and opened in the existing results viewer, so each result lives in its own tab (RUN-5) and the file is a normal `.ktt` (PER-2). Failures are shown with `Workspace::show_error`. A cancelled run shows nothing (RUN-3).
- Live check, not run in CI: `cargo test -p kusto_client --test live -- --ignored --nocapture` runs real queries against `help.kusto.windows.net` with your `az login`.

Not built yet: query parameters (`declare query_parameters`, profiles), choosing a cluster or database from a list (settings only for now), a per-document connection, history browsing, rerun from a saved result (RUN-8), the per-query inline actions (RUN-9), the minimum-500 ms indicator, a sign-in that does not need the Azure CLI, and charts. A long-running response is parsed whole; progressive frames are not requested.

## History

`kusto::ShowHistory` opens a picker (`history.rs`) of the runs in `runs.jsonl`, newest first (`kusto_client::history_entries` folds the records of a run into one entry; cancelled and unfinished runs are not listed). A row is the comments the query starts with (not directives) and then its first line of code, then `time · rows, duration · cluster / database` (or `failed: <first line>`, or `result moved or deleted`); typing filters on the query, cluster, database and error. Enter shows the result through `display` (the panel, or a tab by the `results_location` setting), the secondary Enter opens a tab, and a hover trash button deletes the file. The lens `Results` and the picker say so when a file is gone.

Rerun (RUN-8): a hover play button dispatches `kusto::RerunQuery { query, cluster, database, parameters }`, which `run_query.rs` handles by `launch_run` (the code `start_run` shares: `start_run` reads the editor, a rerun supplies a `RunSpec` with `ParameterSource::Values`). The values of the original run are kept in the `parameters` field of the run log's `started`, `finished` and `failed` records (left out when there are none; the server ignores the field) and carried into `HistoryEntry`, so a rerun repeats the run even after the active profile changed. The query text in a rerun is the stored text, comments and all, so its lens matches the original.

Keeping a result: `kusto::SaveResult { path?, suggested_name? }` (`save_result.rs`; `Save a copy…` in the viewer's tab bar, a download button on a history row, or the palette) asks where with `Workspace::prompt_for_new_path` (which opens in the folder of the most recently active file, so normally the work folder), suggests a name from `suggested_file_name` (the query's `history::title` slugged to 48 characters at a word, plus the local start time), and copies the file with `fs.copy_file`, overwriting what the prompt confirmed; a toast offers to open the copy. Pruning only reads the history folder, so a `.ktt` outside it is never deleted. A result file now carries what is needed to understand and repeat it: `KustoClient::execute` writes `parameters` (the string values; the field already existed in the `.ktt` format) next to `query`, `cluster` and `database`. `ResultsViewer` (`results_viewer.rs`) shows a tab bar for every usable result, with `Data`, `Structured` when the table has activity columns, `Query` when the file names a query (`query_view.rs`: a read-only editor, coloured with the Kusto language when the registry has it, under a line saying where and when it ran and the parameter values), and at the right `Run again`, which dispatches `kusto::RerunQuery` built from the file (`rerun_action`; it needs query, cluster and database). The viewer gets the language registry from the project (`for_project_item`) or, for the panel, from the workspace's project (`display`).

Pruning (`kusto_client::files_to_prune`, run by `history::prune`): after each successful run and at startup, the oldest results past `kusto.history_max_results` (50) or `kusto.history_max_megabytes` (1024) are deleted, 0 meaning no limit; the newest result is always kept; only names of the form `<YYYYMMDD>-<HHMMSS>-<uuid>.ktt` are considered, so hand-saved files and `runs.jsonl` are safe; results open in the workspace that ran (tabs and the panel) are kept but count against the limits.

Found on the way: a result opened from the history lives in a single-file invisible worktree, which a project holds only weakly, and `ResultsFile` held it weakly too, so reloading and writing a column layout back could silently stop. `ResultsFile` now holds the worktree.

## Refreshing the schema

A `↻ Schema: <age>` lens (`CodeLenses.SchemaTitle`) follows the connection lens of each query that has a cluster and database. The age is when the schema in memory was fetched, from the cache file's `fetchedAt` when it came from the cache; `loading…`, `refreshing…` and `not loaded` are the other states (`SchemaManager.StatusOf`, kept per `host/database`, with a `Changed` event that makes the server send `workspace/codeLens/refresh`; not before the client's `initialized`). Its command is `kusto.refreshSchema [cluster, database]`, not a Zed action: Zed already sends any lens command that is not `zed.dispatchAction` to the server as `workspace/executeCommand` (as for `kusto.connection`), so this needed no change in the fork. The server answers at once, fetches in the background (`SchemaManager.RefreshAsync`: the database list and the database, ignoring the cache's age, keeping the old schema on failure) and reports with `window/showMessage` (type 3 with the counts, or type 1 with the reason), which Zed shows as a notification.

## Query parameters

Design from the VS Code fork (`docs/FORK_DESIGN.md` there): a query declares `declare query_parameters(raid:string)`, the values come from the active profile of `.kusto/parameters.yaml` or an adjacent `<name>.parameters.yaml`, and go to Kusto as native parameters (`properties.Parameters` of the v2 request, strings parsed by the service by declared type; proved against the help cluster by the ignored live test `passes_values_for_declared_query_parameters`), never pasted into the text.

- `crates/kusto_client/src/parameters.rs`: `ParameterProfiles::parse` (serde_yaml_ng; an active name that is not a profile is no active profile), `with_active_profile` (edits only the `active:` line), `template`, `sidecar_path`, `declared_parameters` (a scanner that skips comments and quoted text), `parameters_for_query` (only declared names the profile has). `QueryRequest.parameters` adds `properties.Parameters` to the body when not empty.
- `crates/kusto_results_ui/src/query_parameters.rs`: `ParameterFiles::of_editor` (sidecar, and the worktree's `.kusto/parameters.yaml`), `load` (sidecar if it exists, else the project's; a file that cannot be read fails the run instead of running without values), the Picker (profiles, `No active profile`, `Edit profiles…`; writes the file with `write_active_profile`), and the open actions. The run path calls `load` in the run's task.
- Server (`QueryParameters.cs`, YamlDotNet 16.3.0): reads the same files, finds the declared names and whether each has a default, and `CodeLenses.For` adds `Params: <profile>` (`none`, `(no value for …)`, `cannot read <file>`) that dispatches `kusto::SelectParameterProfile`. It watches the files of open documents (not creating `.kusto`) and refreshes the lenses when one changes; workspace folders come from `initialize`. The declared names are tested against one shared file, `fork-docs/samples/query-parameters.json`, in both languages. Not shared: file parsing, which is tested in each language on its own.
- **"Make Active" lens in the profiles files (built):** `extension.toml` lists `YAML` in `[language_servers.kusto-lsp] languages` (`YAML = "yaml"` in `language_ids`). Zed registers an extension's server per language name and YAML's default `language_servers` is `["..."]` (every registered server), so no settings change is needed (read from `extension_lsp_adapter.rs` and `default.json`, not yet seen running). Every YAML file therefore reaches the server, which ignores all of them except `.kusto/parameters.yaml` and `*.parameters.yaml` (`QueryParameters.IsYaml`, `IsProfilesFile`): those are kept in `profilesFiles` (never in `documents`, so they get no query completion, hover or diagnostics), and `CodeLenses.ForProfilesFile` gives `✓ Active` (`kusto.noop`) or `Make Active` (`zed.dispatchAction` with `kusto::MakeParameterProfileActive { name }`) on the line of each key under `profiles:` (`QueryParameters.ProfileKeys`, from YamlDotNet's marks), from the text of the editor. `query_parameters.rs` handles the action: it takes the active editor's buffer (a lens click focuses the editor), checks the file name, computes the new text with `with_active_profile` and applies only the differing part (`minimal_edit`) to the buffer, then saves it. Downside: opening any YAML file starts the .NET server. The other option, a separate "Kusto Parameters" language with its own YAML grammar, was not taken.
- Not built: the status-bar picker (Zed has no equivalent surface for a lens-less file), importing profiles from the clipboard, completion of parameter names in the YAML, and schema-aware checking of a value against its declared type.

## Code lenses (RUN-9, first slice)

Per-query lenses above each blank-line-separated query, from the language server: `▶ Run`, `<spinner> Running… <elapsed>` with `Cancel` (the server re-sends the lenses four times a second while any run is going, because a lens is plain text from the server; if Zed's lens blocks flicker at that rate, drop the spinner and refresh once a second), `Last run: <time>, took <duration>, <rows>`, `Results`, `Copy CID`, `Last run failed: <message>`, `↻ Schema: <age>` (click to refresh), and, for a query that declares parameters, `Params: <profile>`, and in a profiles file `Make Active`. Enable with the Zed setting `"code_lens": "on"`.

- **Fork change** (`crates/editor/src/code_lens.rs`): a lens command `zed.dispatchAction` (`DISPATCH_ACTION_COMMAND`) is handled in the editor instead of being sent to the server. Its arguments are an action name and optional JSON data; the editor builds the action with `cx.build_action`, focuses itself and dispatches it. The click handler already puts the cursor on the lens first. Test: `test_code_lens_dispatches_a_named_action_instead_of_asking_the_server`. Nothing else in Zed changed.
- **How the server learns about runs:** it does not talk to the editor. Each run appends `started`, then `finished` (rows, duration, saved file) or `failed` or `cancelled` to `<data dir>/kusto/history/runs.jsonl` (`kusto_client::RunRecord`, written by `log_run` in `run_query.rs`, trimmed to the last 400 records). The server (`RunLog.cs`) reads the tail of the file when asked for lenses, matches by the query text without comments and layout, and watches the file to send `workspace/codeLens/refresh`. The data directory reaches the server as `KUSTO_ZED_DATA_DIR`, which the extension derives from its working directory (`<data dir>/extensions/work/kusto`).
- **Actions** in `run_query.rs`: `kusto::CancelQuery` (cancels the newest running query at the cursor, else the newest run), `kusto::ShowResult { path }` (shows any saved result in the panel, or a tab), `kusto::CopyClientRequestId { id }`. Lenses that only show text name `kusto.noop`, which the server accepts, so that Zed makes them clickable.
- Not built: Select, Copy and Format lenses (RUN-9), a lens for a query the server cannot tell apart from another with the same text, and two Zed windows writing the log at the same moment can lose a record.

## Known issue: large results are very slow to download in a debug build

Judge speed in a release build (`cargo run --release -p zed`, or `--profile release-fast`). In a debug build (`cargo run -p zed`) a result of about 13 MB took 250 to 290 seconds to download; in a release build the same query matches VS Code (about 2 seconds).

What was measured (the `kusto:` lines in `~/Library/Logs/Zed/Zed.log`, which Zed writes there instead of the terminal when stdout is piped):

| Stage | Time |
| --- | --- |
| token audience, new token | 0.4 s, 0.7 s (once; later runs reuse both) |
| response headers | 0.09 s |
| reading the 13.4 MB body | 288.9 s |
| parse, serialise, save, open the tab | 0.1 s, 0.2 s, 3 ms, 0.24 s |

How it was narrowed down, with the ignored tests in `crates/kusto_client/tests/live.rs`:

- The same query through `ReqwestClient::new()` takes about 1.4 s, even in a debug build. Through `ReqwestClient::proxy_and_user_agent` (what `crates/zed/src/main.rs` builds for the app) it takes 250 to 283 s.
- The two clients differ in HTTP version: the plain clients negotiate HTTP/2, the app's negotiates HTTP/1.1 (`http_version_by_client_construction`). The app's client uses a preconfigured rustls config (`http_client_tls::tls_config`), which seems not to offer h2.
- A 1 MB result is fast with every client in debug and release, so the slowdown needs a large response, and a release build does not show it.
- Not the cause: gzip (runs before and after we asked for it were both slow), JSON parsing, saving, opening the file, the grid, the proxy (none is set) or the token. Compiling `hyper`, `hyper-util`, `httparse`, `rustls` and `bytes` with `opt-level = 3` in `[profile.dev.package]` did not help (about 248 s), so it is not just unoptimised HTTP code. That change was reverted.

Not found: why an HTTP/1.1 body read is slow only in a debug build. Ideas, cheapest first: log chunk sizes and the time between reads of the body in `KustoClient::send`; try `read_to_end` through a larger fixed buffer; build a client with h2 for Kusto only (a second `reqwest` client in `kusto_client`, leaving the shared Zed client alone); look at `ReqwestClient`'s `StreamReader` and `into_async_read` path for a debug-only cost. Do not change the shared client's TLS config without checking what else relies on it.

Reproduce:

```sh
KUSTO_TIMING_QUERY=<file> KUSTO_TIMING_CLUSTER=<url> KUSTO_TIMING_DATABASE=<db> KUSTO_TIMING_APP_CLIENT=1 \
  cargo test -p kusto_client --test live time_a_query_from_a_file -- --ignored --nocapture
cargo test -p kusto_client --release --test live http_version_by_client_construction -- --ignored --nocapture
```

Also found at the same time, and fixed: every run built a new client and so asked for the token audience and started `az` again. The client is now shared and caches both (a repeat run went from about 470 ms to about 60 ms), and it asks for gzip because Zed's reqwest has no decompression feature.

## Not proven yet

Most of this ran on GPUI's test platform and the language server's own tests: no GPU, no pixels. Looked at in a real window by the user: the grid, Row Details, row picking, the wheel, the filter popover, running a query with F5, and the speed of a release build (about the same as VS Code). Not confirmed in a real window: the Results panel, the code lenses and the spinner, schema and signature-help completion, the snippet cursor placement, per-query diagnostics, severity colours, the context menu, Copy as datatable pasted into a query, column saving on a real file, the gutter handle, keyboard and auto-scroll feel, the invalid-file messages, an outside edit, Ctrl+F inside the inspector, panel position persistence.

## Known gaps

- Large downloads are very slow in a debug build (see above); the cause is only partly understood.
- The settings reach the server through a file, not through the LSP: Zed writes the `kusto` cluster and database to `<data dir>/kusto/defaults.json` (`run_query.rs`, `share_defaults_with_language_server`) and the server follows it (`DefaultsFile.cs`), so a change applies without a restart and wins over `lsp.kusto-lsp.initialization_options`. Only the user-level `kusto` values are written, as for running.
- A cached schema is replaced on its own only when it is more than an hour old (option `schemaCacheMinutes`); the `↻ Schema` lens refreshes it now, but there is no palette command for it (a Zed action would have to send `workspace/executeCommand` to the server, which no code in kusto_results_ui does yet) and no way to refresh every database at once. The token audience is cached in memory (editor and server) but not on disk, which would save about 0.4 s on the first run after starting Zed.
- Diagnostics are syntax-only (table and column names are not checked). Signature help finds unqualified function names only, and a repeatable parameter such as `strcat`'s highlights the wrong argument.
- The Results panel is fixed to the bottom dock and shows the first table of a result; there is no Query tab, no Save As and no history picker.
- Pruning protects only the results open in the workspace that ran the query; a result open in another Zed window can have its file deleted (it keeps showing from memory). Results older than the run log's 200 runs are on disk but not in the picker until pruned.
- Two Zed windows writing the run log at the same moment can lose a record.
- Shift+Enter in a `.kql` editor runs the query and no longer inserts a newline (the VS Code binding).

## Next steps

1. The user looks at the Results panel, the lenses and the spinner in a real window; fix what shows up. If the spinner flickers, refresh once a second instead.
2. Persisting the token audience on disk, and a palette command for refreshing the schema. The `kusto` settings, directives, the schema cache and its refresh lens are done; see the decisions above.
3. Look at the parameter lens and picker in a real window; then import profiles from the clipboard (the VS Code `Group1…` JSON import) if it is missed.
4. A key binding for the history and the panel; a rerun with the profile of today (a second button or a modifier), if repeating the original values is not always what is wanted.
5. Schema-aware diagnostics, once schema is cached so they do not flag names while a load is in flight; a cluster and database per file; the missing lenses (Select, Copy, Format); a table tab for each table of a result.
6. Open checks from the spec: Q-3 (is VS Code multi-word search OR) in a running VS Code; Q-9 (other panels in the bottom dock).
