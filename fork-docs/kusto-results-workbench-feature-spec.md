# Kusto Results Workbench for Zed: feature specification

Status: draft for review. Companion to [kusto-results-workbench-design.md](kusto-results-workbench-design.md), which holds the UI designs, the conceptual model and the delivery plan. This document is the functional contract: what the user can do and what must be true when they do it. It deliberately says nothing about crates, types or code structure.

## 1. Purpose and scope

The KustoTraceTools VS Code extension (fork `dev/gregm` of `Kusto-Explorer-VsCode`) turns Kusto query results into a working surface for investigating large, structured diagnostic traces. This spec captures the parts of that extension we want in the Zed fork.

| Area | In scope for this spec | Notes |
| --- | --- | --- |
| Run lifecycle as it feeds results | Yes, minimal | Running, cancelling, error and empty states, result ownership. Execution itself is covered by the existing [zed-kusto-design.md](zed-kusto-design.md). |
| Results panel and result tabs | Yes | Bottom results surface, table tabs, result summary, badge. |
| Results grid | Yes | Typed grid, selection, search, column layout, copy. |
| Sorting | Yes | Typed, three-state, restores result order. |
| Column filtering | Yes | Type-aware, two conditions per column, combined with search. |
| Severity highlighting | Yes | Row colour by `level` / `severity`. |
| Row Details inspector | Yes | Find, wrap, JSON, multipart assembly, exception call-stack formatting. |
| Structured activity view | Yes | Activity tree plus event grid, severity colours, deepest-path action. |
| Result persistence | Yes | `.ktt` compatibility, per-table view state. |
| History list (browse, delete, clear, reveal, copy to workspace) | Out, except the stored data (PER-5) | The list UI can follow later (P2). |
| Query parameter profiles | Deferred (section 5) | Requirements recorded so they are not lost. |
| Agent access to results | Deferred (section 5) | Requirements recorded so they are not lost. |
| Sequence diagram view | Deferred (section 5.3) | A Zed addition. Requirements come from a prototype on two real traces. |
| Trace schema (configurable column roles) | Deferred (section 5.4) | Needed by the sequence view and wanted by the activity view. |
| Timeline model (durations, untraced time, critical path, repetition, failures) | Deferred (section 5.5) | The shared computation behind the findings strip, the waterfall and the sequence view. |
| Findings strip | Deferred (section 5.6) | A short list of facts about a trace, each linked to its rows. |
| Waterfall (Timeline tab) | Deferred (section 5.7) | Where the time went, drawn natively. |
| Message templates and structural rows | Deferred (section 5.8) | Grouping repeated messages and setting aside rows that carry no content. |
| Focus on an activity | Deferred (section 5.9) | Rebuild every view from one activity and what is below it. |
| Charts, `render`, graph and pivot views | Out | To be specified later. |
| Connections explorer, scratch pads, formatting settings, Copilot schema tools | Out | Not part of this effort. |
| Query editing (highlighting, completion, diagnostics) | Out | Covered by the Kusto extension spike in `extensions/kusto`. |

### 1.1 Source of truth

Every behaviour below labelled "VS Code today" was read from the fork's source, not from its README. The main files (all under `Kusto-Explorer-VsCode/`):

- `src/Client/features/dataTableProvider.ts` (grid, selection, sort, resize, reorder, copy, structured layout)
- `src/Client/features/workbenchGrid/columnFilters.ts`, `severityHighlighting.ts`, `loadingOverlay.ts`
- `src/Client/features/rowDetailsView.ts` (inspector, JSON, multipart, call stacks)
- `src/Client/features/activityTree.ts` (activity projection and severity outcome)
- `src/Client/features/resultsViewer.ts` (panel, tabs, badge, `.ktt` persistence)
- `src/Client/features/queryEditor.ts`, `queryCancellation.ts` (run and cancel)
- `docs/FORK_DESIGN.md`, `docs/ACTIVITY_TREE_SEVERITY.md`, `docs/RESULTS_BADGE.md`, `docs/JSON_RESULT_HIGHLIGHTING.md`, `docs/QUERY_CANCELLATION.md`
- Unit tests under `src/Client/tests/unit/`. They pin filters, call-stack formatting, JSON display, severity highlighting and the activity projection. There are **no tests** for multipart assembly or for sort comparators, so those rules below are defined by this spec (from reading the code), not by tests.

### 1.2 Conventions

- **Requirement IDs** are stable (`GRD-3`, `SRT-2`). Cite them in commits, tests and review comments.
- **Priority.** P0 is required for a first usable release and reproduces the VS Code trace workflow. P1 is VS Code behaviour that can follow the first release, or a small addition needed to make a P0 usable. P2 is an improvement over VS Code or a decision we have not made yet.
- **(Zed addition)** marks a requirement that VS Code does not have. Anything not so marked is derived from the VS Code source.
- **"VS Code today"** states current behaviour. **"Zed"** states the requirement. When the two differ the difference is deliberate and is repeated in section 7.
- "Must" is a requirement, "should" is a strong preference, "may" is optional.
- Section 7 lists every place where the Zed requirement deliberately differs from what VS Code does today.
- Wireframes are referenced as `W-n` and live in the design document, section 6.

### 1.3 Glossary

| Term | Meaning |
| --- | --- |
| Result set | Everything one query run returned: one or more tables plus run metadata (query text, cluster, database, start time, duration, client request id, parameters). |
| Table | One named table of a result set: ordered typed columns and rows. |
| Source row index | The zero-based position of a row in its table as returned by the run. Never changes. Displayed to users as a one-based row number. |
| View | The user-visible arrangement of a table after search, filters, sort. The source data is never modified by a view. |
| Selection | The cells, rows or columns the user has picked in the grid. |
| Anchor cell | The cell where a selection started; shift+click and shift+arrow extend the selection from it. |
| Visible position | A row or column's place in the grid as currently displayed (after search, filters, sort and column reordering), as opposed to its source row index or original column index. |
| Display order | The order rows are shown in: the result of search, filters and sort. |
| Data column | A column of the result table; excludes the row-number gutter. |
| Data tab | A results tab showing a table as a grid (as opposed to the Query tab or a structured tab). |
| Inspector | The Row Details surface, bound to the current row selection. |
| Activity | A group of trace rows sharing one `CurrentActivityId`. |
| Severity | An integer 1 to 5 in a column named `level` or `severity`: 1 critical, 2 error, 3 warning, 4 normal, 5 verbose. |
| Multipart message | A logical message split across several rows, each value prefixed `k/N:`. |

## 2. Users and scenarios

The user is an engineer investigating an incident from telemetry held in Azure Data Explorer. Volumes are large (tens of thousands of trace rows is normal; 100k to 500k is the stress target in the existing design doc), values are wide (JSON, exception objects, call stacks), and the same result is read many times while the user narrows down.

- **S1. Find the failing step.** Run a query for one root activity. Scan severity-coloured rows, sort by time, filter `level <= 3`, select a warning row and read the full message in the inspector.
- **S2. Read an exception.** Select a row whose message is an exception JSON with a long call stack. The inspector shows formatted JSON and a call stack reduced to application frames.
- **S3. Reassemble a split message.** A large payload was logged as `1/3:`, `2/3:`, `3/3:` rows. Select all three rows and read the merged, pretty-printed payload.
- **S4. Follow the call tree.** Open the structured view of a trace, see which sub-activity failed from the colour and warning marker on its tree node, jump to the deepest activity, and read that activity's events.
- **S5. Keep the evidence.** Save the result to a file, close the app, reopen it later and see the same table with the same column layout.

## 3. Feature map

```
Run query ──► Results panel ──► Data tab ─────────────► Row Details inspector
                  │               (grid: search,              (find, wrap, JSON,
                  │                filters, sort,              multipart, exception
                  │                selection)                  call stacks)
                  │
                  └────────────► Structured tab ──► activity tree + event grid
                                  (only when the table has activity columns)
```

## 4. Functional requirements

### 4.1 Run lifecycle and result ownership (RUN)

The results surfaces only work if a run has a clear lifecycle. This section states the minimum the results side needs; the execution protocol itself belongs to the execution design.

