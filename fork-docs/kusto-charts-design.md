# Kusto charts for Zed: investigation and draft specification

Status: built; the decisions in section 8 were taken as recommended, and section 10 says what is and is not built. Companion to [kusto-results-workbench-feature-spec.md](kusto-results-workbench-feature-spec.md), which lists charts as "Out, to be specified later" (section 1). This document is that later specification: what we found, what we recommend, the requirements, and the decisions we need from the user before building.

## 1. Goal

Charts of query results in the Results panel and in a results file (`.ktt`), editable in both, in the way the VS Code fork already works. The chart types that matter are **line and time charts, bar charts and column charts**. Everything else the VS Code extension can draw is out of scope unless section 8 says otherwise.

## 2. What VS Code does today

Read from `Kusto-Explorer-VsCode` (`dev/gregm`), mainly `src/Client/features/{chartProvider,chartEditorProvider,plotlyChartProvider,resultsViewer}.ts`, `src/Server/Charting/*.cs`, `src/Server/Utilities/ResultData.cs` and commit `59e7c65` (editable multi-chart result files).

**Persisted model.** A `.ktt` file holds `charts[]` and `chartViews[]` next to `tables[]`. Each chart is `{ name?, tableName?, options }`. `options` is one flat object: `type` (`Line`, `TimeLine`, `Bar`, `BarStacked`, `Column`, ...), `title`, `xTitle`, `yTitle`, `xColumn`, `yColumns[]`, `seriesColumns[]`, `xAxis`/`yAxis` (`Linear`/`Log`), `xMin`/`xMax`/`yMin`/`yMax`, `yLayout` (`SharedAxis`, `DualAxis`, `SeparatePanels`, `SeparateCharts`), `accumulate`, `sort`, `showValues`, `legendPosition`, tick and grid switches, tick angles, `aspectRatio`, `textSize`, `mode` (`Auto`/`Light`/`Dark`), marker options, and the extras the TypeScript type has but the C# class does not: `binSize`, `aggregation`, `maxSeries`, `maxPointsPerSeries`. `chartViews[]` holds per-chart presentation state (today only graph node positions) and is matched to a chart by `name`/`tableName`. A real saved file (`kusto-testing/results.ktt`) holds two charts, the first unnamed, the second named `Chart 2` and bound to `PrimaryResult`.

**Where charts come from.** Two sources:

1. The query's `render` operator. The server reads the Kusto SDK's `ChartVisualizationOptions` and maps `timechart`/`linechart`/`barchart`/`columnchart`/... to a `type` plus the options (`ConnectionManager.cs`, `ChartOptions.FromChartVisualizationOptions`).
2. The user, with no `render`: **Add Chart** creates a chart over any table, so visualising does not require editing the KQL. **Remove Chart** deletes one. A file with several charts shows them as named tabs.

**Rendering.** Plotly.js in a webview. Plotly's SVG traces become unresponsive in the low thousands of points, so the code switches to WebGL above 1,000 points per trace. Hover mode is `closest`, and the legend keeps Plotly's default of toggling a series on click.

**Editing.** A form generated from the `ChartOptions` shape. Every change posts the full options object back and the chart redraws. In a file-backed document the change is saved to the `.ktt` automatically; in the bottom panel it is held in memory.

**Data rules** (all in `plotlyChartProvider.ts`):

- X column: `xColumn` if given; for time chart types the first datetime column that is not a Y column; otherwise the first column that is not a Y column; otherwise column 0.
- Y columns: `yColumns` if given; otherwise every numeric column except the X and series columns.
- Series: if `seriesColumns` is empty the first column that is not numeric, not datetime and not X is used. Rows are grouped by the series values; each pairing of a group with a Y column is one line or bar group, named `series` or `series - ycolumn`.
- Null X or Y drops the point. Points are sorted by X so lines do not zigzag. `accumulate` is a running total. `maxSeries` keeps the top N series by total absolute value. `maxPointsPerSeries` thins by taking every Nth point. `binSize` plus `aggregation` (`Sum`, `Count`, `Average`, `Min`, `Max`) re-bins.
- Copy chart writes a PNG (and SVG) in light or dark.

## 3. What the Zed fork has today

