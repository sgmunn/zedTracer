# Kusto Results Workbench for Zed: design

Status: draft for review. Companion to [kusto-results-workbench-feature-spec.md](kusto-results-workbench-feature-spec.md), which holds the numbered functional requirements (`GRD-3`, `SRT-1`, ...). This document explains the shape of the solution: goals, what the VS Code extension does, the conceptual model, how the surfaces fit together, and the UI designs. It stays at the functional level. It does not choose crates, types, traits or modules; that is the next step once this and the spec are agreed.

It builds on, and does not replace, [zed-kusto-design.md](zed-kusto-design.md) (the earlier handoff on fork boundary, .NET language server, and the staged plan for the native grid).

## 1. Goals and non-goals

### 1.1 Goals

1. Reproduce, in Zed, the parts of the KustoTraceTools VS Code extension that make query results useful for **investigating large, structured diagnostic traces**: a fast typed results grid with sorting, filtering and search; a Row Details inspector that makes long values readable; exception and multipart handling; and the structured activity view.
2. Keep results **faithful**. Formatting is a display concern; the data the server returned is never changed, and files saved from Zed stay readable by the VS Code fork.
3. Keep common investigation actions **next to the results**: no KQL rewrites and no copying into another tool to read an exception or reassemble a message.
4. Make the useful path the default for large payloads, and let the user opt out of presentation choices (for example word wrap).

### 1.2 Non-goals for this effort

- Charts, `render` visualisations, graph and time-pivot views. They are deliberately deferred.
- The connections explorer, scratch pads, formatter settings, Copilot schema tools.
- Editing queries (highlighting, completion, diagnostics). Handled by the Kusto extension spike under `extensions/kusto`.
- Choosing the execution transport. This document only states what the results side needs from it (spec 4.1).
- Detailed code structure. Not yet.

### 1.3 Principles (carried over from the VS Code fork)

- Keep query source and returned data faithful; formatting is client display.
- Prefer conservative transformations that preserve diagnostic evidence (multipart assembly and stack trimming never guess).
- Keep a narrow contract between "the results" and "the thing that draws them" so the grid can evolve without touching run orchestration.
- Make row identity stable so users can always tell what they are looking at (source row numbers never move).

## 2. What the VS Code extension does today

### 2.1 Summary

KustoTraceTools is a locally maintained fork of Microsoft's Kusto Explorer for VS Code. Its results workflow is: run a query, look at rows in a grid, select one or more rows, and inspect the details without leaving the editor. Most of what the fork adds sits on the results side.