| ID | Pri | Requirement |
| --- | --- | --- |
| RUN-1 | P0 | Running the query at the cursor or the current selection produces a result set that is shown in the results panel. The panel opens if closed, without taking editor focus. |
| RUN-2 | P0 | While a run is executing, the query it was started from shows a running indicator and a **Cancel** action, and a progress notification also offers Cancel. Cancel does not exist while the query is idle. It disappears when execution finishes, before results are published. Each notification cancels its own run; the inline Cancel targets the newest run of that query. (VS Code: inline action, editor toolbar, notification; the running indicator stays visible for at least 500 ms so it never flickers.) |
| RUN-3 | P0 | Cancelling one run must not affect other runs. A cancelled run must not replace displayed results, must not create a saved result, and must not show an error. Running indicators clear and the next run works normally. |
| RUN-4 | P0 | A failed run shows the error message and its details. In the panel the error replaces the previous result. In an editor result tab the error opens its own error tab and leaves other result tabs untouched. When the server reports an error range, the editor marks it (VS Code places an error glyph before the range start; a highlight or underline is acceptable in Zed). |
| RUN-5 | P0 | Every result set is owned by the run that produced it. A late response from an older run must never replace the display of a newer run or a result tab belonging to another run. |
| RUN-6 | P0 | Result metadata is retained with the result set: query text, cluster, database, start time, duration, client request id, and the parameter values used. |
| RUN-7 | P1 | When results are shown in the panel, a new run replaces the panel's contents. When results are shown as tabs, each completed run opens its own tab (VS Code default `newTab`), so concurrent runs are safe. Both modes are selectable (SET-2). |
| RUN-8 | P1 | Rerun the query stored in a **saved result document** using its stored parameters. Rerun is cancellable. A successful rerun replaces the document's contents; a cancelled rerun leaves it unchanged. History entries do not offer rerun. |
| RUN-9 | P1 | **Per-query editor actions.** Each query in a Kusto editor offers inline actions: Select (selects the query text), Run (or Running, with Cancel, per RUN-2), Copy (colourised query), Format, and, when a stored result exists for that query, **Results** (opens the most recent stored result whose comment- and whitespace-insensitive query text matches, without running), **Last run** (start time and duration, for example `Last run: Sep 30, 10:42:11 AM, took: 1.8s`) and **Copy CID** (copies the run's client request id, which identifies the run to support and to the agent tool in AGT-4). With no matching stored result, Results reports `No saved results were found for this query.` |

Late responses: VS Code ignores a response that arrives after cancel and after a newer render (see `docs/QUERY_CANCELLATION.md` and `docs/RESULTS_BADGE.md`). The same guarantee applies to any state carried in the status area, including the row-count badge.

### 4.2 Results panel and tabs (PNL)

Wireframes: W-1, W-2, W-11.

| ID | Pri | Requirement |
| --- | --- | --- |
| PNL-1 | P0 | A **Results** panel in the bottom dock hosts the current run's output. Its title carries a **row-count badge**: the sum of rows across all tables of the displayed result set. |
| PNL-2 | P0 | Badge rules (from `docs/RESULTS_BADGE.md`): a populated result shows its total; an empty result or a result with no tables shows `0 rows`, replacing any earlier count; an error shows an error marker (VS Code: a numeric badge `1` with the tooltip `Error`; an empty result publishes an explicit zero badge with the tooltip `0 rows`, which VS Code hides); a later populated result shows its own count; a delayed or retried render can never restore a badge from an older result. |
| PNL-3 | P0 | Empty state: with no result yet, the panel says so (`no results` in VS Code today). A run that returns zero rows shows the grid header and the `No results` message (GRD-6), not the empty state. |
| PNL-4 | P0 | A result set with one table shows one **Data** tab (or no tab bar when it is the only view). A result set with several tables shows one tab per table, labelled `<table name> (<row count>)`. |
| PNL-5 | P0 | On every surface that offers the structured view (ACT-16), when a table has both `CurrentActivityId` and `ParentActivityId` columns, an extra tab is added next to its data tab, labelled `Data - Structured` (single table) or `<table name> - Structured (<row count>)`. Requirements in section 4.12. |
| PNL-6 | P1 | A **Query** tab in editor result tabs (PNL-10) shows the cluster, the database and the query text that produced the result. Shown only when the result has query text. (VS Code shows no start time or duration here, and its bottom panel has no Query tab.) |
| PNL-7 | P1 | (Zed addition) A one-line result summary sits above or below the grid: cluster / database, row count of the active table, duration, and start time. |
| PNL-8 | P0 | A panel toolbar offers: Copy (current selection or table), Search toggle, Save As. Copy and Search apply only when a data tab is active. |
| PNL-9 | P1 | The active tab is remembered while the same result set stays displayed. A new result set activates its first data tab. |
| PNL-10 | P1 | Results can also be shown in an editor tab beside or in place of the panel (SET-1). The tab shows the same data and structured tabs as the panel, plus the Query tab (PNL-6). |
| PNL-11 | P1 | The panel may be hidden while no Kusto document is open and no standalone result view exists (VS Code's visibility rule; it does not depend on which editor is active). |

### 4.3 Grid display (GRD)

Wireframes: W-2, W-3, W-9.

The grid is a spreadsheet-like, read-only surface. It must remain smooth at the stress workloads in the existing design doc (100k to 500k rows, 15 to 30 columns).

| ID | Pri | Requirement |
| --- | --- | --- |
| GRD-1 | P0 | Columns show the name from the result set. (Zed addition) A column's Kusto type is available to the user, as a header tooltip at minimum; VS Code's grid header shows only the name. |
| GRD-2 | P0 | A leading **row-number gutter** shows the one-based source row number for every row, and never changes with sort or filter. The gutter cell of the header is the **corner cell**. |
| GRD-3 | P0 | Each cell shows the value as text on a single line, truncated with an ellipsis when wider than its column. The full value is always reachable (Row Details, tooltip, copy). |
| GRD-4 | P0 | Value formatting: null renders as an empty cell; `true` and `false` render as words; numbers as given by the server; datetime as the ISO 8601 text the server sent; timespan in `[-][d.]hh:mm:ss[.fffffff]`; guid as text; dynamic values (objects and arrays) as compact single-line JSON. Text must never be interpreted as markup. |
| GRD-5 | Dropped | (Zed) No paging. The grid virtualises its rows, so it shows every row of the view and scrolls; cost does not depend on row count (architecture part 2, S1). See section 7, row 23. The page-size setting (SET-3) goes with it. |
| GRD-6 | P0 | A footer states how many rows the view holds: `{rows} rows`, or `{shown} of {rows} rows` when search or filters hide some, then the selection (`Row N selected`, `K rows selected`). (VS Code: `Showing {start} to {end} of {rows} rows`.) Two empty cases: a table with **no rows** shows the message `No results` (VS Code leaves the footer blank), and a search or filter that matches **nothing** shows `No results match your search query` (VS Code takes this text from its grid library). |
| GRD-7 | P0 | Search, filters and sort operate on the complete table already retrieved, They never re-run the query or alter the KQL. |
| GRD-8 | P0 | The header stays visible while rows scroll vertically. |
| GRD-9 | P0 | Loading feedback. VS Code shows a blocking overlay `Rendering N rows…` (structured grids: `Rendering N events…`) with a spinner, only while a grid of 1000 rows or more is first created; it fades out when the grid exists. Zed requirement: opening a table, sorting, filtering and searching that are still running 250 ms after the user's action must show a busy indicator carrying the row count being processed, and must expose a busy state to assistive technology. While the work runs the window must keep handling input (scroll, resize, switching tab) with no stall over 100 ms. A blocking overlay is allowed, a non-blocking indicator is preferred (W-9). Test: a 100k-row fixture sorted on a string column. |
| GRD-10 | P0 | Column headers show a sort indicator (SRT-4) and a filter funnel (FLT-1). |
| GRD-11 | P1 | Very wide values in a cell never change row height. |

### 4.4 Selection (SEL)

Wireframe: W-3.

VS Code implements an Excel-style rectangular selection. The rectangle is always contiguous in the current display order.

| ID | Pri | Requirement |
| --- | --- | --- |
| SEL-1 | P0 | Click a cell selects it. Clicking the only selected cell again clears the selection. |
| SEL-2 | P0 | Shift+click extends a rectangle from the anchor cell to the clicked cell. Click and drag selects a rectangle; dragging near an edge auto-scrolls. |
| SEL-3 | P0 | Clicking a gutter cell selects the whole row (all data columns). Shift+click on another gutter cell extends the range of rows. Click and drag on the gutter selects a range of rows. Clicking the only selected row's gutter clears it. |
| SEL-4 | P0 | Shift+click on a column header selects the whole column, shift+click again on another header extends the column range, and shift+drag across headers selects a range of columns. Shift+click on the only selected column clears it. |
| SEL-5 | P0 | Shift+click on the corner cell selects the whole table, or clears it when everything is already selected. |
| SEL-5a | P0 | **Scope of whole-column and whole-table selection.** In Zed, a whole-column or whole-table selection covers every row of the current view (after search and filters), across all pages. VS Code covers only the rows on the **current page**; see section 7. |
| SEL-6 | P0 | Selection is cleared when the user sorts, reorders columns, changes pages, changes search or filters, or (in a structured view) selects a different activity, because the visible positions the rectangle referred to no longer exist. VS Code clears the selection only on sort, column reorder and activity change; after a page, search or filter change it keeps a stale highlight and leaves the inspector showing the old rows (section 7). |
| SEL-7 | P0 | Every change in the set of selected rows is published to the inspector (RDT-1) as source row indexes, deduplicated, with the table identity. Clearing the selection publishes an empty selection. |
| SEL-8 | P0 | Selection uses a visible, theme-aware highlight that outranks severity colours (SEV-3). Selected header cells and gutter cells show which columns and rows are fully selected. |
| SEL-9 | P1 | Keyboard: arrow keys move the anchor cell, Shift+arrow extends, Home / End and PageUp / PageDown move within the row and page, Ctrl/Cmd+A selects all, Ctrl/Cmd+C copies. VS Code today provides only Ctrl/Cmd+C from the keyboard; the rest is a Zed requirement for a native surface. |
| SEL-10 | P2 | (Zed addition) Preserve the selection by row identity across sort and filter instead of clearing it. |
| SEL-11 | P1 | (Zed addition) Ctrl/Cmd+click on a gutter cell adds or removes that row from a **non-contiguous set of selected rows**, so the parts of a multipart message can be picked from a mixed view (Q-4). |

### 4.5 Search (SRC)

| ID | Pri | Requirement |
| --- | --- | --- |
| SRC-1 | P0 | A **Search** toggle in the panel toolbar shows or hides a search box above the grid (hidden by default). Showing it focuses the box. |
| SRC-2 | P0 | Search is case-insensitive **literal** substring matching over the text of every data column. Punctuation and diacritics in the term are significant (`10:42`, `1/3` and `café` match those exact characters). The gutter is not searched. VS Code lower-cases the term, strips diacritics and strips punctuation such as `. , / # ! $ % ^ & * ; : { } = - _ ~ ( )` from the term but not from the cell text, so terms containing those characters never match (section 7). |
| SRC-3 | P0 | Search combines with column filters: a row is shown only when it matches the search **and** every active column filter. |
| SRC-4 | P0 | Search is applied as the user types, with the same responsiveness rules as GRD-9. |
| SRC-5 | P0 | Multi-word search: every whitespace-separated term must appear in the row, each in any cell (AND across terms, terms may match different cells). VS Code matches rows containing **any** term (OR), read from its grid library's source (confirm in a running VS Code before writing parity tests); see section 7. |
| SRC-6 | P1 | The search box shows the placeholder `Search...`. (Zed addition) Escape clears it. |
| SRC-7 | P1 | Hiding the search box must either clear the query or leave a visible indicator that a search is active. VS Code hides the box and keeps the query filtering the rows invisibly. |

### 4.6 Sorting (SRT)

Wireframe: W-3.

| ID | Pri | Requirement |
| --- | --- | --- |
| SRT-1 | P0 | Sorting is **three-state per column**: clicking a header sorts ascending, clicking again sorts descending, clicking a third time returns to the original result order. Sorting a different column starts that column at ascending and replaces the previous sort. |
| SRT-2 | P0 | Clicking the **corner cell** sorts by row number, and completes the same three states. The third state, original result order, is the default and is what "Restore result order" means. |
| SRT-3 | P0 | Sorting reorders the view only. Source values, source row indexes and gutter numbers are untouched. Rows that compare equal are ordered by source row index, whichever direction is sorted. (VS Code sorts in place, so in descending order ties keep the previous view order instead.) |
| SRT-4 | P0 | The sorted column's header shows an ascending or descending indicator. With no sort active no header shows an indicator, so the user can always tell whether the display is in original order. |
| SRT-5 | P0 | Comparison is by type, never by formatted text. The rules SRT-5a to SRT-5f give the order for each type. |
| SRT-5a | P0 | **int, long, real, decimal**: numeric. |
| SRT-5b | P0 | **datetime**: chronological on the parsed instant, at the resolution the server sends (100 ns ticks). VS Code parses to milliseconds, so values differing only in the last digits compare equal. |
| SRT-5c | P0 | **timespan**: by duration, including negative and day-prefixed values. (VS Code sorts timespans as text; section 7.) |
| SRT-5d | P0 | **bool**: `false` before `true`. |
| SRT-5e | P0 | **string, guid**: Unicode text, case-insensitive, with a case-sensitive tiebreak. VS Code additionally applies natural (numeric-aware) ordering, so `Step2` sorts before `Step10`, and ignores punctuation; Decided (Q-12): natural (numeric-aware) ordering, case-insensitive, punctuation not ignored. |
| SRT-5f | P0 | **dynamic**: by its compact JSON text, as a string. |
| SRT-6 | P0 | Nulls sort as the smallest value: first when ascending, last when descending. This matches Kusto's own `order by` default. Values that cannot be parsed for their column type sort after all parseable values, in both directions, and keep source order among themselves. |
| SRT-7 | P0 | Sorting does not change search or filters; it applies to the searched, filtered rows. |
| SRT-8 | P0 | Sorting a 500k-row table must not freeze the window (GRD-9). |
| SRT-9 | P2 | Multi-column sort by Shift+click adding secondary keys. Not in VS Code. |

### 4.7 Column filtering (FLT)

Wireframe: W-4.

| ID | Pri | Requirement |
| --- | --- | --- |
| FLT-1 | P0 | Every data column header carries a **funnel** action. The funnel is highlighted while that column has an active filter and reports its pressed state to assistive technology. |
| FLT-2 | P0 | Clicking the funnel opens a filter popover anchored to the header, titled `Filter <column>`. It closes on Escape, on a click outside it, or when another popover opens. It stays inside the visible area. |
| FLT-3 | P0 | A column has up to **two conditions**. When there are two, a selector chooses `Match all conditions` (AND) or `Match any condition` (OR). Buttons: `Add condition` (when fewer than two), `Remove condition` (when two), `Clear`. `Clear` removes the column's filter and closes the popover. |
| FLT-4 | P0 | The operators offered depend on the column type. See FLT-4a to FLT-4d. |
| FLT-4a | P0 | **string, guid, dynamic, and any other type**: Contains, Does not contain, Equals, Does not equal, Starts with, Is empty, Is not empty. |
| FLT-4b | P0 | **int, long, real, decimal, timespan**: Equals, Does not equal, Greater than, Greater than or equal, Less than, Less than or equal, Is empty, Is not empty. |
| FLT-4c | P0 | **datetime**: On, Not on, After, On or after, Before, On or before, Is empty, Is not empty. |
| FLT-4d | P0 | **bool**: Is true, Is false, Is empty, Is not empty. |
| FLT-5 | P0 | Operators that need a value show a text input; others hide it. Placeholders: `ISO date/time` for datetime, `d.hh:mm:ss` for timespan; when a column has two conditions the first value's placeholder is `First value`. |
| FLT-6 | P0 | Matching rules: string operators are case-insensitive. Numeric operators compare parsed numbers. Timespan operators compare parsed durations (`[-][d.]hh:mm:ss[.fff]`). Datetime operators compare parsed instants at 100 ns resolution; a date or date/time typed without a zone is read as UTC, because Kusto datetimes are UTC. (VS Code reads a date-only value as UTC but a date/time without a zone in the machine's local zone, and compares only to the millisecond.) `Is empty` is null or empty text. `Is true` and `Is false` accept `true`/`1` and `false`/`0`. A cell or a typed value that cannot be parsed for a numeric, timespan or datetime comparison does not match. A null cell matches only `Is empty` and the negative operators (`Does not equal`, `Does not contain`, `Not on`); it never satisfies a greater-than or less-than comparison. (VS Code treats a null numeric cell as 0, and does not match a null datetime for `Not on`.) |
| FLT-7 | P0 | Dynamic and other structured values are filtered against their compact JSON text. |
| FLT-8 | P0 | A condition takes effect once it is usable: it needs a value if its operator requires one. A column whose conditions are all unusable has no filter. Changes apply live, debounced by roughly 150 ms. |
| FLT-9 | P0 | The grid toolbar shows a single **Clear all filters** action, visible only while at least one filter is active. |
| FLT-10 | P0 | Filters are evaluated over the whole loaded table (GRD-7) and combine with search (SRC-3) and with sort (SRT-7). |
| FLT-11 | P0 | Filter state is transient: it lasts for the open grid view and is not saved with a result. (VS Code decision; see Q-6 for whether to persist.) |
| FLT-12 | P0 | In a structured view, filters apply within the events of the currently selected activity (ACT-6). |
| FLT-13 | P2 | (Zed addition) A typed-value picker (calendar, distinct-value checklist). |

### 4.8 Column layout (COL)

| ID | Pri | Requirement |
| --- | --- | --- |
| COL-1 | P0 | Initial column widths are derived from the header label and cell content, so the label is never truncated at first display; VS Code caps a content-derived width at 500 px. The gutter is at least 40 px wide and resizable. |
| COL-2 | P0 | Dragging the right edge of a header resizes that column. The pointer changes near the edge. A resize must not trigger a sort. |
| COL-3 | P0 | Dragging a header (not on its resize edge and not a selected header) **reorders** columns. A vertical line shows the drop position; the column moves before or after the target depending on which half the pointer is over. Reordering clears the selection (SEL-6). |
| COL-4 | P0 | Column order and widths, including gutter width, are saved with the result (PER-3) and restored on open. Ordinary and structured grids of the same table save independently. |
| COL-5 | P1 | (Zed addition) Reset layout: an action returns a table to original column order and automatic widths. |
| COL-6 | P2 | (Zed addition) Hide or pin columns. |

### 4.9 Copy and drag (CPY)

All copy actions take the **current selection**, or the **whole table** when there is no selection. With no selection VS Code copies **all source rows in source order, ignoring search, filters and sort**; Zed does the same (CPY-10 discusses the alternative). A selection is copied in display order (VS Code reads it from the visible rows; some of its comments say otherwise).

| ID | Pri | Requirement |
| --- | --- | --- |
| CPY-1 | P0 | **Copy** (Ctrl/Cmd+C, toolbar, context menu). A single selected cell copies its raw value as plain text: no header, quoting or separators. Anything larger puts tab-separated text with a header row on the clipboard. A rich HTML table alongside it is P1 (CPY-11). |
| CPY-2 | P0 | TSV escaping follows Excel: a value containing a tab, newline, carriage return or double quote is wrapped in double quotes with inner quotes doubled. |
| CPY-3 | P0 | Value text for copy: null is empty, objects and arrays are compact JSON, everything else its plain text. |
| CPY-4 | P0 | **Copy as Markdown**: a pipe table with a header and separator row, plain text only, with pipes and line breaks in values escaped. |
| CPY-5 | P0 | **Copy as HTML**: the table markup as plain text. Also offering it as rich HTML is CPY-11. |
| CPY-6 | P0 | **Copy as datatable**: a KQL `datatable(...)` expression that reproduces the selected cells with their column types, generated so it parses and round-trips. VS Code obtains the expression from its language server; Zed needs an equivalent generator. Vectors (null, dynamic, timespan, guid, datetime, escaped strings, column names needing quoting, empty table) are to be captured from VS Code's output for a fixture table and added to section 6. | **Built** as `kusto_results::export::datatable`, ported from the extension's `KustoGenerator` and checked against Kusto.Language 12.4.0 (its literal quoting, keyword list and bracketing, and its output for a table of every type, are the test vectors in `export.rs`); every expression made from the sample files parses with no syntax diagnostics. Differences: section 7, row 24.
| CPY-7 | P0 | A context menu on the grid offers Copy, Copy as Markdown, Copy as HTML and Copy as datatable (W-10). |
| CPY-8 | P1 | Ctrl/Cmd+C copies only when the grid is the active surface and the user is not editing a text input (for example the search box). |
| CPY-9 | P2 | Drag a selection into a Kusto editor to insert it as a `datatable` expression; dragging from the corner carries the whole table, dragging from a fully selected column header carries the column selection. VS Code supports this; a native drag source into an editor may not be practical at first. |
| CPY-10 | P2 | (Zed addition) Offer "copy the filtered and sorted view" as an alternative to the whole-source-table default when nothing is selected. |
| CPY-11 | P1 | Rich HTML on the clipboard next to the text: for Copy (TSV plus an HTML table) and Copy as HTML. VS Code does this only on Windows; elsewhere it copies plain text. Zed's clipboard currently has no HTML entry type, so this needs platform work (design doc section 3.1). |

### 4.10 Severity highlighting (SEV)

Wireframe: W-3.

| ID | Pri | Requirement |
| --- | --- | --- |
| SEV-1 | P0 | If a table has a column named `level` or `severity` (matched case-insensitively, after trimming), each row whose value in that column is the integer 1 to 5 is tinted across all its data cells: 1 critical, 2 error, 3 warning, 4 normal, 5 verbose. Values that are not integers 1 to 5 leave the row untinted. |
| SEV-2 | P0 | The five colours are user-configurable (SET-4). An empty colour leaves that level in the theme's default row styling. Defaults are translucent tints (VS Code: critical `#f14c4c40`, error `#f4877133`, warning `#cca7002e`, normal `#89d1851f`, verbose `#75beff17`; the last two hex digits are opacity). |
| SEV-3 | P0 | A row's severity tint follows the row through sort and filter. Selection highlight and hover take precedence over the tint. |
| SEV-4 | P0 | The gutter cell is not tinted. |
| SEV-5 | P1 | The same palette drives the structured view's tree colours (ACT-9), so one setting changes both. |

### 4.11 Row Details inspector (RDT)

Wireframes: W-5, W-6, W-7.

The inspector is a panel bound to the current grid selection. It shows the full, readable content of the selected row without duplicating the cramped grid layout.

| ID | Pri | Requirement |
| --- | --- | --- |
| RDT-1 | P0 | The inspector follows the selection of whichever results grid was last interacted with: data grids and structured grids, in the panel or in an editor tab. It does not re-render when the same selection is published again. |
| RDT-2 | P0 | Empty state text: `Select a result row to inspect its values here.` Shown when no rows are selected. |
| RDT-3 | P0 | With one row selected, a header shows the table name and `Row N` (one-based **source** row number) with the field count, then one **field block** per column, in the table's column order, each showing the column name, its type, and the full value. |
| RDT-4 | P0 | Null values show the word `null`, dimmed and italic. Text values keep their line breaks and can be selected and copied. |
| RDT-5 | P0 | **Word wrap** toggle: a header button labelled `Wrap lines: On` or `Wrap lines: Off`. On by default. Off makes long lines scroll horizontally instead of wrapping, including inside JSON blocks. The setting persists while the inspector stays open. |
| RDT-6 | P0 | **Find in row**: a search box in the header with placeholder `Find in row`. Typing highlights every case-insensitive match in the field values, and scrolls the first match into view. Ctrl/Cmd+F focuses and selects the box while the inspector is focused. Escape, while the box is focused, clears it and removes the highlights. Matches are found across formatting boundaries (for example across separately coloured JSON tokens) and highlighting never changes the displayed text. Find covers values; it does not need to match column names. |
| RDT-7 | P0 | With several rows selected that do **not** form one complete multipart message, the inspector shows the first selected row (first in display order) and tells the user how many rows are selected, for example `2 rows selected - showing the first`. VS Code shows only the first row and no note. Showing which rows are selected and moving between them is P2 (RDT-9). |
| RDT-8 | P0 | Values are shown as the source data, with **display-only** transformations (JSON pretty-printing, call stack trimming, multipart assembly). None of them modify the underlying result. Copy from the inspector copies what is displayed. |
| RDT-9 | P2 | Previous/next navigation among multiple selected rows. Not in VS Code. |
| RDT-11 | P0 | (Zed addition) When the result that owns the current subject is replaced by a new run or closed, the inspector returns to its empty state. VS Code leaves the old rows displayed. |
| RDT-12 | P1 | (Zed addition) The find text survives selection changes and the wrap toggle. VS Code rebuilds the view on each, which drops the find text and the scroll position. |
| RDT-10 | P1 | The inspector can be placed in a dock the user chooses. VS Code today defaults to the Explorer sidebar and lets users move it to the secondary sidebar. |

#### 4.11.1 JSON presentation (JSN)

| ID | Pri | Requirement |
| --- | --- | --- |
| JSN-1 | P0 | A value is treated as **JSON** when it is a dynamic object or array, or a string which, after trimming, starts with `{` or `[` and parses as JSON. Scalar JSON text such as `123`, `true` or `"text"` is an ordinary value and is not reformatted. Strings that start with `{` or `[` but do not parse are shown as plain text. |
| JSN-2 | P0 | JSON is pretty-printed with two-space indentation inside a code-style block distinct from plain values, and the block wraps or scrolls per RDT-5. |
| JSN-3 | P0 | **Syntax colouring** by token kind: property names, strings, numbers, booleans, `null`. Colours come from the theme. Colouring must not change the text, whitespace or characters displayed, and characters that are significant in markup are shown literally. |
| JSN-4 | P0 | Escaped newlines (`\n`) inside JSON string values are shown as real line breaks, so call stacks and multi-line messages are readable; an escaped carriage return (`\r`) is left as it is (VS Code parity). This is display-only: nothing is written back. Edge case for review: a genuine backslash followed by `n` is indistinguishable after this step (Q-7). |
| JSN-5 | P0 | The call stack transformation (EXC-1 to EXC-9) is applied, at any nesting depth, to every string property named `callstack` (matched case-insensitively) inside a JSON value. |

#### 4.11.2 Multipart messages (MPM)

Some diagnostic events are emitted in several rows, each value beginning with a marker such as `1/3:`.

| ID | Pri | Requirement |
| --- | --- | --- |
| MPM-1 | P0 | When the user selects rows that together form **one complete multipart message** in some column, the inspector assembles them: it orders the parts by part number, removes each row's marker prefix, and joins the remaining text with no separator. |
| MPM-2 | P0 | The marker is `k/N:` at the start of the value: `k` and `N` are integers, optional whitespace is allowed around the `/` and after the colon, one space after the colon belongs to the marker and is removed. `N` must be at least 2 and `k` between 1 and `N`. |
| MPM-3 | P0 | Assembly is deliberately conservative. It happens for a column only when **all** are true: two or more rows are selected; every selected row has a marker in that column; all markers declare the same `N`; the number of selected rows equals `N`; the part numbers are exactly 1 to N with no duplicates or gaps. Otherwise nothing is assembled and RDT-7 applies. Unrelated rows, incomplete sequences and ordinary messages must stay separate. |
| MPM-4 | P0 | The assembled output replaces the per-row field view. Its header reads `N selected rows · multi-part message assembled`. Each assembled column is one field block labelled with the column name and the tag `merged N-part message`. If several columns qualify, each gets its own block. |
| MPM-5 | P0 | The assembled text goes through the JSON rules in 4.11.1. Payloads that are JSON (the common case) render as highlighted, pretty-printed JSON with call stacks trimmed. |
| MPM-6 | P0 | Assembly works in whatever order the rows are selected, including a display order that has been sorted or filtered. |
| MPM-7 | P1 | Selection reality check: because selection is a contiguous rectangle (section 4.4), the parts of a message are only selectable together when they are adjacent in the current view. Users get there today by filtering or sorting. Whether to add non-contiguous row selection is an open question (Q-4). |

#### 4.11.3 Exception call-stack presentation (EXC)

Exception objects carry a `callStack` string that is accurate but hard to scan. The transformation runs on the client so the raw query result stays intact. Every step below is display-only.

Given a call stack string, produce a shorter string with one useful frame per line:

| ID | Pri | Requirement |
| --- | --- | --- |
| EXC-1 | P0 | **Normalize line breaks.** Convert both real line breaks and the two-character escape sequences for return and newline (a backslash followed by `r` or `n`) into a single separator, so that frames the telemetry put on one line, and frames it split across lines, are handled the same way. This step must run **after** path shortening (EXC-2) or otherwise avoid touching backslashes inside a path. |
| EXC-2 | P0 | **Shorten source paths.** A Windows path followed by a line reference such as `D:\a\_work\1\s\Core\Example.cs :line 114` becomes the file name and line only: `Example.cs:line 114`. Whitespace around the colon and after `line` is tolerated. Both drive-letter and UNC (`\\server\share\...`) paths are handled. POSIX paths (`/mnt/build/File.cs:line 12`) are left as they are (VS Code parity); shortening them is P2. |
| EXC-3 | P0 | **Split into frames before filtering.** A frame starts at each `at ` followed by an identifier character or `<`. Splitting always precedes noise filtering, so a line that contained both an application frame and a runtime frame never hides the application frame. |
| EXC-4 | P0 | **Simplify compiler-generated names.** An async state-machine frame `Type.<Method>d__N.MoveNext()` reads `Type.Method()`. A lambda or display-class frame such as `Type+<>c__DisplayClassN_M.<Method>b__K(args)` reads `Type.Method()`. |
| EXC-5 | P0 | **Remove runtime noise.** Frames whose text contains any of these are dropped: `System.Threading.Tasks.`, `System.Threading._IOCompletionCallback.`, `System.Threading.ExecutionContext`, `System.Threading.ThreadPool`, `System.Runtime.CompilerServices.`, `System.Runtime.`, `System.Net.`, `System.IO.`, `System.Text.Json.`, `System.Diagnostics.`, `System.Collections.`, `Polly.`. |
| EXC-6 | P0 | **Never erase a stack.** If filtering would remove every frame, the unfiltered frame list (after steps 1 to 4) is shown instead. In particular an inner exception whose only frames are framework or callback frames must still show them. |
| EXC-7 | P0 | Application frames keep their `in File.cs:line N` suffix on the **same line** as their own frame. |
| EXC-8 | P0 | Output has one frame per line, in original order. **Decided (Q-13): this deliberately differs from VS Code.** VS Code joins a frame that ends in `:line N` with the next frame on the same line and keeps chaining, and two of its unit tests and `docs/FORK_DESIGN.md` describe that joined form. |
| EXC-9 | P1 | The noise list is user-extensible in a setting. VS Code hard-codes it. |

Reference behaviour is pinned by these examples (from `rowDetailsView.test.ts`); all become test vectors (section 6). Two further VS Code tests expect the joined output described under EXC-8 and would be rewritten if Q-13 is decided as recommended:

| Input | Output |
| --- | --- |
| `at Contoso.Service.Handler.<HandleAsync>d__12.MoveNext() at System.Runtime.CompilerServices.AsyncTaskMethodBuilder.Start() at Contoso.Service.Program.Main() at System.Net.Http.HttpClient.SendAsync() at Polly.Retry.AsyncRetryEngine.ImplementationAsync()` | `at Contoso.Service.Handler.HandleAsync()` / `at Contoso.Service.Program.Main()` (two lines) |
| `t+<>c__DisplayClass21_0.<LoadIntoBufferAsync>b__0(Task copyTask) \r\n at System.Threading.Tasks.Task.Execute()` (`\r\n` here is the four-character escape, as in the VS Code test) | `t.LoadIntoBufferAsync()` |
| `at System.Net.Http.HttpClient.SendAsync()` (only frame) | unchanged (EXC-6) |
| `{ "callstack": "at App.Work() at System.IO.File.ReadAllText()" }` (nested at any depth) | `callstack` becomes `at App.Work()`; other properties unchanged |

**Two behaviours in VS Code that this spec does not carry over** (details and options in section 7):

1. Frame joining, described under EXC-8. This is documented and tested behaviour, so it was treated as a product decision (Q-13, decided: one frame per line), not a plain defect. The design doc's own requirement, "at least one displayed line per stack frame", contradicts it.
2. VS Code replaces any `\r` or `\n` character pair with a space **before** shortening paths, so a Windows path containing a directory that starts with `r` or `n` (for example `D:\repos\node\x.cs`) is corrupted: `at X() in D:\repos\node\src\Foo.cs :line 12` becomes `at X() in D: epos ode\src\Foo.cs :line 12` and the path is not shortened. This one is a defect (confirmed by running the code).

### 4.12 Structured activity view (ACT)

Wireframe: W-8.

Applies to a table containing both a `CurrentActivityId` and a `ParentActivityId` column. Column names match exactly first, then case-insensitively.

**Projection rules**

| ID | Pri | Requirement |
| --- | --- | --- |
| ACT-1 | P0 | Rows are grouped into **activities** by `CurrentActivityId` (trimmed text). All rows of one activity remain individual **events**, in source order. A row with a missing or blank `CurrentActivityId` becomes its own single-event root activity, labelled `(missing CurrentActivityId)`, so no row disappears. |
| ACT-2 | P0 | An activity's parent is the one distinct non-blank `ParentActivityId` found in its rows. Parent relations form a **forest**; multiple roots are normal. |
| ACT-3 | P0 | Anomalies never drop rows. An activity whose parent does not exist in the result is a root and is marked *orphan*. An activity whose rows name more than one parent is a root and is marked *conflicting parents* (no guess is made). In a parent **cycle**, the edge of the earliest-observed activity in the cycle (by first source row) is broken, that activity becomes a root and is marked *cycle*, and the rest of the branch keeps its relationships. |
| ACT-4 | P0 | Order: roots by first source row; children by first source row; events in source order. |
| ACT-5 | P0 | Each activity records the number of activities in its branch (itself included) and the depth of its deepest descendant chain (0 for a leaf). |

**Layout and interaction**

| ID | Pri | Requirement |
| --- | --- | --- |
| ACT-6 | P0 | The structured tab is a split: the activity tree on the left and the standard results grid on the right. **Selecting an activity shows only that activity's events in the grid.** The grid keeps sorting, filtering (FLT-12), search, column layout, severity highlighting, selection, source-row identity in the gutter, and inspector synchronization exactly as in the data tab. Initially the first root is selected and every branch is collapsed. |
| ACT-7 | P0 | A tree node shows, left to right: a disclosure marker (expand, collapse, or a leaf bullet); a warning marker when applicable (ACT-10); for branches, a depth badge `↓N` whose tooltip reads `N level(s) below; M activities in this branch`; the event count in parentheses; the activity's `MarkerName` when the table has that column (value from the activity's first event; column match case-insensitive); and the activity id. (Zed places the depth badge and the count before the names so they line up in columns; VS Code puts them after.) The node tooltip is the activity id, plus ` — <MarkerName>` when present, plus the severity sentence when ACT-10 applies. |
| ACT-8 | P0 | Mouse: click selects the activity; click on the disclosure marker toggles expansion; double-click toggles expansion. Keyboard on a focused node: Enter or Space selects; Right expands; Left collapses, or if already collapsed selects the parent. The tree exposes tree semantics (levels, expanded, selected) to assistive technology. |
| ACT-9 | P0 | **Severity colouring of nodes** (uses the grid's palette, SEV-2). An activity's **final event** is its last event in source order, excluding descendants. If the final event's severity is warning, error or critical (level 3, 2, 1), the node takes that level's colour at full strength. If the final event is normal, verbose or has no usable severity, but any *earlier* event of the same activity is warning, error or critical, the node takes the **worst** earlier level's colour at **30 %** strength, meaning the colour's own alpha is multiplied by 0.30 (VS Code mixes the palette colour 30 % with transparent); this marks a *handled* issue. Otherwise, if the final event has a usable level, the node takes its colour at full strength. Otherwise the node is uncoloured. Text and controls are never faded. Child severity never changes the parent's colour. Applies only when the table has a `level`/`severity` column. |
| ACT-10 | P0 | A **warning triangle** appears on an activity only when it has its own warning, error or critical event, whether that event is final or an earlier handled one. Orphan, conflicting-parent and cycle anomalies do not show the triangle. Its tooltip is `Final <critical/error/warning> event in this activity` or `Earlier <...> event in this activity`. |
| ACT-11 | P0 | **Deepest** action: shown in the tree heading only when some activity has depth greater than 0, labelled `Deepest · D` with tooltip `Reveal deepest activity (K at level D)`. Each use selects the next activity among those tied at the greatest depth (cycling), expands its ancestors, selects it, focuses it and scrolls it to the centre. Depth here is the tree level of the activity's first event, counted from 0 at a root. |
| ACT-12 | P0 | A draggable **splitter** separates the panes. Default tree width 340 px, minimum 180 px, the events pane keeps at least 280 px. Keyboard on the splitter: Left/Right move it 20 px (80 px with Shift), Home and End go to the limits. Double-click resets to the default. The splitter exposes separator semantics with min, max and current values. |
| ACT-13 | P0 | Selecting an activity clears grid selection and republishes an empty selection to the inspector. |
| ACT-14 | P1 | The ordinary and structured grids of one table save column layout independently (COL-4). |
| ACT-15 | P1 | For large results the structured grid must not duplicate the whole table in memory: it shares the rows of the ordinary view (VS Code embeds them once for this reason). It initializes when its tab is first shown. |
| ACT-16 | P1 | Structured view is available in the same places as the data tab, including live results in the bottom panel and the reuse tab. VS Code offers it only in `.ktt` document tabs, which includes each live run when results are shown in an editor tab in new-tab mode, but not in the bottom panel or the reuse tab (Q-2). |
| ACT-17 | P2 | Surface hierarchy anomalies as a tooltip line on the node (`Parent not found`, `Conflicting parents`, `Cycle broken here`) without using the warning triangle. VS Code records the flags but does not show them. |
| ACT-18 | P1 | **Time** (Zed addition). When the table has a readable timestamp (TRC-1), a node also shows how long the activity ran and when it started, as an offset from the start of the trace, in two columns before the marker (`4.2 s`, `+0.045 s`). The tooltip gives the absolute start in UTC to the millisecond, the duration to a readable precision and, for an activity with children, how much of it is not covered by traced work (TLN-3), and says the times come from the logged events and include the activity's descendants (TLN-1, TLN-2). An activity with no readable timestamp shows empty columns. A table with no timestamp column shows neither column. |

### 4.13 Persistence (PER)

| ID | Pri | Requirement |
| --- | --- | --- |
| PER-1 | P0 | **Save As** writes the displayed result set to a file. The dialog offers `.ktt` (KustoTraceTools Results) and legacy `.kqr`, defaults to `results.ktt`, and appends `.ktt` when the chosen name has neither extension. The saved file opens in a result tab. |
| PER-2 | P0 | Reading and writing must stay compatible with the VS Code file: a JSON document with `query`, `cluster`, `database`, `parameters`, `executionStartedAt`, `executionDurationMs`, `clientRequestId`, `tables[]` (each with `name`, `columns[{name,type}]`, `rows[][]`), `charts[]`, `tableViews[]` and `chartViews[]`. Unknown properties (charts today) must be preserved untouched on a round trip. Legacy `.kqr` files open the same way. |
| PER-3 | P0 | **View state** is saved per table view in `tableViews[]`: `name` (the table name, or `<table>::activity-structured:<index>` for the structured grid), optional `gutterWidth`, and `columns[]` of `{ index, width? }` in display order. Missing columns are appended in original order; out-of-range indices are ignored. Filters, search, sort and selection are not saved (FLT-11). |
| PER-4 | P0 | Opening a `.ktt` file shows the same tabs as a live result (data tabs, structured tabs, Query tab), with saved layout applied. Column layout changes made in a saved result are written back to the file automatically; the file is not otherwise editable from the viewer. An external edit to an open file re-renders it. A file that is not valid JSON shows `Invalid result file.`, and one with no tables shows `No result data found.` |
| PER-5 | P1 | **History.** Each completed run is stored as a `.ktt` document in an application-managed history location, indexed with: file name, timestamp, a short label taken from the first meaningful comment in the query (else from the query text), cluster, database, row count, start time, duration, client request id. History keeps the most recent 200 entries. Opening a history entry shows it without rerunning. |
| PER-6 | P1 | Result files must remain faithful to what the server returned. No display transformation (JSON formatting, call stack trimming, multipart assembly) is ever written into a result. |

### 4.14 Settings (SET)

Wireframe: W-12. Zed setting names are not fixed here; the behaviours are.

| ID | Pri | Setting | Default | Meaning |
| --- | --- | --- | --- | --- |
| SET-1 | P1 | Results location | panel | `panel` (bottom dock), `beside` (editor tab beside the query), `main` (editor tab in the main area). |
| SET-2 | P1 | Editor result mode | new tab | With `beside` or `main`: `new tab` opens each completed run in its own history-backed tab; `reuse` replaces one shared tab. |
| SET-3 | Dropped | Page size | | Went with paging (GRD-5). |
| SET-4 | P0 | Severity colours | see SEV-2 | Five colours, critical to verbose. Empty means theme default. **Built** as `kusto_results.severity_colors` (`critical`, `error`, `warning`, `normal`, `verbose`). |
| SET-5 | P1 | Call stack noise list | built-in list (EXC-5) | Extra frame prefixes to hide (EXC-9). |

### 4.15 Commands and shortcuts (CMD)

Command names are indicative; the point is that each of these is invocable from the command palette and, where noted, a key.

| ID | Pri | Command | Default key (VS Code today) |
| --- | --- | --- | --- |
| CMD-1 | P0 | Run Query | F5, Shift+Enter (in a Kusto editor) |
| CMD-2 | P0 | Cancel Query | none (toolbar, inline action, notification) |
| CMD-3 | P0 | Copy (selection or table) | Ctrl/Cmd+C in the grid |
| CMD-4 | P0 | Copy as Markdown / HTML / datatable | none |
| CMD-5 | P0 | Toggle Search | none |
| CMD-6 | P0 | Show Row Details | none |
| CMD-7 | P0 | Clear All Filters | none |
| CMD-8 | P1 | Save Results As | Ctrl+S / Ctrl+Shift+S in a standalone results view |
| CMD-9 | P1 | Rerun Query (from a saved result) | none |

### 4.16 Non-functional requirements (NFR)

| ID | Pri | Requirement |
| --- | --- | --- |
| NFR-1 | P0 | **Scale.** Open, scroll, sort, filter and search a table of 100k to 500k rows and 15 to 30 columns, including nulls, duplicate values, wide strings and dynamic values, without freezing the window. Record a baseline (load time, memory, scroll frame rate, sort and filter latency) on named hardware and set numeric targets from it, as the existing design doc already requires. |
| NFR-2 | P0 | **Responsiveness.** Interaction never blocks on data work; long work is cancellable or superseded (typing a new filter value discards the previous evaluation). |
| NFR-3 | P0 | **Theming.** All colours come from the active theme, with the explicit exceptions of the user-configured severity colours and the JSON token colours' theme fallbacks. Light and dark themes are both supported. |
| NFR-4 | P0 | **Data fidelity.** Values are displayed and copied exactly as returned, apart from the stated display-only transformations. Values are never interpreted as markup or executed. |
| NFR-5 | P0 | **Concurrency.** Results, badges, selections and inspector content always belong to the run and table that produced them (RUN-5). |
| NFR-6 | P0 | **Accessibility.** Every interactive control has a text label and a keyboard path: tree (ACT-8), splitter (ACT-12), filter popover (FLT-2), busy state (GRD-9), sortable headers, pressed state on funnels. Grid cells are reachable by keyboard (SEL-9). |
| NFR-7 | P1 | **Robustness.** A malformed cell, an unexpected type, a missing severity column or a malformed `.ktt` file produces a readable message or a fallback display, never a blank or crashed panel. |
| NFR-9 | P1 | **Numeric and structural fidelity.** `long` values above 2^53, numeric literals such as `5.0`, and the key order and duplicate keys of dynamic values are displayed and copied exactly as returned, not as a JavaScript-style parse would normalise them. Supports NFR-4 and PER-6. |
| NFR-8 | P1 | **Memory.** Large results are held once; derived views (structured grid, filtered view) reference rows rather than copying them. |

## 5. Deferred and adjacent requirements

Recorded so intent is not lost. Not part of the first delivery.

### 5.1 Query parameter profiles (QPP)

Source: `docs/FORK_DESIGN.md`, `src/Client/features/queryParameterProfiles.ts`.

- QPP-1. Queries declare parameters with `declare query_parameters(name:type, ...)`. Values are passed as **native Kusto query parameters**, never by rewriting the query text.
- QPP-2. Profiles (named sets of values) live in a workspace file, `.kusto/parameters.yaml`, of the shape `active: <name>` and `profiles: { <name>: { <param>: <value> } }`. Editing the file takes effect without restart, and changes made through a UI picker update the file.
- QPP-3. A saved query file may have a sidecar `<query>.parameters.yaml` in the same format that overrides the workspace file for that query. An action creates the sidecar from the effective profiles and opens it.
- QPP-4. A status bar picker switches the active profile. Other actions: create, edit, import from clipboard JSON, open workspace file, open query file, insert declarations for the active profile.
- QPP-5. The parameter values used for a run are stored with its result (RUN-6).

### 5.2 Agent access to results (AGT)

Source: `docs/FORK_DESIGN.md`, `src/Client/features/savedQueryResults.ts`.

- AGT-1. The user can hand existing results to the agent as context, without re-running the query. The first supported scope is the **selected rows**; the whole result set is optional.
- AGT-2. Before sharing, the UI shows which rows will be sent, how much data that is (with limits for large tables), and whether the data is raw table data, formatted row details, or both. Formatted details include reassembled multipart payloads.
- AGT-3. Handoff is context transfer only. It gives the agent no connection and no permission to re-run the query. New queries run through the normal user-initiated flow.
- AGT-4. **What VS Code actually implements today** is narrower than AGT-1 to AGT-3: a Copilot tool that takes a full client request id (CID), reads the matching stored History result, and returns it as markdown (optionally for one table), at most 100 rows by default and 1000 at most, with a note stating how many rows were shown. There is no selected-rows handoff or pre-share disclosure yet. The CID comes from the **Copy CID** action (RUN-9).

### 5.3 Sequence diagram view (SEQ)

A Zed addition; VS Code has nothing like it. It shows a structured trace as a sequence diagram: the actors are the processes that logged the trace, and an arrow is a call from one process to another. The point is to answer "which service called which, in what order, and where did it fail" without reading the activity tree.

**Where the rules come from.** They were found by building a prototype (a throwaway script, not in the repo) and running it on two real traces, then fixing what went wrong. Both traces have the columns `CurrentActivityId`, `ParentActivityId`, `MarkerName`, `ProcessName`, `TIMESTAMP`, `Severity` and `MessageText`.

| Trace | Rows | Activities | Actors | Parent-to-child edges that cross actors | Calls after merging | Items drawn |
| --- | --- | --- | --- | --- | --- | --- |
| Sample 1 | 1,002 | 164 | 4 | 4 of 163 | 4 | 4 |
| Sample 2 | 6,413 | 1,314 | 5 | 56 of 1,312 | 31 | 7 (6 arrows and 1 loop) |

No activity in either trace has events from more than one actor. Sample 2 has one root with 1,312 of the 1,314 activities and a caller that is not in the result.

Every requirement is a Zed addition. The projection (SEQ-2 to SEQ-14) is UI-free and lives with the other projections in `kusto_results`; Mermaid text is one output of it (SEQ-15).

**Projection**

| ID | Pri | Requirement |
| --- | --- | --- |
| SEQ-1 | P1 | The **Sequence** tab is offered for a table that has the activity columns (ACT) and resolves the *actor* and *timestamp* roles (TRC-1). It is available in the same places as the structured tab (ACT-16). When a role is missing the tab is not offered. |
| SEQ-2 | P1 | An activity's **actor** is the actor-column value of its first event. Blank values form one actor labelled `(unknown)`. Actor names are shortened for display (common namespace prefix and generic suffix removed, so `Microsoft.ASPaaS.FrontEnd.Service` reads `FrontEnd`); the full name is the tooltip. Two actors that shorten to the same name keep their full names. |
| SEQ-3 | P1 | A **call** exists where a child activity's actor differs from its parent's actor (ACT-2). Child activities of one parent that are in the same callee actor and overlap in time (from first to last event) are one call; non-overlapping ones are separate calls. This matters because one request often leaves two sibling activities in the callee, and one parent activity can make dozens of calls (Sample 2: 25 calls under one parent). |
| SEQ-4 | P1 | **Nesting follows the activity tree, never the timestamps.** A call made inside another call's callee is drawn inside it. Timestamps only order siblings and give durations (first to last event of the callee activities). The first logged event of an activity can be later than its children's, and process clocks differ, so a timestamp-ordered diagram shows overlapping activations. |
| SEQ-5 | P1 | **Framing.** The root activity (no parent in the result) with the largest branch frames the diagram: an arrow from a pseudo-actor `(caller not in result)` into the root's actor opens it and the matching return, with total duration and an error mark when one applies, closes it. Other roots that contain calls are drawn the same way after it. Roots with no calls are not drawn but counted in a footer note (`N activities not shown`), so nothing is dropped silently (ACT-3). |
| SEQ-6 | P1 | **Collapsing.** Consecutive sibling calls with the same caller actor, callee actor and label, and with no nested calls and no errors, become one `loop` block showing the count, the time span from the first to the last call, and the shortest and longest duration. Collapsing can be switched off (SEQ-19). |
| SEQ-7 | P1 | The **arrow label** is the `MarkerName` of the calling activity (its first event), with a configurable namespace prefix removed. A callee's marker is not drawn by default, because it is usually a generic entry marker (`...IncomingRequest`); it is shown in the details (SEQ-10). |
| SEQ-8 | P1 | **Steps explain why.** Calls are grouped under the **step** that caused them: the in-process ancestor of the calling activity that sits a configurable number of levels below the framing root (default 1). A step is drawn as a labelled block around its calls. In Sample 2 the token call and the 25-call loop fall under one step and the failing request under another, which says more than the root alone. |
| SEQ-9 | P1 | **Wrapper markers** (default: names ending `IncomingRequest`; a list the user can change, with `*` at either end as a wildcard) are skipped when choosing a step label, and an arrow whose own label would be one is drawn `caller marker → callee marker` instead. The default is narrow on purpose: a prefix such as `WCL-` also matches informative markers like `WCL-MwcAccessInfoProvider-GetAzureSqlS2SAccessTokenAsync`. |
| SEQ-10 | P1 | **Details for an arrow** show the callee marker, the activity ids, the source rows, the start offset from the trace start, the duration, and the chain of in-process ancestor markers from the calling activity up to the root. |

**Errors and warnings**

| ID | Pri | Requirement |
| --- | --- | --- |
| SEQ-11 | P1 | A call whose callee branch contains an event of severity 2 or worse has its return arrow marked as an error, and so does the frame. A note is drawn only for an **origin**: an activity with an error of its own and no activity below it (in any actor) with an error. An error that each caller logs again on the way up is therefore told once, where it began, and the callers show only the error mark. The note sits on the call in whose scope the origin ran. Notes with the same actor, marker and message merge into one with `×N`; at most 3 per scope, the rest summarised as `+N more distinct errors`. Rows that say nothing about the failure (later parts of a split message, split notices, scope-end markers) are never the note text; the first part of a split message is allowed, and when the message is JSON its `message` property is used, even when the JSON is cut off. Using the assembled multipart text (MPM) is not done yet. |
| SEQ-12 | P1 | Note text wraps at about 60 characters, at word boundaries, for at most 3 lines, with `<br/>` between the lines, and ends in an ellipsis when it was cut. Arrow and step labels are cut at 64 characters. Both limits can be changed. |
| SEQ-13 | P2 | **Handled errors.** An activity that logged an error and then ended with a normal event (the *muted* outcome of ACT-9) recovered from it. It is not an origin, does not mark its callers as failed and gets no note. Each actor's participant label carries a count of them (`FrontEnd ⚠ 1`). In Sample 2 a certificate warning that the service handled no longer appears next to the real failure. |
| SEQ-14 | P2 | **Warning summary in a loop.** A loop shows the warning its calls logged most often, with a count (`⚠ marker: text ×25`). Messages matching the routine list (default `*request completed*`; a `*` at either end matches any text) are skipped, so a completion line logged at warning level does not hide the warning that matters. |

**Output and presentation**

| ID | Pri | Requirement |
| --- | --- | --- |
| SEQ-15 | P1 | The projection renders to **Mermaid** `sequenceDiagram` text, which the tab draws with the existing markdown view (which uses `mermaid_render`) in the active theme (NFR-3). That view already has the code toggle, a copy button and zoom, so the tab adds none of its own and the text can be copied for a wiki, a pull request or a ticket. The text is always valid: `;`, `#`, `%`, quotes and backslashes in values are removed or replaced, and values are never interpreted as markup (NFR-4). |
| SEQ-16 | P1 | The diagram is built when the tab is first shown, in a background task that is superseded by a newer request (NFR-2). Until it is ready the tab shows a busy state. It reads the rows of the ordinary view and does not copy them (NFR-8). |
| SEQ-17 | P1 | Width is bounded: shortened actor names (SEQ-2), labels and notes cut to a set length (SEQ-12), and a tab that scrolls in both directions and can zoom. Measured on Sample 2 the unbounded diagram is 3,370 by 1,183 px at 1x. |
| SEQ-18 | P1 | A limit on arrows drawn, default 300, a loop counting as one. The first arrows in drawing order are kept, and the footer says how many calls are not shown. Calls nested more than 50 deep are counted the same way. The number is a starting point and should be set from measuring a large trace (NFR-1). |
| SEQ-19 | P1 | **Options.** The setting `kusto_results.sequence` holds the step depth (0 for no steps), loop collapsing, the wrapper list, the routine-warning list, the generic actor suffixes and the arrow limit. The Sequence tab has two controls, **Steps** (off, 1, 2, 3) and **Collapse loops**, that change the tab being looked at and draw the diagram again; they start from the setting and are not saved. Not built: including in-process activities that have an error or last longer than a set time, and marker include and exclude patterns. |
| SEQ-20 | P2 | **Click-through.** Selecting an arrow selects the callee activity in the structured tab and its rows in the grid. A picture cannot do this; it needs either a hit map over the rendered image or a native renderer. The projection already keeps the activity ids and source rows of every call (SEQ-10), so nothing here blocks it. |
| SEQ-21 | P2 | **Saved diagram.** The diagram is built on demand and not stored (Q-17). Only a diagram the user has edited (SEQ-22) is stored in the result file, per table, as the extra property `sequenceDiagrams`: the text, a generator version and the options used. A missing or invalid stored diagram is never an error. The property is an unknown property to every other reader and must survive their round trip (PER-2). |
| SEQ-22 | P2 | **Edited diagram.** If the user edits the text, it is stored as edited and never replaced by a rebuild; a `Regenerate` action discards the edit after confirmation. The stored text is a derived display and never changes the table data (PER-6). |

**Acceptance vectors (spec).** Measured on the two real traces, which are git-ignored and cannot be committed, so a synthetic trace with the same shape must be added to `fork-docs/samples/` (`generate_samples.py`) before tests are written.

- SEQ-V1. Sample 1 gives 4 calls in one frame; the failing one is marked, and its note names the deepest error.
- SEQ-V2. Sample 2 gives 31 calls, drawn as 6 arrows and 1 loop of 25 (SEQ-6), inside one frame whose root is the activity with 1,312 of the 1,314 activities (SEQ-5).
- SEQ-V3. In Sample 2 each of the 25 loop calls has two sibling activities in the callee that merge into one call (SEQ-3).
- SEQ-V4. A trace in which an activity's first logged event is later than the first event of one of its child activities, or in which two actors' clocks disagree, still draws every call inside its parent's call, with no overlapping activations (SEQ-4).
- SEQ-V5. A trace with a message containing `;`, `#`, `%` and a quote produces text that Mermaid accepts (SEQ-15).
- SEQ-V6. A trace with one root and no cross-actor edges produces the frame and a footer line, not an empty diagram.

**Spike result (rendering).** The Mermaid text for Sample 2 was rendered with merman 0.8.0-alpha.5, the engine inside `mermaid_render`, through its raster-safe pipeline and then rasterised with resvg 0.46. Participants, numbering, activation bars, dashed returns, `loop`, notes and the symbols `×`, `✗` and `⚠` all rendered, in about 0.6 s. A `rect` block, used for steps (SEQ-8), also renders: a shaded band across the actors it touches, with the step label as a note. Not yet checked: Zed's own theme and accent styling, and a diagram with hundreds of calls.

### 5.4 Trace schema (TRC)

The activity view and the severity colours look for fixed column names (`CurrentActivityId`, `ParentActivityId`, `MarkerName`, `level` or `severity`), and the sequence view needs two more roles. Other schemas will use other names, so the roles should be mapped in one place.

| ID | Pri | Requirement |
| --- | --- | --- |
| TRC-1 | P1 | A **trace schema** maps roles to column names. Roles: activity id, parent activity id, marker, actor, timestamp, severity, message. The activity view needs the first two; the others are optional for it. The sequence view needs activity id, parent, actor and timestamp. |
| TRC-2 | P1 | Resolution order: a schema named in a setting, then the built-in names matched exactly and then case-insensitively (as ACT does today). The first schema whose required roles all resolve wins. |
| TRC-3 | P1 | A setting holds a list of schemas, each with a name and a column name per role, and optionally the column names that must be present for it to apply, so one setting can serve several clusters. Names follow the existing `kusto` settings. |
| TRC-4 | P1 | Every feature that reads a trace column (ACT, SEV, SEQ, and any later analysis such as AGT) uses the resolved schema. No feature keeps its own fixed names. |
| TRC-5 | P2 | A schema can be set for a single query or file, for example in a connection-style comment like `// :setDefaultCluster(...)`. Not decided. |
| TRC-6 | P1 | When a tab is not offered because a role is missing, the tab bar says which role could not be resolved, so the user knows what to configure. |

**Status.** TRC-1 to TRC-4 are built for the activity view and the severity colours: `trace_schema.rs` in `crates/kusto_results`, and the `kusto_results.trace_schemas` setting, read by the grid and the viewer in `crates/kusto_results_ui`. A change to the setting applies to results opened afterwards, not to ones already open. The projection (SEQ-2 to SEQ-14, SEQ-17, SEQ-18, and the text of SEQ-15) is built in `sequence.rs`, with tests. The Sequence tab, built when first shown (SEQ-1, SEQ-15, SEQ-16) and the note that names a missing role (TRC-6) are in `crates/kusto_results_ui`; the diagram is drawn by the markdown view. Its tests cannot show the drawn diagram, so that has to be checked in a window. The options (SEQ-19) are the `kusto_results.sequence` setting and the Steps and Collapse loops controls in the tab. Not built yet: slow or failing in-process activities and marker patterns (the rest of SEQ-19), click-through (SEQ-20, which needs a diagram that can be hit-tested, so a native renderer), saving an edited diagram (SEQ-21, SEQ-22, which need somewhere to edit it) and a per-file schema (TRC-5, not decided).

### 5.5 Timeline model (TLN)

The facts about *time* and *failure* in a structured trace, computed once from the activity tree and used by the findings strip (5.6), the waterfall (5.7) and the sequence view (5.3). Nothing here draws anything. It lives in `crates/kusto_results` next to the activity projection.

**Where the rules come from.** Measured on the two real traces used for the sequence view (git-ignored, so the numbers are recorded here).

| | Sample 1 | Sample 2 |
| --- | --- | --- |
| Rows, activities | 1,002, 164 | 6,413, 1,314 |
| Activities under 1 ms | 80 | 1,234 (25 of them have a single event, so zero duration) |
| Activities of 100 ms or more | 10 | 3 |
| Children that start before, or end after, their parent | 5 of 163 | 2 of 1,312 |
| Most siblings overlapping at once | 18 | 2 |
| Largest self time | about 6.0 s in one activity | about 4.2 s of an activity of about 4.5 s, which holds 25 calls that add up to about 0.3 s |
| Scope start and end rows | 328 (33%) | 2,474 (39%) |

| ID | Pri | Requirement |
| --- | --- | --- |
| TLN-1 | P1 | **Bounds.** An activity runs from its earliest to its latest event that has a readable timestamp (the timestamp role, TRC-1), counting every row including structural ones (TLN-6). An activity with one such event has zero duration; one with none has no bounds and is never given invented ones, not even its children's. Durations are only as good as what was logged, and the UI says so where it shows one. |
| TLN-2 | P1 | **Widening.** An activity that has bounds is widened, bottom up, to start no later than its earliest descendant and end no earlier than its latest, because a scope is still running while the work it started is. In Sample 1 a parent logged for 137 ms while its child ran for 6.2 s; without widening the trace reads 137 ms and 6 s of work is clipped away, with it the trace is 6,974 ms, its true span. The number of activities whose own events fall outside their parent's own is kept, because it is a data-quality fact (FND-3). |
| TLN-3 | P1 | **Self time**, shown to users as *untraced time*. An activity's duration minus the union of its clipped children's intervals. It is time the trace does not explain: the process may be waiting on something outside the trace, doing work that logs nothing, or sleeping. It is never called slow code. |
| TLN-4 | P1 | **Critical path.** Walk back from the end of an activity: the child that finishes last (not after the cursor) is on the path, the gap between the cursor and its end is the activity's own time, then continue from the child's start and ignore children that overlap it. Recurse into each child on the path. Every tick of a root's duration is attributed to exactly one activity's own time, so the attributions sum to the root's duration. A chain of "the child that ends last" is not enough: in Sample 2 it follows the final 76 ms request and misses the 4.2 s before it. |
| TLN-5 | P1 | **Repetition.** Children of one parent with the same marker, at least 5 of them (a setting), form a repeat. It records the count, the time the repeats cover (union of their intervals), the shortest and longest duration, the first and last start, and the activities. |
| TLN-6 | P1 | **Structural rows.** A row whose message matches a configurable list (default `Monitored scope start*` and `Monitored scope end*`; `*` at either end matches any text) says that something started or ended and carries no content. Structural rows still set an activity's bounds (TLN-1). They are left out of findings text and templates, and can be dimmed or hidden in the grid (TPL-1). The setting is `kusto_results.structural_messages`. |
| TLN-7 | P1 | **Failures.** The analysis behind SEQ-11 and SEQ-13 is computed once and shared: an activity's error row, whether it recovered (ACT-9, muted), whether anything below it failed, and which activities are failure origins. |
| TLN-8 | P1 | Each computation is linear or n log n in the number of activities and rows and does not recurse on the depth of the tree (NFR-1). |

**Status.** All of TLN-1 to TLN-8 are built: `timeline.rs` and `failures.rs` in `crates/kusto_results`, with tests, and checked on both real traces (ignored tests print the numbers). On Sample 2 the critical path attributes 4,208 ms to `ListTablesWithSchemas`. The sequence view reads its times and failures from them. The setting `kusto_results.structural_messages` is built (TLN-6); the repeat minimum is not a setting yet.

### 5.6 Findings strip (FND)

A short list of facts about the trace in front of you, each one linked to the rows it comes from. It answers "what is notable here" before any reading. Every finding is computed locally and deterministically; none comes from a model. It is also the right input for an agent (AGT): a few evidence-linked findings are a far better prompt than thousands of rows.

| ID | Pri | Requirement |
| --- | --- | --- |
| FND-1 | P1 | The **strip** sits above the grid, the activity tree and the sequence and timeline views of a trace result (a table with the activity columns, ACT). It is one line when collapsed (the number of findings by severity and the headline) and a list when expanded. It starts collapsed. |
| FND-2 | P1 | A **finding** has a kind, a severity (error, warning, information), a one-sentence title, optional detail, the source rows and activities it comes from, and a magnitude used for ordering. A finding with no rows or activities is not shown; a data-quality finding names the rows involved. |
| FND-3 | P1 | **Catalogue.** (1) *Failure origins*: each origin with its actor, marker and message and how far it passed up; identical failures are one finding with a count. (2) *Untraced time*: an activity that has children and whose own time (TLN-3) is at least 10% of the trace's duration and at least 10 ms, with how much of it the children cover; a leaf has no children to explain it, and slow leaves appear in the critical path. It is a warning from half the trace's duration, else information. (3) *Critical path*: the three activities with the most own time on it (TLN-4). (4) *Repetition* (TLN-5): a repeat that covers at least 5% of the trace's duration, or has at least 25 calls and covers at least 1%; many calls that take almost no time are not worth a line. The activities one call leaves behind repeat together and are one finding. (5) *Handled errors*, per actor (SEQ-13). (6) *Severity mix*: rows at level 3 or worse, linking to them. (7) *Data quality*: roots whose caller is not in the result, activities that name several parents or loop, activities that run outside their parent's time range (TLN-2), activities with no readable timestamp, and a missing severity or timestamp column. |
| FND-4 | P1 | Order: errors first, then warnings, then information; within a severity in the order of the catalogue above, then by size (share of the trace's duration, or count). At most 12 are listed, then a line `N more`. |
| FND-5 | P1 | Selecting a finding selects its rows in the grid (switching to the Data tab) or, in the Structured tab, its activity, and scrolls to it. In the Sequence and Timeline tabs it highlights what it can; those tabs add this later (P2). |
| FND-6 | P1 | Wording is factual and says what is not known. Untraced time is *not covered by traced work*, never *slow*. A duration says it is from the logged events. A partial trace says its caller is missing. |
| FND-7 | P1 | Findings are computed away from the window, when a trace result is first shown, from the same projection the other views use, and are rebuilt when the result or the trace schema changes. They are never written to the result file (PER-6). |
| FND-8 | P2 | **Copy findings** as Markdown for a ticket, and as JSON for an agent. |
| FND-9 | P2 | The strip remembers whether it was expanded, per window. |

**Status.** FND-1 to FND-7 are built: the model in `findings.rs` (`crates/kusto_results`) and the strip in `findings_strip.rs`, wired into the viewer (`crates/kusto_results_ui`), with tests, and checked on both real traces: the 6,413-row trace reads as root cause with its path, the 4.2 s not covered by traced work, the critical path, the 25-call repeat and the handled errors, built in about 20 ms. The strip starts closed and sits above every tab. Choosing a finding selects its rows in the Data tab (or its activity in the Structured tab); rows hidden by a filter or the search are named in a note. In the Sequence and Timeline tabs a choice switches to the Data tab, and highlighting there is the P2 part of FND-5. Not built: copying findings (FND-8) and remembering the expanded state (FND-9).

### 5.7 Waterfall (WFL)

The **Timeline** tab: where the time went. It is the natural next step from the activity tree, with durations, concurrency and the critical path. It is drawn natively, not as a Mermaid gantt: a gantt has no hierarchy, no interaction and does not hold thousands of rows.

| ID | Pri | Requirement |
| --- | --- | --- |
| WFL-1 | P1 | A **Timeline** tab is offered where the Sequence tab is (SEQ-1): a table with the activity columns and a readable timestamp. A missing role is named (TRC-6). |
| WFL-2 | P1 | One row per activity in tree order (ACT-4), indented by depth, with a bar from its start to its end on one shared time axis, labelled in offsets from the start of the trace. The axis scrolls and zooms in time, and the rows scroll. |
| WFL-3 | P1 | **Folding.** Most activities are too short to see: 1,234 of Sample 2's 1,314 are under 1 ms. An activity shorter than a threshold (default 1% of the trace's duration, the setting `kusto_results.waterfall.fold_share`) is folded into its parent, whose row says how many it hides and expands on demand. The chain from a failure to the root is never folded. The critical path needs no protection of its own: an activity that holds a meaningful share of it is at least that long, whereas every activity in a sequential run is "on" the path with no time of its own, and protecting them all left 132 of Sample 1's 164 rows and 910 of Sample 2's 1,314 (with only failures protected they are 32 and 21). Folding applies to rows, never to time: the part of a parent its folded children cover is not drawn as untraced. A **Show all** button opens every fold. |
| WFL-4 | P1 | **Untraced time** (TLN-3) is drawn inside a parent's bar as a hatched part; children's bars are drawn over it. |
| WFL-5 | P1 | Bars are coloured by actor. An error is a mark at its event; a handled error is a lighter mark. The warning marker follows ACT-10. |
| WFL-6 | P1 | Parallel work shows as overlapping bars on separate rows, so concurrency can be read without extra lanes. |
| WFL-7 | P1 | Selecting a row selects the activity as the tree does, and the grid and the inspector follow (ACT-6, ACT-13). |
| WFL-8 | P1 | A tooltip gives the marker, the actor, the offset and duration, the untraced time, the number of events, and the worst severity, and says the times come from the logged events (TLN-1). |
| WFL-9 | P1 | **Critical path** can be highlighted (TLN-4), with the own time of each activity on it. |
| WFL-10 | P2 | **Repetition** (TLN-5) can be shown as one row with a tick for each repeat. |
| WFL-11 | P1 | The rows are virtualised and the folding default keeps a trace of 100k activities usable (NFR-1). The model is built in a background task (NFR-2). |

**Status.** WFL-1 to WFL-9 and WFL-11 are built: the model in `waterfall.rs` (`crates/kusto_results`, with tests, checked on both real traces) and a native view, `waterfall_view.rs`, with the Timeline tab in the viewer (`crates/kusto_results_ui`). It has the label column with duration, a time axis, zoom and pan, bars coloured by actor, hatched untraced time, error and handled-error marks, a critical-path emphasis toggle, folds, Show all, a tooltip, selection that the inspector follows, a double-click that opens the activity in the Structured tab, and a finding chosen in the tab reveals its activity. Tests check the layout (a bar's place and width), the folds, the selection and the links, but not how it looks, which has to be checked in a window. Not built: repetition collapsed to one row (WFL-10, P2), search matches never folding, and a keyboard path through the rows.

### 5.8 Message templates and structural rows (TPL)

Repeated messages differ only in ids and numbers. Grouping them shows what a trace says, not each time it says it. The gain depends on the trace: Sample 1 has 674 rows of content that fall into 362 templates (1.9 times fewer), Sample 2 has 3,939 that fall into 568 (6.9 times), and the ten most common templates cover 62% of Sample 2's content rows. Markers already group (59 to 61 distinct), so templates are most useful *within* a marker. A third or more of all rows are structural (TLN-6), which setting aside helps every trace.

| ID | Pri | Requirement |
| --- | --- | --- |
| TPL-1 | P1 | **Structural rows** (TLN-6) are shown dimmed in the grid, and a **Hide structural rows** button removes them from the view in the Data and Structured tabs. Hiding is a view, never a change to the result (PER-6). The toggle starts off. |
| TPL-2 | P2 | A **template** is a row's message with the variable parts masked: GUIDs, timestamps, URLs, long hex strings, and any word that contains a digit become a placeholder; a multipart prefix `k/N:` is dropped; whitespace is collapsed; the result is cut at 200 characters. One function does this, so every feature agrees. |
| TPL-3 | P2 | A derived **Pattern** column, not stored in the result, can be shown, sorted and filtered like any column. **Group by pattern** lists each template with its count and expands to its rows. |
| TPL-4 | P2 | **Rare and bad**: a filter for rows whose template occurs once in the table and whose severity is 3 or worse. In Sample 2, 36 of 432 one-off templates qualify, which is a short list worth reading. |
| TPL-5 | P2 | The list of masks can be extended in a setting. |
| TPL-6 | P3 | A clustering method that learns templates from the data (as log-parsing tools do) is only worth building if masking leaves too many templates on real traces. On the two samples a plain regex gave 371 and 1,240 templates and masking digit words gave 362 and 568, so the quality of the masking matters more than the method. |

**Status.** TPL-1 is built: structural rows are dimmed in the grid and the Hide structural rows button, in the Data and Structured tabs, leaves them out of the view (off by default, a view only, with the count in the footer). Finding them over 500,000 rows takes about 9 ms. Not built: templates, the Pattern column, grouping and the rare-and-bad filter (TPL-2 to TPL-6).

### 5.9 Focus on an activity (FOC)

A trace can be thousands of activities, and the question is often about one step of it. Focus makes one activity the root: the Data, Structured, Sequence and Timeline tabs and the findings are all rebuilt from that activity and everything below it. It works because every view is built from one activity projection, which refers to events by their source row, so a focus is only a different set of rows fed to the same builder and the row numbers stay true. In Sample 2, focusing on `ListTablesWithSchemas` gives a trace of 4.57 s whose findings and axis are relative to it, not to the 4.72 s root.

| ID | Pri | Requirement |
| --- | --- | --- |
| FOC-1 | P1 | **Focus on this activity** is offered by a right-click on a node of the activity tree, on a row of the Timeline, and on a row of a grid of a trace (the activity that row belongs to). A row with no activity id has none to focus on and does not offer it. |
| FOC-2 | P1 | The **scope** of a focus is the chosen activity and every activity below it, and the source rows of all their events. Rows that belong to no such activity are left out. The chosen activity is a root and is not marked as an orphan (ACT-3): its caller being outside the focus is the point, not a data problem. |
| FOC-3 | P1 | Every view is built from the scope: the Data tab shows only those rows, still numbered by their source row; the Structured tab, the Sequence tab and the Timeline are built from the activities in it; the findings (FND) are about it, and their shares and the axis are relative to the focused activity. The grid keeps its search, filters and sort; its selection is cleared, as when its rows change. |
| FOC-4 | P1 | A **focus bar** above the tabs says what is focused (the marker and the activity id) and offers **Up one level**, which focuses the parent and is not offered for an activity that has none, and **Show whole trace**. With no focus the bar is not shown. |
| FOC-5 | P1 | **Focus by id.** A **Focus…** button opens a field that takes an activity id, trimmed; it matches exactly, then ignoring case, and says so when there is no such activity. This is for an id found in a log or a ticket. |
| FOC-6 | P1 | Focus is view state. It is not written to the result file (PER-6), and reloading the file clears it. It is computed away from the window (NFR-2); the full table is always kept, so Up one level and Show whole trace need no re-read. |
| FOC-7 | P2 | A key binding for Focus on this activity and Up one level. |
| FOC-8 | P2 | The focus is remembered per result for the session. |

**Acceptance vectors (spec).** FOC-V1: focusing a leaf gives a one-activity trace in every tab. FOC-V2: focusing the root changes nothing but the bar. FOC-V3: the rows of a focus keep their source numbers and are exactly those of the activity and its descendants. FOC-V4: Up one level from a root is not offered. FOC-V5: an id that is not in the trace names it and leaves the focus as it was.

**Status.** Not built.

## 6. Acceptance vectors

Concrete cases each implementation must satisfy. Vectors marked **(VSC test)** come from the VS Code unit tests. Vectors marked **(spec)** are defined by this spec from reading the code, because VS Code has no test for them. Automated tests should reproduce all of them. Fixture files that exercise these cases, with expected results, are in `fork-docs/samples/` (see its README). Wire formats follow what the VS Code server emits: datetimes as ISO 8601 with seven fractional digits, timespans as `[-][d.]hh:mm:ss[.fffffff]`.

**Multipart (MPM) (spec)**

| Case | Selected values in one column | Expected |
| --- | --- | --- |
| Complete, ordered | `1/2:{"a":`, `2/2:1}` | Assembled `{"a":1}` shown as JSON |
| Complete, reverse selection order | `2/2:1}`, `1/2:{"a":` | Same result |
| Incomplete | `1/3:x`, `2/3:y` | Not assembled; first row shown with a count note |
| Mismatched totals | `1/2:x`, `2/3:y` | Not assembled |
| Duplicate part | `1/2:x`, `1/2:y` | Not assembled |
| One ordinary row among parts | `1/2:x`, `hello` | Not assembled |
| Marker spacing | `1 / 2 : x`, `2 / 2 : y` | Assembled `xy` |
| Total of 1 | `1/1:x`, `1/1:y` | Not assembled (`N` must be at least 2) |

**Filters (FLT) (VSC test for operator behaviour; wire-format values are spec)**

| Column type | Filter | Cells | Matches |
| --- | --- | --- | --- |
| string | Contains `TIMEOUT` | `Timeout while reading`, `ok` | first |
| long | Greater than `9` | `10`, `9`, `100`, `abc`, null | `10`, `100` |
| long | Less than `5` | `3`, `7`, null | `3` only (VS Code also matches null; section 7) |
| timespan | Less than `00:01:00` | `00:00:30.0000000`, `1.00:00:00` | first |
| datetime | After `2025-01-01` | `2024-12-31T23:59:59.0000000Z`, `2025-01-01T00:00:01.0000000Z` | second |
| datetime | On `2025-01-01T00:00:00.0000000Z` | `2025-01-01T00:00:00.0000000Z`, `2025-01-01T00:00:00.0000007Z` | first only (VS Code matches both) |
| bool | Is true | `true`, `false`, `1`, `0` | `true`, `1` |
| any | two conditions, AND / OR | as expected | AND narrows, OR widens |

**Sorting (SRT) (spec)**

- `long`: `2`, `10`, null, `1` ascends null, 1, 2, 10; descends 10, 2, 1, null.
- `timespan`: `1.00:00:00`, `00:00:59`, `-00:00:05` ascends `-00:00:05`, `00:00:59`, `1.00:00:00`.
- Three clicks on any header ends in original result order with no header indicator.
- Equal keys are ordered by source row number in both directions.

**Severity (SEV) (VSC test)**

- Column `Level` with values `1`, `3`, `5`, `9`, `x`, null tints rows 1, 3 and 5 only.
- Table with neither `level` nor `severity` tints nothing.

**Activity (ACT) (VSC test unless noted)**

- Rows A(parent none), B(parent A), C(parent B): A root, chain depth 2, `Deepest · 2` selects C.
- Two roots: both listed in first-observed order.
- Parent `X` not in table: activity is a root, marked orphan, no triangle.
- Rows of one activity naming parents P and Q: root, marked conflicting, no triangle.
- Cycle A to B to A: the earlier-observed activity is the root, both rows still visible.
- Activity with events (warning, normal): node coloured at 30 % warning, triangle shown.
- Events (normal, error): full error colour, triangle shown.
- Events (normal, verbose): the **verbose** colour at full strength (the final event decides), no triangle.
- Events (verbose, normal): the **normal** colour at full strength, no triangle.
- A child with an error under a parent whose own events are normal: parent unchanged.

**Badge (PNL) (VSC test)**

- 2 rows then 0 rows then 3 rows then no tables then error then delayed retry of an empty result: badge reads 2, 0, 3, 0, error, 0 in that order, and a stale delayed render never overrides a newer one.

**Call stack (EXC) (VSC test for the four examples in 4.11.3; the rest spec)**

- The four examples in 4.11.3.
- `at X() in D:\repos\node\src\Foo.cs :line 12` becomes `at X() in Foo.cs:line 12` (path not mangled).
- `at X() in D:\repos\node\src\Foo.cs :line 12\nat Next()`, where `\n` is the two-character escape, becomes two frames: `at X() in Foo.cs:line 12` and `at Next()`.
- A three-frame stack in which every application frame has a source location is shown as three lines (Q-13).

**Search (SRC) (spec)**

- Cells `10:42:11`, `Retry 1/3`, `café`: searching `10:42`, `1/3` and `café` each matches its row (VS Code matches none of them).
- Searching `retry timeout` matches only a row that contains both words, in any cells.

**Copy (CPY) (spec)**

- No selection, filter active: Copy copies all source rows in source order.
- One cell selected: raw value only. Two cells: header row plus values, TSV.
- A value containing a tab, a newline and a double quote is quoted and doubled per CPY-2.

## 7. Where Zed differs from VS Code, or needs confirming

Every deliberate difference in one place. "Fix" means Zed does not reproduce the VS Code behaviour.

| # | VS Code today | Zed requirement | Type | Why |
| --- | --- | --- | --- | --- |
| 1 | Consecutive frames ending in `:line N` are joined into one line and keep chaining. Pinned by two unit tests and described in `docs/FORK_DESIGN.md`, which also asks for at least one line per frame. | One frame per line (EXC-7, EXC-8). | Decided (Q-13) | The stack becomes unreadable when every frame has a source location. Confirmed by running the join step. |
| 2 | The `\r`/`\n` escape replacement runs before path shortening; `\repos` and `\node` in a path become `<space>epos` and `<space>ode`. | Handle escapes and shorten paths without that interaction (EXC-1, EXC-2). | Fix | Confirmed by running the code. |
| 3 | `timespan` columns sort as text. | Sort by duration (SRT-5c). | Fix | Filters already parse timespans. |
| 4 | Filter text: date-only values are UTC, date/time without a zone is local, comparison is to the millisecond. | UTC, 100 ns resolution (FLT-6). | Fix | Kusto datetimes are UTC with 100 ns ticks. |
| 5 | Several rows selected that are not a multipart message: the first row is shown, silently. | Count note (RDT-7). | Fix | Users would think they saw all of it. |
| 6 | Null and unparseable values sort by the grid library's text comparison. | Documented rule (SRT-6). | Fix | Predictable results. |
| 7 | Structured view is missing from the bottom panel and the reuse tab, but present in every `.ktt` document tab (including live runs in new-tab mode). | Available everywhere (ACT-16). | Change | One component should not vary by placement. Q-2. |
| 8 | Hierarchy anomalies are computed but never shown. | Tooltip line, no triangle (ACT-17). | Addition (P2) | The information exists. |
| 9 | A null numeric cell is read as 0 by filters (`Less than 5` matches it); null timespan and datetime do not match, including `Not on`. | Null satisfies only `Is empty` and the negative operators (FLT-6). | Fix | An absent value is not zero. |
| 10 | Selection is cleared on sort, reorder and activity change only. After a page, search or filter change the highlight is stale and the inspector keeps the old rows. | Clear on all of them (SEL-6). | Fix | The highlight would point at unrelated cells. |
| 11 | Multi-word search is OR; the term (not the cell) loses punctuation and diacritics, so `10:42`, `1/3`, `café` never match. | AND across terms, literal matching (SRC-2, SRC-5). | Fix | Trace text is full of punctuation. |
| 12 | Strings sort with natural numeric ordering and ignore punctuation. | Natural ordering kept, punctuation not ignored (SRT-5e, Q-12). | Decided | Natural ordering is useful for step names; ignoring punctuation is not. |
| 13 | Sort is in place: in descending order ties keep the previous view order. | Ties by source row index in both directions (SRT-3). | Fix | Stable, explainable order. |
| 14 | With no selection, Copy copies the whole source table in source order, ignoring filters and sort. Whole-column and whole-table selection cover only the current page. | Copy default the same (CPY intro, CPY-10). Whole-column and table selection cover the whole view (SEL-5a). | Decided (Q-11) | A page-bounded selection surprises users. Q-11. |
| 15 | Rich HTML plus text on the clipboard on Windows only; other platforms copy plain text. | Text P0, rich HTML P1 (CPY-11). | Change | Zed's clipboard has no HTML entry type. |
| 16 | The bottom panel has no Query tab and no start time or duration; the Query tab is in editor tabs and shows cluster, database and query text. | Panel Query tab and summary line are Zed additions (PNL-6, PNL-7). | Addition | Convenient; not derived from VS Code. |
| 17 | The loading overlay appears only when a grid of 1000+ rows is first created. | Busy feedback for open, sort, filter and search (GRD-9). | Addition | Sorting 500k rows needs the same feedback. |
| 18 | Zero rows leaves the footer blank; zero matches uses the library's own `No results match your search query`. | Both messages specified (GRD-6). | Clarify | Was implicit. |
| 19 | A successful rerun overwrites the saved result; only a cancelled rerun leaves it unchanged. | Same (RUN-8). | Clarify | The docs implied otherwise. |
| 20 | The inspector keeps showing old rows after a new run or a closed result; find text and scroll are lost on any re-render. | Clear on replace or close, keep find text (RDT-11, RDT-12). | Addition | Stale data in the inspector is misleading. |
| 21 | Hiding the search box keeps the query active invisibly. | Clear it or show an indicator (SRC-7). | Fix | Rows vanish for no visible reason. |
| 22 | Numeric and structural fidelity relies on a JavaScript parse: `long` above 2^53 loses precision, and dynamic values are re-serialised. | Exact values (NFR-9). | Fix | Data fidelity is a stated principle. |
| 23 | The grid pages its rows, 1000 at a time by default. | No paging: one scrolling list of every row in the view (GRD-5). | Change | The grid virtualises, so paging adds a pager, a page-size setting and selections that cannot cross pages, and saves no work. Measured: 200,000 rows by 20 columns builds about 32 rows per frame. |
| 24 | Copy as datatable takes its literals from .NET values: a bool is written `True` or `False` (not valid KQL), reals are rounded to 15 significant digits and in a dynamic column a string loses its quotes (`dynamic(abc)`). | Lower-case `true` and `false`, reals keep the digits the server sent, a string in a dynamic column is quoted as JSON (`dynamic("abc")`). | Fix | The expression must parse and reproduce the values (NFR-9). |

## 8. Questions and decisions

Status: the recommendations below were reviewed and accepted, so Q-1 to Q-13 are decided. Q-14 is decided as "assess in phase A"; its outcome is still open. Q-3 additionally needs a check in a running VS Code before parity tests are written. Q-15 to Q-20 belong to the sequence view (section 5.3); Q-17 and Q-18 are decided and the rest are open. Q-21 to Q-26 belong to the timeline, findings, waterfall and template sections (5.5 to 5.8); Q-21 to Q-24 are decided and Q-25 and Q-26 are open. Q-27 to Q-29 belong to focus (5.9) and are decided.

| # | Question | Decision |
| --- | --- | --- |
| Q-1 | Where does the Row Details inspector live in Zed: its own right-dock panel, or a pane inside the results surface? | Own right-dock panel so it follows the selection from any grid, including editor-tab results. See design doc section 5. |
| Q-2 | Should the structured view be available everywhere (bottom panel and reuse tab too)? | Yes (ACT-16). |
| Q-3 | Multi-word search: AND or OR? | Resolved from the grid library's source: VS Code is OR. Zed is AND (SRC-5). Confirm VS Code's behaviour in a running instance before writing parity tests. |
| Q-4 | Add non-contiguous row selection so multipart parts can be picked from a mixed view? | Yes, as SEL-11 (P1). It directly serves S3. |
| Q-5 | Copy row order. | Resolved from source: a selection is copied in display order; with no selection the whole table is copied in source order. |
| Q-6 | Persist filters and sort in `.ktt`? | No for the first release (VS Code does not). Revisit if users want to re-open an investigation exactly as left. |
| Q-7 | JSON display converts escaped `\n` to real line breaks in every string. Restrict to `callStack` fields? | Keep for all strings for parity, but never apply it to keys or to text outside JSON. |
| Q-8 | Which call-stack frame filters are the Zed defaults? | Start with the VS Code list (EXC-5) and add the user list (EXC-9) so the list stops being a code change. |
| Q-9 | Does the bottom dock already host panels the user expects to keep open alongside Results? | Confirm during design doc phase A (a check, not a choice). |
| Q-10 | Keep `.ktt` as the file extension? | Yes; PER-2 requires round-trip compatibility. |
| Q-11 | Scope of whole-column and whole-table selection: current page (VS Code) or whole view? | Whole view (SEL-5a). |
| Q-12 | String sort: natural numeric ordering, and whether to ignore punctuation? | Natural ordering yes, ignoring punctuation no. |
| Q-13 | Call-stack frame joining: keep VS Code's joined form (documented, tested) or one frame per line? | One frame per line. It is what the design doc's first call-stack bullet asks for. Two VS Code unit tests expect the joined form and would need rewriting if the VS Code fork is kept in step. |
| Q-14 | Should the existing `tabular_data_preview` crate's sort and checklist filters be reused for this grid? | Assess in phase A (design doc section 3). It solves the popover and header regions, but it has string-only sort and distinct-value filters, not the typed operators required here. |
| Q-15 | How is a step (SEQ-8) drawn, and what is the right default step depth? | Open, partly answered. A `rect` block with the label as a note renders in merman and reads well on Sample 2: depth 1 gives one step for the token call and the loop, and one for the failing request. Still to try on more traces, and to see in Zed's theme. |
| Q-16 | Does the VS Code extension accept an unknown top-level property in a `.ktt` file (needed for SEQ-21)? | Open. Zed's reader keeps unknown properties (PER-2); the VS Code reader has not been checked. |
| Q-17 | Store a generated diagram in the result file, in a side file, or not at all (SEQ-21)? | Decided: build it on demand and do not store it. A diagram builds in milliseconds, and the markdown view's copy button (SEQ-15) covers sharing. Only an edited diagram is stored (SEQ-22, P2). |
| Q-18 | Is an actor the process name alone, or the process name plus the process id? | Decided: the name alone. The actor column is configurable (TRC-1) for traces where another column is the better actor. |
| Q-19 | Draw the diagram as a Mermaid picture, or build a native renderer so arrows can be clicked (SEQ-20)? | Open. The picture is enough for the first version; the projection is kept separate so a native view can follow. |
| Q-20 | What if one activity has events from more than one actor? | Open. Neither sample does this. SEQ-2 takes the first event's actor; a trace that mixes them may need splitting the activity. |
| Q-21 | Where does the findings strip go (FND-1)? | Decided: above the grid, tree and sequence and timeline views of a trace result, collapsed to one line until opened. |
| Q-22 | How are rows with no content (scope start and end) recognised (TLN-6)? | Decided: a configurable list of message patterns, default `Monitored scope start*` and `Monitored scope end*`, not detection. |
| Q-23 | What is the waterfall's folding threshold (WFL-3)? | Decided: a share of the trace's duration, default 1%, so it suits a trace of any length. |
| Q-24 | Native waterfall or a Mermaid gantt? | Decided: native (section 5.7). |
| Q-25 | What size of untraced time is a finding (FND-3)? | Open. 10% of the trace and at least 10 ms is a starting point; try it on more traces. |
| Q-26 | Should structural rows be hidden by default (TPL-1)? | Open. They are a third of the rows, but hiding rows by default surprises. Dimmed and shown is the starting point. |
| Q-27 | Does a focus scope the Data tab too (FOC-3)? | Decided: yes, with the source row numbers unchanged. |
| Q-28 | Show the path from the root to the focus, or only a bar with Up one level (FOC-4)? | Decided: the bar with Up one level and Show whole trace. A path can follow if it is missed. |
| Q-29 | Save the focus in the result file (FOC-6)? | Decided: no, it is view state. |