**The two surfaces are already one implementation.** The Results panel and a results-file tab both show a `ResultsViewer` over a `ResultsFile` (`crates/kusto_results_ui/src/results_panel.rs`, `results_viewer.rs`). Every run is written to a `.ktt` in the history folder and the panel opens that file (`run_query.rs`, around lines 898 to 945 and 822). So a chart tab added to `ResultsViewer` serves both, and an edit made in the panel can be saved to the run's history file, which VS Code's panel cannot do.

**Saving is a solved pattern.** `ResultsFile::save_layout` updates the in-memory `ResultSet`, then a 300 ms debounced `write` stores it through the worktree. It returns early when the file is not writable, so edits then live for the session only. Chart edits should use the same path.

**The file format already carries charts.** `crates/kusto_results/src/result.rs` reads top-level properties it does not know (including `charts` and `chartViews`) as raw JSON and writes them back unchanged; a test pins this. Only `tableViews` is interpreted. A chart feature must interpret `charts`/`chartViews` without losing option keys it does not know.

**`render` is dropped.** `kusto_client::response::parse_query_response` keeps only `PrimaryResult` tables, so the `@ExtendedProperties` table is discarded. The SDK source (decompiled `Kusto.Data` 13.0.2, `QueryResultSetV2.ParseChartVisualizations`) shows its real shape: three columns `(TableId:int, Key:string, Value:string)`; each row with `Key == "Visualization"` carries, in `Value`, a JSON object `{Visualization, Title, XColumn, Series[], YColumns[], AnomalyColumns[], XTitle, YTitle, Kind, YSplit, Legend, XAxis, YAxis, Ymin, Ymax, Xmin, Xmax, Accumulate, IsQuerySorted}`; `TableId` is the id of the **frame** of the table it applies to (the properties are frame 0 and the first result is usually frame 1), not its place among the results. The shape was checked against the help cluster. `Visualization` is the lower-case operator name (`timechart`, `linechart`, `barchart`, `columnchart`, `areachart`, `stackedareachart`, `piechart`, `scatterchart`, ...). The unit-test fixture in `response.rs` had a one-column `@ExtendedProperties` table, which was not the real shape, and now has the real one. The service also ignores the legacy `render columnchart kind=stacked` when a `with (...)` follows it, so `kind` belongs inside the `with`.

**Drawing.** There is no webview, so Plotly is not an option. GPUI offers what a 2D chart needs: the `canvas` element, `PathBuilder` (stroked and filled paths, dash arrays, arcs, curves; used by `editor/src/element.rs` and `git_ui/src/git_graph.rs`), `paint_quad`, text shaping, and mouse events. The Timeline tab (`waterfall_view.rs`) is drawn with positioned elements; the Sequence tab is Mermaid rendered by the Markdown view.

**Mermaid `xychart` exists.** Zed's `merman` renderer (0.8.0-alpha.5) supports `xychart` with `bar` and `line` series and a horizontal orientation. In its sources and tests it has no legend, no series names and no datetime axis (the X axis is a category list or a numeric range), and it is a static image with no hover. Useful for a throwaway spike; not a base for the product.

**Export is feasible.** `resvg` is built with `text` and `system-fonts`, and `cx.write_to_clipboard(ClipboardItem::new_image(..))` is already used (the REPL output copies images; the macOS pasteboard writes PNG), so a chart can be turned into SVG, rasterised and copied.

**UI controls exist.** `ContextMenu`/`DropdownMenu`, `Editor` text inputs and popovers (see `filter_popover.rs`) cover a chart form. Per-feature settings live in `results_settings.rs`. `actor_colours.rs` is the precedent for a theme-aware categorical palette.

## 4. Recommendation

**Draw natively with GPUI, from a scene that is built without it.**

- In `kusto_results` (no GPUI): the chart model (`ChartSpec`, a serde mirror of the options with a pass-through for unknown keys); the data step (table plus spec to series of numeric points: column inference, series grouping, sorting, accumulate, top-N, thinning); axis scales and tick generation (linear, log, and calendar-aware "nice" datetime ticks); and a `ChartScene` of primitives (polyline, filled area, rect, text, with colours as theme roles). Text width is the only thing layout needs from the UI, so it is a small trait, with a fixed-width implementation for tests.
- In `kusto_results_ui`: a `ChartView` that paints the scene on a `canvas`, adds hover and legend interaction, and a chart editor. Add it as a tab in `ResultsViewer` (`ViewMode::Chart(index)`).
- In `kusto_client`: read the `Visualization` rows and map them to `ChartSpec`s on the result.
- Export reuses the scene: the same primitives are written as SVG, rasterised with `resvg`, and copied as PNG.