| Layer | What it is | Relevance here |
| --- | --- | --- |
| Language server (C#) | Kusto language features, schema, query execution, `ResultData` serialisation. | Execution and authoring; out of scope except for the shape of a result. |
| Results orchestration | Decides where a run's output is shown (bottom panel, beside, main), keeps history, saves and opens `.ktt`. | Behaviour to reproduce (spec 4.1, 4.2, 4.13). |
| Grid | An HTML data table in a webview, with a script injected around it for selection, sort, resize, reorder, copy, drag and drop. | The behaviour to reproduce (spec 4.3 to 4.9). The implementation is not something to copy. |
| Fork extensions to the grid | Column filters, severity highlighting, large-result loading overlay, structured activity layout. | Reproduce (4.7, 4.10, 4.12). |
| Row Details | A separate view bound to the selected rows. | Reproduce (4.11). |
| Query parameter profiles, agent handoff, cancellation, badge fixes | Smaller fork features around the same workflow. | Cancellation and badge in 4.1 / 4.2; profiles and agent handoff deferred (spec 5). |

### 2.2 Fork-authored versus upstream

The upstream extension (authored mostly by Matt Warren, per `git log`) supplies the query editor, connections, history, charts and a basic grid: typed columns, cell, row and column selection, copy, column resize and reorder with saved state, and drag-and-drop as a `datatable` expression. The fork adds the investigation features:

| Fork addition | Commit trail (topic) |
| --- | --- |
| Row Details inspector, find, word wrap | "Add row details inspector", "Add row details search" |
| Multipart message assembly | "Assemble multipart messages in row details" |
| Exception call-stack formatting | "Improve exception details formatting" |
| JSON formatting and highlighting in details | "Format JSON values in row details", "Highlight JSON in row details" |
| Typed column filters | "Add typed results grid column filters", "Fix results grid column filtering" |
| Three-state column sort, numeric sort fix | "Add three-state result column sorting", "Fix boolean representation and numeric sorting" |
| Severity row colours | "Add configurable severity row highlighting" |
| Structured activity view and severity colouring | "Add activity hierarchy projection", "Add structured activity results view", "Color structured activity nodes by own event severity" |
| Large-result loading overlay, page size, gutter resize | "Show loading state for large result grids", "Add configurable results grid page size", "Make results row gutter resizable" |
| Query cancellation, results badge fixes | `docs/QUERY_CANCELLATION.md`, `docs/RESULTS_BADGE.md` |
| Query parameter profiles, agent access to results | `docs/FORK_DESIGN.md` |
| `.ktt` result files (legacy `.kqr` still opens) | "Save KustoTraceTools results as ktt" |

### 2.3 Lessons to take, and traps to avoid

Take:

- **A narrow boundary around the grid.** The fork put all of its grid work behind a small provider interface (create a grid from a table plus saved view state, report the selected source rows, support copy, report column layout, release handlers). Everything that shows results, live or saved, goes through it. The Zed design keeps the same idea: one results component, reused by the panel, editor tabs and saved files.
- **Pure, testable transformation layers.** The activity projection, filter matching, call-stack formatting and multipart assembly are all plain data-in, data-out logic covered by unit tests. They should stay independent of the drawing surface in Zed too.
- **Display-only rules.** Nothing formatted for display is ever written back.
- **Conservative behaviours.** Multipart assembly, orphan/cycle handling and "never erase a stack" all choose to show more rather than guess.

Avoid:

- Grid behaviours added as strings of script injected into a page. It leads to the quirks listed in spec section 7 (three-state sort as a workaround, selection state duplicated across layers, cross-tab handlers).
- A selection model tied to display positions and cleared on every sort, which forces users to re-select and hides the source-row identity that the inspector needs.
- Implicit or unverified semantics (multi-word search, copy order, timezone of filter values). The spec fixes each of these.
- Coupling structured view availability to where a result is displayed.

## 3. Where Zed stands today

State of the fork at commit `0ee22839ac` (`feature/kusto-syntax-spike`), functional view only:

| Piece | State | Consequence |
| --- | --- | --- |
| `extensions/kusto` | Dev extension: `.kql` highlighting (tree-sitter), a local .NET language server for completion, hover and syntax diagnostics, and offline schema completion from `.kusto-schema.json`. No cluster access, no execution. | Authoring is progressing independently. No results exist yet. |
| `crates/investigation` | Prototype: an `InvestigationPanel` in the right dock and a custom viewer for `.trace` files. Both display **static sample data**, not parsed content. | Proves that a panel and a custom workspace item can be registered. It is not a results surface. |
| `ui` data table component | An existing generic table with resizable columns, pinned columns, striped rows, virtualised rows, and hover behaviour. From its public surface I found no support for typed columns, sort, filter, cell-range selection, header actions or column reordering; this has not been confirmed by using it. | A candidate foundation, to be measured against spec NFR-1 before adoption. The existing design doc already asks for this check. |
| Query execution | Not built. | Results must first be fed from files. |

The most valuable enabler is a data source that already exists: **real `.ktt` files** produced by the VS Code fork. They contain real traces, real exception JSON and real multipart rows. Opening `.ktt` in a native viewer needs no execution transport, so results, sorting, filtering, the inspector and the structured view can all be built and verified before the .NET execution service exists. That also makes the compatibility requirement (PER-2) a day-one test instead of a later migration concern.

### 3.1 Capabilities to verify in Zed before committing

These decide how much can be reused versus built. Each is a small spike with a yes/no answer.

| Need | Requirement | Question |
| --- | --- | --- |
| Virtualised rows with 15 to 30 columns and horizontal scroll at 100k to 500k rows | NFR-1, GRD-* | Does the existing table hold frame rate, or does the grid need its own rows layer? |
| Header hit areas beyond click: sort, funnel, resize, drag-reorder | SRT, FLT, COL | Can the header be composed from several distinct interactive regions? |
| Rectangular cell selection with drag, autoscroll, shift extend | SEL | Is there a selection primitive, or is it built above the table? |
| Anchored popover that stays on screen, with text inputs and selects | FLT-2 | Available as reusable UI? |
| Selectable, searchable, highlightable long text with mixed colours and wrapping | RDT-5, RDT-6, JSN-3 | Can text be selected, copied, wrapped or not, and highlighted across coloured runs? |
| Tree with expand, collapse, keyboard and tinted nodes | ACT-7..11 | Reuse the existing tree list, or extend? |
| Draggable and keyboard-operable splitter | ACT-12 | Available? |
| Rich clipboard (HTML plus text) on all three platforms | CPY-1, CPY-5 | Does the clipboard API support multiple formats? |
| Placement of a new right-dock panel next to the existing Investigation panel | RDT | Do two panels coexist in the right dock? |

## 4. Conceptual model

This is the vocabulary the rest of the design uses. It is a model of the data and the user's state, not of code.

```
Run ─────────► Result set ──────► Table (1..n)
 (identity,     (query, cluster,     ├─ Columns: ordered, name + Kusto type
  status)        database, timing,   └─ Rows: immutable, each with a source row index
                 parameters)
                        │
                        └──► Table view state (saved):  column order, widths, gutter width

Displayed table (transient view state, per open grid)
   source rows ─► [activity scope] ─► [search] ─► [column filters] ─► [sort] ─► [page] ─► visible rows
                   (structured only)

Selection (per grid): a rectangle of cells, expressed as visible positions, resolved to:
   set of source row indexes  ─────────────────► Inspector subject
```

### 4.1 Invariants

1. **Source data is immutable.** A result set, once complete, is not modified by anything the user does in a view.
2. **Row identity is the source row index.** It is what the gutter shows (plus one), what selection publishes to the inspector, what breaks sort ties, and what the structured view keeps when it regroups rows.
3. **The display is a pure function of source data and view state.** Given the same table, filters, search and sort, every open grid shows the same rows in the same order.
4. **Filter and sort work on an index of source rows.** They do not copy or rewrite values. Structured and ordinary views of the same table share the same rows.
5. **Everything that changes text for display is separate from data.** JSON formatting, call-stack trimming and multipart assembly produce display text; they never feed back into copy-as-data, saved files or filters. (The inspector copies what it displays; grid copy copies data.)
6. **Ownership follows the run.** A result set, its badge, its tabs and its selection belong to one run. Nothing arriving late from another run can change them.

### 4.2 The processing order of a grid

The order matters and is fixed by the spec:

1. Start with all source rows of the table.
2. Structured view only: keep the events of the selected activity.
3. Apply search (all terms must appear in some cell of the row).
4. Apply column filters (all columns' filters must match).
5. Sort by the chosen column with source row index as the tie-break; no sort means source order.
6. Page.

Sorting therefore reorders the rows that already passed search and filters, and the footer count is the count after step 4.

## 5. Surfaces and information architecture

### 5.1 The four surfaces

| Surface | Where | Content |
| --- | --- | --- |
| **Results panel** | Bottom dock | The output of the latest run in the panel: tabs, toolbar, grid, structured view. Title carries the row-count badge. |
| **Result tab** | Editor area (beside or in place) | The same content in a pane item. Used for `.ktt` files, history entries, and (optionally) each completed run. |
| **Row Details** | Right dock panel | The inspector bound to the current row selection of whichever grid was last used. |
| **Editor** | Editor area | The query. Hosts the run indicator, Cancel, and error range highlight. |

The Results panel and the Result tab are two *placements* of one results component. A user setting decides where a live run goes (spec SET-1, SET-2). A saved `.ktt` always opens as a Result tab.

### 5.2 Why Row Details is its own right-dock panel

It has to follow the selection from any grid: panel, editor tab, structured tab. If it lived inside a grid's surface it would need a copy per surface and would disappear when the panel is hidden, exactly when the user wants to read a long value in a maximised editor. VS Code does the same in effect (a sidebar view, movable). It also lets users pin it beside the editor while the results sit at the bottom, which is the layout in W-1. It shares the right dock with the existing Investigation panel prototype and does not depend on it.

### 5.3 Event flow between surfaces

```
[Editor] --Run--> [Run] --complete--> [Results panel | Result tab]
                    │ cancel                         │
                    ▼                                │ row selection changes
              indicator cleared,                     ▼
              nothing displayed              [Row Details] renders the subject
                                                     ▲
                        Structured tab activity -----┘ (selecting an activity clears selection)
```

- A run publishes a complete result set once; there is no streaming display.
- The grid publishes *only the set of selected source row indexes and the table* to Row Details. Row Details does everything else (multipart, JSON, call stacks).
- Row Details ignores repeated publication of an identical selection (no redraw, no lost scroll position).

### 5.4 Layout rules

- The results panel is the default destination. Editor placement is opt-in.
- When there is no query editor active, the Results panel may be hidden (PNL-11); opening a `.ktt` shows results without requiring an editor.
- The Row Details panel is empty-state until a row is selected; it never blocks or steals focus.

## 6. UI designs

Wireframes are low fidelity. They fix content, order, states and labels; visual styling follows the active Zed theme. Boxes drawn with `+---+` are surfaces; text in `[ ]` is a control; `<...>` is a placeholder for data.

### W-1. Overall layout

The default arrangement while investigating: the query above, results at the bottom, Row Details pinned at the right.

```
+------+--------------------------------------------------------------+-----------------------+
| Proj | investigate.kql                                          [x] | ROW DETAILS           |
| ects |--------------------------------------------------------------|-----------------------|
|      |  1  declare query_parameters(raid:string);                   | AsyncTrace            |
| .    |  2  ASTrace                                                  | Row 4012 . 27 fields  |
| .    |  3  | where RootActivityId == raid                           | [ Find in row       ] |
|      |  4  | where Level <= 3                                       | [ Wrap lines: On ]    |
|      |  5  | order by Timestamp asc                                 |-----------------------|
|      |     [> Run]  [ Cancel ]            Last run 10:42 . 1.8 s    | Timestamp   datetime  |
|      |--------------------------------------------------------------| 2026-09-30T10:42:11Z  |
|      | RESULTS 1,240 | Data | Data - Structured | Query   [C][S][W]| Level          long   |
|      |--------------------------------------------------------------| 2                     |
|      | help.kusto... / Samples . 1,240 rows . 1.8 s                 | Message       string  |
|      |  # | Timestamp        | Level | Message               |...   | {                     |
|      | 4012| 10:42:11.402Z   |  2    | Timeout reading ...   |      |   "type": "TimeoutEx",|
|      | 4013| 10:42:11.410Z   |  3    | Retry 1/3 scheduled   |      |   "callStack": "at ...|
|      | ...                                                          | ...                   |
|      | Showing 1 to 1000 of 1,240 rows               [ < 1 2 > ]    |                       |
+------+--------------------------------------------------------------+-----------------------+
```

### W-2. Results panel anatomy

```
+---------------------------------------------------------------------------------------------+
| RESULTS  (1,240)                                                                            |
+---------------------------------------------------------------------------------------------+
| [Data] [Data - Structured] [Query]                             [Copy] [Search] [Save As...] |
+---------------------------------------------------------------------------------------------+
| help.kusto.windows.net / Samples   1,240 rows   1.8 s   started 10:42:09                    |  <- PNL-7
+---------------------------------------------------------------------------------------------+
| [ Clear all filters ]                                              Search: [______________] |  <- FLT-9, SRC-1
+---------------------------------------------------------------------------------------------+
|  #  | Timestamp (dt)  ▲ [F] | Level (long) [F] | Message (string) [F]  | ...                 |
|-----+-----------------------+------------------+-----------------------+---------------------|
|  1  | 2026-09-30T10:42:09Z  |  4               | Request started       |                     |
|  2  | 2026-09-30T10:42:10Z  |  4               | Connecting to sql-... |                     |
|  3  | 2026-09-30T10:42:11Z  |  3               | Retry 1/3 scheduled   |   <- tinted (SEV-1) |
|  4  | 2026-09-30T10:42:11Z  |  2               | Timeout reading ...   |   <- tinted         |
+---------------------------------------------------------------------------------------------+
| Showing 1 to 1000 of 1,240 rows                     Rows per page [1000 v]   [ < 1 2 > ]    |
+---------------------------------------------------------------------------------------------+
   [F] = funnel (highlighted when a filter is active)     ▲ = sorted ascending
```

Tab labels: one table with no extras hides the tab bar; several tables show `<name> (<rows>)`; a structured tab is `<name> - Structured (<rows>)`.

### W-3. Grid anatomy

Shows the gutter, sort state, funnels, severity tint, and selection.

```
        corner
          |    sort indicator            active filter (funnel filled)
          v         v                        v
+------+------------------+-----------+--------------------+--------------------------+
|  #   | Timestamp   ▲ [F]| Level [*F]| Region        [F]  | Message              [F] |
+------+------------------+-----------+--------------------+--------------------------+
|  1   | 10:42:09         |  4        | westus2            | Request started          |  green tint (4)
|  2   | 10:42:10         |  4        | westus2            | Connecting to sql-01     |
|  3   | 10:42:11         |  3        | westus2            | Retry 1/3 scheduled      |  amber tint (3)
| [4]  |[10:42:11        ]|[ 2       ]|[westus2           ]|[Timeout reading from ...]|  selected row
|  5   | 10:42:12         |  5        | eastus             | Verbose heartbeat        |  blue tint (5)
+------+------------------+-----------+--------------------+--------------------------+
   ^                                                  ^
   gutter: source row number, never moves        column edge: drag to resize
   click = select row; shift+click = extend      drag a header = reorder (drop line shows)
   corner: click = sort by row number, shift+click = select all
```

Header interactions, exactly as the spec requires (SRT-1, SEL-4, COL-2, COL-3):

| Gesture on a data header | Result |
| --- | --- |
| Click | Sort: ascending, then descending, then original order |
| Shift+click | Select column; shift+click another to extend |
| Click the funnel | Open the filter popover |
| Drag the right edge | Resize column |
| Drag the header | Reorder column (drop line) |

### W-4. Filter popover

Anchored under the funnel. Live: every change applies after about 150 ms.

String column, two conditions:

```
+----------------------------------------------+
| Filter Message                               |
| [ Match all conditions             v ]        |
| [ Contains       v ] [ timeout            ]   |
| [ Does not contain v ] [ retry            ]   |
| [ Remove condition ]  [ Clear ]               |
+----------------------------------------------+
```

Numeric column, one condition:

```
+----------------------------------------------+
| Filter Level                                 |
| [ Less than or equal  v ] [ 3            ]    |
| [ Add condition ]  [ Clear ]                  |
+----------------------------------------------+
```

Datetime and boolean columns:

```
+--------------------------------------------+    +--------------------------------------------+
| Filter Timestamp                           |    | Filter Succeeded                           |
| [ After         v ] [ ISO date/time     ]  |    | [ Is false       v ]  (no value input)     |
| [ Add condition ]  [ Clear ]               |    | [ Add condition ]  [ Clear ]               |
+--------------------------------------------+    +--------------------------------------------+
```

Operator sets by type are in spec FLT-4. Closes on Escape or outside click.

### W-5. Row Details, single row

The example shows a JSON field, an exception call stack, and find highlighting. Header controls stay pinned while the fields scroll.

```
+---------------------------------------------+
| ROW DETAILS                                 |
+---------------------------------------------+
| AsyncTrace                                  |
| Row 4012 . 27 fields                        |
| [ Find in row: timeout          ]           |
| [ Wrap lines: On ]                          |
+---------------------------------------------+
| Timestamp                        datetime   |
| 2026-09-30T10:42:11.4020000Z                |
|---------------------------------------------|
| Level                            long       |
| 2                                           |
|---------------------------------------------|
| Message                          string     |
| Failed to read from sql-01                  |
|---------------------------------------------|
| Exception                        dynamic    |
| +-----------------------------------------+ |
| | {                                       | |
| |   "type": "SqlTimeoutException",        | |
| |   "message": "[[Timeout]] expired",     | |   [[ ]] = find match highlight
| |   "callStack": "at Contoso.Db.Query()   | |
| |     in Db.cs:line 88                    | |
| |   at Contoso.Api.Handler.Run()          | |
| |     in Handler.cs:line 132"             | |
| | }                                       | |
| +-----------------------------------------+ |
|---------------------------------------------|
| CorrelationVector                string     |
| null                                        |
+---------------------------------------------+
```

Colours inside the JSON block: property names, strings, numbers, booleans and `null` each use a distinct theme colour (JSN-3).

**Call stack before and after (EXC).** The same stack as it appears in the raw value and in the inspector:

```
RAW value (one very long line, real path, framework frames interleaved):
  at Contoso.Db.<QueryAsync>d__12.MoveNext() in D:\a\_work\1\s\Core\Db.cs :line 88 at
  System.Runtime.CompilerServices.TaskAwaiter.ThrowForNonSuccess() at System.Threading.
  Tasks.Task.Execute() at Contoso.Api.Handler.<>c__DisplayClass3_0.<Run>b__0(Task t) in
  D:\a\_work\1\s\Core\Handler.cs :line 132 at System.Net.Http.HttpClient.SendAsync()

SHOWN in the inspector (one useful frame per line, filename:line only):
  at Contoso.Db.QueryAsync() in Db.cs:line 88
  at Contoso.Api.Handler.Run() in Handler.cs:line 132
```

### W-6. Row Details, assembled multipart message

Three rows selected whose `Message` values are `1/3:...`, `2/3:...`, `3/3:...`.

```
+---------------------------------------------+
| ROW DETAILS                                 |
+---------------------------------------------+
| AsyncTrace                                  |
| 3 selected rows . multi-part message        |
| assembled                                   |
| [ Find in row                   ]           |
| [ Wrap lines: On ]                          |
+---------------------------------------------+
| Message              merged 3-part message  |
| +-----------------------------------------+ |
| | {                                       | |
| |   "type": "AggregateException",         | |
| |   "innerExceptions": [ ... ]            | |
| | }                                       | |
| +-----------------------------------------+ |
+---------------------------------------------+
```

Not assembled (incomplete, mismatched, duplicate, or ordinary rows) falls back to W-7.

### W-7. Row Details, empty and multiple-selection states

```
+---------------------------------------------+     +---------------------------------------------+
| ROW DETAILS                                 |     | ROW DETAILS                                 |
+---------------------------------------------+     +---------------------------------------------+
|                                             |     | AsyncTrace                                  |
|  Select a result row to inspect its values  |     | Row 4012 . 27 fields                        |
|  here.                                      |     | 2 rows selected - showing the first         |
|                                             |     | [ Find in row ]  [ Wrap lines: On ]         |
+---------------------------------------------+     +---------------------------------------------+
   empty (RDT-2)                                       several rows, not a multipart message (RDT-7)
```

### W-8. Structured activity view

Available when the table has `CurrentActivityId` and `ParentActivityId`. The tree is on the left, the grid on the right shows only the selected activity's events.

```
+---------------------------------------------------------------------------------------------+
| [Data] [Data - Structured]                                                                  |
+---------------------------------------------------------------------------------------------+
| Activities            [Deepest . 4] |#| Timestamp        | Level | Message                  |
|                                     +-+------------------+-------+--------------------------+
| v . Request (12)               |    | 1| 10:42:09         |  4    | Request started          |
|   v  Auth (3)                        | 2| 10:42:09         |  4    | Token validated          |
|      . Token  (2)                    |...                                                    |
|   v ! Provision (9)  [orange]        | Events of the selected activity ("Provision")         |
|      > ! CreateDb (6) [red] ↓2       |                                                       |
|           . Retry (4) [amber, ! ]    |                                                       |
|   > Notify (2)                       |                                                       |
| . Startup (5)                        |                                                       |
|                                     ||<-- splitter (drag, or Left/Right, Home/End)          |
+---------------------------------------------------------------------------------------------+
  tree pane: default 340 px, min 180 px          grid pane: at least 280 px
```

Tree node anatomy (ACT-7):

```
 [disclosure] [!] MarkerName   3f2a9c1e-...   (6)   ↓2
      |         |     |            |            |     |
      |         |     |            |            |     depth badge: 2 levels below, tooltip lists branch size
      |         |     |            |            event count
      |         |     |            activity id
      |         |     MarkerName of the first event (when the table has that column)
      |         warning triangle: only for the activity's OWN warning/error/critical event
      v expanded, > collapsed, . leaf
```

Node colour (ACT-9), using the same palette as the grid:

```
 final event level:  critical/error/warning ........ node tinted at full strength          [ red  ]
 final event normal/verbose, earlier warn/err ...... node tinted at 30% (a "handled" issue)  [ pale red ]
 no issue, final event normal/verbose .............. node tinted with that level at full    [ green/blue ]
 no severity column, or unusable value ............. uncoloured
```

Interactions: click selects; click the disclosure or double-click toggles; Right expands, Left collapses or goes to the parent; **Deepest** cycles through the deepest activities, expanding their ancestors and scrolling them into view.

### W-9. Panel states

```
Idle (no results yet)             Running                                Error
+--------------------+            +---------------------------------+   +-------------------------------+
| RESULTS            |            | RESULTS                         |   | RESULTS  (!)                  |
|                    |            |   Running query...   [ Cancel ] |   | X  Semantic error: 'Foo'      |
|      no results    |            |   (editor shows Cancel too)     |   |    Details ...                |
+--------------------+            +---------------------------------+   +-------------------------------+

Zero rows                                     Large table loading (non-blocking, GRD-9)
+------------------------------------------+   +--------------------------------------------------+
| RESULTS  0                               |   | RESULTS  (412,000)                               |
| #  | Timestamp | Level | Message         |   |  (o) Sorting 412,000 rows...                     |
|              No results                  |   |  grid stays visible and scrollable               |
| Showing 0 to 0 of 0 rows                 |   +--------------------------------------------------+
+------------------------------------------+
```

Cancelled runs leave the previous display untouched and show nothing (RUN-3).

### W-10. Grid context menu

```
+--------------------------+
| Copy                     |
| Copy as Markdown         |
| Copy as HTML             |
| Copy as datatable        |
|--------------------------|
| Save As...               |
+--------------------------+
```

Applies to the current selection, or the whole table when nothing is selected.

### W-11. Tab bar and badge variants

```
One table, no extras        (no tab bar, grid fills the panel)     RESULTS  1,240

One table with structured   [Data] [Data - Structured]             RESULTS  1,240

Several tables              [PrimaryResult (1,240)] [Stats (4)]    RESULTS  1,244   (sum of tables)

With query text             [Data] [Data - Structured] [Query]

Empty result                (grid header + "No results")           RESULTS  0
Error                       (error view)                           RESULTS  (!)
```

### W-12. Settings (indicative)

```
Results
  Location            [ Bottom panel v ]    panel | beside | main
  Editor result mode  [ New tab      v ]    new tab | reuse       (when Location is not panel)
  Page size           [ 1000         ]
  Severity colours
     Critical [ #f14c4c40 ]   Error  [ #f4877133 ]   Warning [ #cca7002e ]
     Normal   [ #89d1851f ]   Verbose[ #75beff17 ]     (empty = theme default)
Row Details
  Extra call stack frame prefixes to hide  [ Contoso.Infrastructure. , ... ]
```

## 7. Behaviour models

### 7.1 Sort state per grid

```
                  click                    click                    click
   unsorted ─────────────► ascending ─────────────► descending ─────────────► unsorted
      ▲                        │                          │                    (original order)
      │                        └─── click another column ─┴──► that column ascending
      └──── corner cell / Restore result order ─────────────────────────────────────┘
```

Exactly one column is sorted at any time (multi-column sort is P2). "No header shows an indicator" is the definition of original order.

### 7.2 Selection model

A selection is a single rectangle: anchor and focus in visible positions. The grid resolves it to a set of **source row indexes** for the inspector and copy.

| Gesture | Effect |
| --- | --- |
| Click cell | Rectangle of one cell (toggle off if it was the only one selected) |
| Shift+click / drag | Extend rectangle |
| Click / drag on gutter | Whole rows |
| Shift+click on header | Whole column(s) |
| Shift+click on corner | Whole table (toggle) |
| Sort, reorder, page change, filter or search change, activity change | Clear |

Consequences the design accepts: multipart parts are only selectable together when adjacent in the current view (users filter first). Non-contiguous row selection (Q-4 in the spec) is the proposed extension; it changes the selection from "a rectangle" to "a set of rows plus a rectangle of columns" and does not alter anything else in the model.

### 7.3 Filter model

A filter belongs to a column. It has a type (taken from the column), a join (`and` or `or`), and one or two conditions, each an operator and, when needed, a value. Search adds one more predicate over all data columns. A row is visible when it satisfies the search and **every** column filter. Filters are not saved. Evaluation is over all rows, before paging, and is superseded when the user types again.

### 7.4 Inspector subject resolution

```
selection changes
   │
   ├─ no rows ─────────────────────────────────────► empty state
   ├─ rows form ONE complete multipart message in ≥1 columns ─► assembled view (one block per qualifying column)
   ├─ one row ────────────────────────────────────► field list for that row
   └─ several rows, not multipart ─────────────────► first row + "N rows selected" note

for every value shown:
   JSON? (object/array, or string starting { or [ that parses) ─► pretty JSON, syntax colours,
         escaped newlines shown as line breaks, every "callStack" string trimmed
   else ─► plain text (null shown as `null`)
```

### 7.5 Structured projection

Group by activity; derive the parent per activity; form a forest; repair anomalies without dropping rows (orphan, conflicting parents, cycles); order by first appearance; compute per-activity severity outcome, branch size and depth. The tree is a view of that projection and the grid is the standard grid, scoped to the selected activity's rows. The severity outcome uses only an activity's own events (final event, worst earlier event), never its descendants.

### 7.6 Result ownership and late arrival

Every run has an identity. Panel, badge, tabs, selection and inspector content are tied to it. Rules:

- Publishing a result set happens once, after execution ends and cancellation is no longer possible.
- A response from a cancelled or superseded run is discarded and changes nothing, including the badge.
- With the `new tab` mode each completed run has its own tab, so concurrent runs cannot collide; with the panel mode the most recently *completed* run wins.

## 8. Data and persistence

- **Format:** `.ktt`, the VS Code fork's JSON result file. Read and written as-is, with unknown properties (charts) preserved. Legacy `.kqr` opens. See spec PER-2 and PER-3 for the fields.
- **What is saved:** the result set (query, cluster, database, parameters, timings, request id, tables) and per-table column layout. **What is not:** search, filters, sort, selection, scroll position, inspector state.
- **Auto-save of layout:** in a saved result, layout changes are written back automatically. There is no dirty state to manage.
- **History:** every completed run is written as a `.ktt` to an app-managed location and indexed for a history list (most recent 200). History is a P1; it also underpins the tab-per-run mode.
- **Types on the wire:** dates arrive as ISO 8601 text, timespans as `[-][d.]hh:mm:ss[.fffffff]`, guids as text, bools as booleans, dynamic as JSON (objects/arrays) or JSON text. The grid interprets them by column type when sorting and filtering and never guesses from the text alone.
- **Volume:** the whole table is held in memory in the process showing it; there is no server-side paging. Views reference rows instead of copying them (spec NFR-8).

## 9. Quality and performance approach

- **Baseline first.** Before setting numeric targets, measure on named hardware with the workloads in `zed-kusto-design.md` (100k to 500k rows, 15 to 30 columns; nulls, duplicates, wide strings, dynamic values): open, scroll, sort, filter, search, memory.
- **Logic is tested without UI.** Activity projection, filter matching, typed comparison, call-stack formatting, multipart assembly and TSV/Markdown/HTML formatting are pure and get table-driven tests using the vectors in spec section 6. The VS Code fork's unit tests are the seed for those vectors.
- **Interactions are exercised in the running app** (selection drag, header gestures, popover placement, splitter, keyboard paths) using Computer Use, as the repository's working instructions already require for behaviour unit tests cannot establish.
- **Compatibility checks.** Round-trip a corpus of real `.ktt` files produced by the VS Code fork: open in Zed, resize a column, confirm the file is still valid for VS Code and unchanged elsewhere.

## 10. Delivery plan

The plan follows the staged approach in `zed-kusto-design.md` and reorders it around what can be built from files alone. Each phase is independently demonstrable.

| Phase | Deliverable | Spec IDs | Notes |
| --- | --- | --- | --- |
| A | Result viewer for `.ktt` (custom item), typed table with gutter, paging, column resize, selection, copy (TSV, Markdown, HTML), context menu | GRD, SEL, COL-1..4, CPY-1..7, PER-2..4 | Answers the capability checks in section 3.1 for table, selection and clipboard. First real 100k to 500k baseline. |
| B | Sorting, filtering, search, severity colours, loading feedback | SRT, FLT, SRC, SEV, GRD-9 | Filter popover and header interactions. Performance targets set from the phase A baseline. |
| C | Row Details panel | RDT, JSN, MPM, EXC | Right-dock panel; multipart, JSON colouring, call stacks. Independent of execution. |
| D | Structured activity view | ACT | Tree, splitter, severity outcome, Deepest. |
| E | Results panel in the bottom dock with badge, tabs, states; run lifecycle, cancellation, history | PNL, RUN, PER-5, PER-1 | Requires the execution service from the main design doc. Live results reuse the same component built in A to D. |
| F | Deferred and P1 leftovers | Spec section 5, remaining P1/P2 | Query parameter profiles, agent handoff, keyboard grid navigation, non-contiguous selection, user call-stack noise list. |

Exit criteria for A to D: for a set of real `.ktt` traces, the four investigation scenarios S1 to S4 can be completed in Zed with the same outcome as in VS Code, and the acceptance vectors in spec section 6 pass.

## 11. Decisions, risks and open questions

### 11.1 Decisions proposed (please confirm)

| # | Decision | Alternative |
| --- | --- | --- |
| D-1 | Row Details is a right-dock panel. | A pane inside the results surface. |
| D-2 | One results component serves the bottom panel and editor tabs. | Two implementations. |
| D-3 | `.ktt` stays the interchange format and is round-trip compatible with the VS Code fork. | A new Zed-only format. |
| D-4 | Structured view is available for any result with the two activity columns, live or saved. | Saved files only (VS Code today). |
| D-5 | Fix the four VS Code deviations in spec section 7 rows 1 to 4 rather than reproducing them. | Exact parity, bugs included. |
| D-6 | Build phases A to D from `.ktt` files first, before the execution service exists. | Wait for execution. |

### 11.2 Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| The existing table component cannot deliver rectangular selection, header gestures and 500k-row responsiveness. | Phase A slips; grid becomes a larger new component. | Do the section 3.1 spikes first; decide reuse versus build on evidence. |
| Rich text in the inspector (selection, wrap toggle, find highlight over coloured runs) is harder in a native text layer than in HTML. | RDT-5, RDT-6, JSN-3 slip. | Spike early; fall back to plain colour segments with find implemented over the raw text. |
| Multi-format clipboard (HTML plus text) differs per platform. | CPY-1, CPY-5 partly delivered. | Decide platform support during phase A; degrade to text-only where needed. |
| Drag-and-drop of a selection into an editor as a `datatable` expression may not be feasible. | CPY-9 dropped. | Already P2; `Copy as datatable` covers the workflow. |
| Behaviours only verified by reading code (multi-word search, copy order) differ in a running VS Code. | Parity tests fail for a trivial reason. | Confirm the two open questions in the spec (Q-3, Q-5) before writing tests. |

### 11.3 Open questions

See spec section 8 (Q-1 to Q-10). The two that most affect the design: the inspector's home (Q-1, D-1) and non-contiguous row selection for multipart messages (Q-4).

## 12. Traceability: VS Code to Zed

| VS Code feature | Spec IDs | VS Code source | Phase |
| --- | --- | --- | --- |
| Results grid with gutter, paging, columns | GRD | `dataTableProvider.ts` | A |
| Cell, row, column, table selection | SEL | `dataTableProvider.ts` | A |
| Column resize, reorder, saved layout | COL, PER-3 | `dataTableProvider.ts`, `resultsViewer.ts` | A |
| Copy TSV / Markdown / HTML / datatable | CPY | `dataTableProvider.ts`, `tsv.ts`, `markdown.ts`, `html.ts` | A |
| `.ktt` open, save, auto-save layout | PER | `resultsViewer.ts` | A |
| Three-state sort, typed order | SRT | `dataTableProvider.ts` | B |
| Column filters | FLT | `workbenchGrid/columnFilters.ts` | B |
| Global search | SRC | `dataTableProvider.ts` | B |
| Severity row highlighting | SEV | `workbenchGrid/severityHighlighting.ts` | B |
| Large-result loading overlay | GRD-9 | `workbenchGrid/loadingOverlay.ts` | B |
| Row Details, find, wrap | RDT | `rowDetailsView.ts` | C |
| JSON formatting and colouring | JSN | `rowDetailsView.ts` | C |
| Multipart assembly | MPM | `rowDetailsView.ts` | C |
| Call-stack formatting | EXC | `rowDetailsView.ts` | C |
| Structured activity tree, severity, Deepest, splitter | ACT | `activityTree.ts`, `dataTableProvider.ts` | D |
| Results panel, badge, tabs, states | PNL | `resultsViewer.ts` | E |
| Run, cancel, error range, result ownership | RUN | `queryEditor.ts`, `queryCancellation.ts` | E |
| History-backed results | PER-5 | `historyManager.ts` | E |
| Query parameter profiles | QPP | `queryParameterProfiles.ts` | F |
| Agent access to results | AGT | `savedQueryResults.ts` | F |