Why this over the alternatives:

| Option | Verdict |
| --- | --- |
| Native GPUI scene (above) | Recommended. Interactive, theme-aware, sharp at any size, one set of layout rules for screen and export, and nearly all of it is unit-testable without a window. Most work: ticks, layout and hover are ours to write. |
| Mermaid `xychart` | Spike only (section 9). No datetime axis, legend or series names; static. |
| SVG string shown as an image | Fine for export, not for the live view: no hover, and re-rasterised on every resize. |
| Webview and Plotly | Not available in Zed. |
| `plotters` crate | Already in the lock file only through `criterion`. Its output is static; it would add a dependency and a second layout engine, and still not give hover. |

## 5. Draft requirements

Priorities follow the workbench spec: **P0** the first usable release, **P1** soon after, **P2** later or undecided. "VS Code today" is in section 2; a requirement without a note is the same as VS Code.

### 5.1 Chart model and file (CFM)

| ID | P | Requirement |
| --- | --- | --- |
| CFM-1 | P0 | `charts[]` and `chartViews[]` are read and written in the VS Code shape (section 2). A file with neither is written exactly as before. |
| CFM-2 | P0 | Option keys and `chartViews` content the fork does not know survive a read and write unchanged, including every VS Code option on a chart type the fork does not draw. A chart of an unsupported `type` is kept and shown as "not supported here", never dropped or rewritten. |
| CFM-3 | P0 | `type` values are the VS Code names (`Line`, `TimeLine`, `Bar`, `Column`, ...), not the `render` names. |
| CFM-4 | P0 | A chart binds to a table by `tableName`, else to the first table. A `tableName` that no longer matches shows an error on that chart only. |
| CFM-5 | P1 | Reading tolerates a missing `options.type` and out-of-range values (a chart that cannot be drawn says why; the file still opens). |

### 5.2 Where charts come from (CSR)

| ID | P | Requirement |
| --- | --- | --- |
| CSR-1 | P0 | `@ExtendedProperties` is read: each `Visualization` row yields a chart for table `TableId`, mapped as `ChartOptions.FromChartVisualizationOptions` does (kind to `type`, `Series` to `seriesColumns`, `Legend: hidden` to `legendPosition: None`, `YSplit` to `yLayout`, `NaN` limits to unset). A malformed `Value` yields no chart and no failure. |
| CSR-2 | P0 | A result that has a `render` chart saves it in the history `.ktt` like any other chart. |
| CSR-3 | P0 | The user can add a chart to any result with no `render`, choosing a type; the columns are inferred (CDM-1 to CDM-3) so the first chart appears without a form. |
| CSR-4 | P0 | The user can remove a chart and rename it. A result can hold several charts, each its own tab. |
| CSR-5 | P1 | A run whose query has `render` opens on the chart tab, as VS Code does for the document view. (Setting, CST-1.) |
| CSR-6 | P2 | Decision needed: do charts survive re-running the same query (section 8, D3)? |

### 5.3 Turning a table into series (CDM)

| ID | P | Requirement |
| --- | --- | --- |
| CDM-1 | P0 | X column, Y columns and series columns follow the inference rules in section 2 when the options leave them out. |
| CDM-2 | P0 | Y values must be numeric (`int`, `long`, `real`, `decimal`); a non-numeric Y column is ignored with a note in the editor, not an error. Values are drawn as `f64`; 64-bit integers and decimals beyond 2^53 lose precision on the chart only, never in the data. |
| CDM-3 | P0 | Rows with a null X or Y are dropped from that series. Line series are sorted by X. A `datetime` X is parsed from the server's ISO 8601 text; `timespan` X is parsed from `[-][d.]hh:mm:ss[.fffffff]` (P1). A string X is a category, in first-seen order unless `sort` says otherwise. |
| CDM-4 | P0 | Series come from grouping by the series columns; a multi-column key is joined with a separator; several Y columns on a grouped chart are named `series - column`. |
| CDM-5 | P0 | `accumulate` draws running totals per series. |
| CDM-6 | P1 | `maxSeries` keeps the top N series by total absolute value; the legend says how many were hidden. |
| CDM-7 | P0 | Large series are thinned for drawing without losing peaks (min and max per pixel column), not by taking every Nth point; hover still reports real values. (VS Code today: every Nth point.) |
| CDM-8 | P2 | `binSize` with `aggregation`. Defer: `bin()` in the query does this better. Added after the first look at a real result, which had several rows for one series at one X and drew a vertical zigzag there: the `aggregation` option alone (sum, average, min, max, count) combines the rows that share an X, and without it the chart notes how many do. Binning is still not built. |
| CDM-9 | P0 | The chart is built off the UI thread and rebuilt only when the data or the data-affecting options change. |

### 5.4 Drawing (CVW)

| ID | P | Requirement |
| --- | --- | --- |
| CVW-1 | P0 | Types: **Line**, **TimeLine** (a line with a datetime axis), **Bar** (horizontal bars), **Column** (vertical bars). Grouped by default. |
| CVW-2 | P1 | `BarStacked`, `BarStacked100`, `ColumnStacked`, `ColumnStacked100`, `Area`, `AreaStacked`, `AreaStacked100`. |
| CVW-3 | P0 | Axes: linear and log scale; fixed min and max, either of which can be set alone (the other end fits the data; VS Code requires both, but using the chart showed one at a time is what is wanted), and on bars over times or numbers the rows outside the X range are left out; titles; tick marks and grid lines each on or off; tick label angle; labels that would overlap are thinned or rotated, never overprinted. |
| CVW-4 | P0 | Datetime axis ticks land on calendar boundaries (seconds to years) with labels that drop the shared part (a date once, then times). Times are shown in UTC by default, matching the grid (GRD-4); a setting may switch to local (CST-1). |
| CVW-5 | P0 | Legend: `Auto`, `Right`, `Bottom`, `None`; entries show the series colour and name; a long list scrolls or truncates with a count. |
| CVW-6 | P0 | `showValues` writes the value on bars. `sort` orders categories by total. |
| CVW-7 | P0 | Colours come from a categorical palette derived from the theme (like `actor_colours.rs`), stable per series name so a series keeps its colour across redraws, and readable in light and dark themes. `mode: Light`/`Dark` only affects export. |
| CVW-8 | P1 | `yLayout`: `DualAxis`, `SeparatePanels`, `SeparateCharts`. `SharedAxis` is the P0 default. |
| CVW-9 | P0 | `aspectRatio` (`Fill` and the fixed ratios) and `textSize` are honoured; with `Fill` the chart follows the tab size. |
| CVW-11 | P1 (Zed addition) | Reference lines: the user can put horizontal and vertical lines across a chart at values they choose, each with an optional label, a dashed or solid style and one of a few theme colours, saved as `referenceLines` in the chart's options (VS Code drops them). They are added from the right-click menu at the pointer and from the pane, are drawn under the data, and an axis grows to hold them unless its limits are set. |
| CVW-10 | P0 | An empty chart (no rows, no numeric column, no X) shows a one-line reason, not a blank area. |

### 5.5 Interaction (CIN)

| ID | P | Requirement |
| --- | --- | --- |
| CIN-1 | P0 | Hover shows the nearest point: X, series name and value, for line charts a crosshair at the nearest X listing every visible series; for bars the hovered bar. |
| CIN-2 | P0 | Clicking a legend entry toggles that series; the axes rescale to what is visible. State is per view, not saved. |
| CIN-3 | P1 | Drag to zoom X, double-click to reset; mouse wheel with a modifier zooms. |
| CIN-4 | P1 | Hovering a point selects the matching row in the grid on request (a Zed addition; the grid already publishes selections). |

### 5.6 Editing (CED)

| ID | P | Requirement |
| --- | --- | --- |
| CED-1 | P0 | The same chart can be edited in the Results panel and in a results-file tab. Opening the editor does not move or resize the chart unexpectedly. |
| CED-2 | P0 | Controls for: type, title, X column and title, Y columns, series columns, Y title, axis scale and limits, legend, sort, show values, accumulate, aspect ratio, text size, tick and grid switches. Every other option is kept but not shown (CFM-2). |
| CED-3 | P0 | A change redraws immediately and is saved to the file with the existing debounce (section 3). A file that cannot be written keeps the edit for the session and says so once. |
| CED-4 | P0 | Column pickers list only sensible columns (numeric for Y), but never hide one already chosen. |
| CED-5 | P1 | "Reset to defaults", and capture the current look as the default (VS Code's defaults actions). |

### 5.7 Placement and commands (CPL)

| ID | P | Requirement |
| --- | --- | --- |
| CPL-1 | P0 | Each chart is a tab beside the table tabs in `ResultsViewer`, in the panel and in a file tab alike. A result with no chart shows an "Add chart" action in the tab bar. |
| CPL-2 | P0 | Commands: Add Chart, Edit Chart (toggle the editor), Remove Chart, Copy Chart; each reachable from the tab bar and the command palette. Key bindings are unassigned by default. |
| CPL-3 | P1 | Selecting a table tab and choosing Add Chart binds the chart to that table. |

### 5.8 Export (CXP)

| ID | P | Requirement |
| --- | --- | --- |
| CXP-1 | P1 | Copy Chart writes a PNG to the clipboard at the current size (or a chosen size), in the chart's `mode` or the current theme. |
| CXP-2 | P2 | Save as SVG or PNG. |

### 5.9 Settings (CST)

| ID | P | Requirement |
| --- | --- | --- |
| CST-1 | P1 | Defaults for the options VS Code takes from `kustoTraceTools.chart.*` (legend position, tick and grid switches, show values, text size, aspect ratio), plus: chart opens on `render`, and UTC or local time. An option saved in a chart wins over the default. |

### 5.10 Non-functional (CNF)

| ID | P | Requirement |
| --- | --- | --- |
| CNF-1 | P0 | A chart of 10 series by 5,000 points redraws in one frame budget while hovering, and opens without blocking typing (CDM-9). |
| CNF-2 | P0 | Round-trip tests for CFM-1 and CFM-2 run over the sample `.ktt` files; scene tests need no window. |
| CNF-3 | P0 | No chart code runs, and no extra memory is held, for a result that has no chart and is never charted. |

## 6. Acceptance vectors (first set)

1. `StormEvents | summarize count() by bin(StartTime, 1d) | render timechart`: one `TimeLine` chart bound to table 0, datetime X, one series, ticks on day boundaries.
2. The same with `by State`: one series per state, legend lists them, hover crosshair lists every visible series at the nearest day.
3. `render barchart` and `render columnchart` with a string X and two numeric columns: grouped bars, two series named after the columns.
4. `render timechart with (ymin=0, ymax=100, title="x", ytitle="y")` maps to the matching options; `NaN` limits stay unset.
5. A `.ktt` with two charts (the `kusto-testing/results.ktt` shape): two chart tabs; edit the second, wait for the debounce, reopen: the first is byte-identical apart from formatting, the second has the edit.
6. A file with a `Sankey` chart and an unknown option on a `Line` chart: both survive a save unchanged.
7. A chart with only nulls in Y, a chart on a table with no numeric column, a `tableName` that does not exist: each says why on that chart; the others still draw.
8. A read-only file: the edit shows, no write happens, the user is told once.
9. A 100,000-row table with 20 series: opens, hovers and toggles a legend entry without a visible stall.

## 7. Where Zed differs from VS Code

- The panel's chart edits are saved to the run's history file (VS Code keeps them in memory). They are lost when history pruning deletes the run (D3).
- Thinning keeps peaks (CDM-7); VS Code takes every Nth point.
- Times default to UTC (CVW-4), consistent with the grid; VS Code hands the text to Plotly.
- `render` is read from the response, not inferred from the query text.
- Text is never rotated, because GPUI cannot paint it: the title of the vertical axis sits above the axis, and a crowded axis shows fewer labels. `xTickAngle` and `yTickAngle` are kept in the file but not applied.
- The options that name an axis (`xAxis`, `yTitle`, `xShowGrid`, ...) mean the axis on the screen, as in VS Code, so for a horizontal bar chart `x` is the axis of the values.
- The tooltip of a line chart snaps to the nearest X anywhere in the plot, not only near a point.

## 8. Decisions needed

| # | Question | Recommendation |
| --- | --- | --- |
| D1 | Which types are P0? | Line, TimeLine, Bar, Column. Stacked and area as P1 (they cost little once grouped bars exist). Pie, scatter and the rest later; Graph, Sankey, TreeMap, 3D, TimePivot, Ladder, Plotly, Card, anomaly are out. |
| D2 | Where does the editor live: a side pane inside the chart tab, a popover, or a dock panel like Row Details? | A side pane inside the chart tab, collapsible, so it works identically in the Results panel and a file tab. A dock panel would have to follow whichever viewer is active. |
| D3 | History files are one per run and are pruned. Should chart definitions survive a re-run of the same query? | Not in the first release. Note that re-run (RUN-8) is not built yet. Revisit by keying saved chart specs on the query text, or by writing `render` into the query. |
| D4 | Time zone for datetime axes. | UTC, with a setting. |
| D5 | Does `render` open the chart tab first? | Yes, as VS Code does for the document view; a setting turns it off. |
| D6 | Is a hover that selects the grid row wanted (CIN-4)? | Optional; skip until asked. |
| D7 | The VS Code `binSize`/`aggregation`/`maxSeries`/`maxPointsPerSeries` extras. | Keep them in the file; draw `maxSeries` (CDM-6); leave binning to `bin()` in the query. |

**Taken:** all of D1 to D7 as recommended. D2 became a collapsible pane beside the chart; D3 is still open in practice (a re-run writes a new history file that has only the charts its own `render` asks for).

## 9. Delivery plan

Two short spikes first, because they remove the two biggest unknowns.

- **S1, a real `render` response.** Run one `render timechart` query against the help cluster and keep the raw v2 frames as a test fixture. Confirms the `@ExtendedProperties` shape taken from the SDK source here, and replaces the one-column fixture in `response.rs`.
- **S2, a GPUI line chart.** Three series by about 2,000 points with linear and datetime axes, theme colours and a hover crosshair, painted on a `canvas` with `PathBuilder`. Measures path building cost per frame and whether stroked polylines need caching. Decides CNF-1 before the model is written.
- Optional **S3**, Mermaid `xychart` in the Markdown preview, for a five-minute look at a bar or line chart from a query. Throwaway.

Then, in order, each a separate commit with tests:

1. `kusto_results`: chart model, round trip with unknown keys (CFM).
2. `kusto_client`: read `@ExtendedProperties` into charts (CSR-1, CSR-2).
3. `kusto_results`: data step and scales (CDM, tick generation), with scene tests.
4. `kusto_results_ui`: read-only Line and TimeLine tab (CVW, CIN-1, CIN-2).
5. Bar and Column, then the stacked variants.
6. Editor, Add and Remove Chart, saving (CED, CSR-3, CSR-4, CPL).
7. Copy Chart (CXP-1) and settings (CST-1).
8. Update the workbench spec section 1 table and the status doc.

## 10. Status

Built, with tests (`cargo test -p kusto_client -p kusto_results -p kusto_results_ui --lib`): CFM-1 to CFM-5; CSR-1 to CSR-5; CDM-1 to CDM-7 and CDM-9; CVW-1 to CVW-7, CVW-9 and CVW-10, and the stacked forms of bars and columns (CVW-2 except the area types); CIN-1 and CIN-2; CED-1 to CED-4; CPL-1 and CPL-2, and CPL-3 (Add chart binds to the table of the tab you are on); CXP-1; CST-1 except the choice of a time zone.

Not built: CIN-3 (zoom), CIN-4 (hover selects the grid row), CVW-8 (`DualAxis`, `SeparatePanels`, `SeparateCharts`; the option is read and saved), CDM-8 (binning; combining rows at the same X is built), CED-5 (reset to defaults), CXP-2 (save a picture), area, pie and scatter, a local-time axis, CSR-6 (keeping a chart across runs), and CNF-1 was not measured: the paths of each series are built again on every paint, including one caused by moving the pointer.

The spikes: S1 was run (the real `render` response is in `crates/kusto_client`'s tests and the live test). S2 became the scene: layout and ticks are tested without a window, written as SVG and looked at for a set of sample charts, and drawn by a GPUI canvas whose paint code has run only in GPUI's test platform, so the tab has not been looked at in a real window.
