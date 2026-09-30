# Sample result files

Test data for the Kusto Results Workbench ([spec](../kusto-results-workbench-feature-spec.md), [design](../kusto-results-workbench-design.md)). All files use the `.ktt` format written by the VS Code KustoTraceTools fork (JSON: `query`, `cluster`, `database`, `parameters`, `tables[]`, `tableViews[]`, and so on). Legacy `.kqr` files use the same format.

## Files

| File | Committed | What it is |
| --- | --- | --- |
| `synthetic-types.ktt` | yes | 3 tables (40 rows, 4 rows, 0 rows). Every Kusto column type, edge values, saved column layout. |
| `synthetic-types.expected.json` | yes | Expected sort orders and filter results for `synthetic-types.ktt`. |
| `synthetic-legacy.kqr` | yes | A 10-row subset of the types table saved under the legacy extension. |
| `synthetic-trace-edge.ktt` | yes | 525-row trace: hierarchy anomalies, severity outcomes, multipart messages, exception call stacks. |
| `synthetic-trace-edge.expected.json` | yes | Expected outcome for every case in `synthetic-trace-edge.ktt`. |
| `generate_samples.py` | yes | Writes all of the above (deterministic). |
| `generated/synthetic-large.ktt` | **no** (git-ignored) | Scale fixture: 200,000 rows by 20 columns, about 230 MB. Made with `python3 generate_samples.py --large`. `--rows N` changes the size. |
| `sample1.ktt`, `sample2.ktt` | **no** (git-ignored) | Real telemetry from a production cluster. Kept for local use only; never commit. |

Everything synthetic is invented: no real identifiers, hostnames or messages. Regenerate with:

```sh
python3 generate_samples.py            # small committed fixtures
python3 generate_samples.py --large    # plus generated/synthetic-large.ktt
```

The expected files record source row numbers **zero-based** (the grid shows them plus one).

## What each fixture exercises

### synthetic-types.ktt (table `PrimaryResult`, 40 rows)

| Area | Where | Spec |
| --- | --- | --- |
| All types: `long`, `bool`, `int`, `real`, `decimal`, `string`, `datetime`, `timespan`, `guid`, `dynamic` | 12 columns | GRD-4, SRT-5, FLT-4 |
| Nulls in every column | throughout | SRT-6, FLT-6 |
| 64-bit fidelity: `9007199254740993`, `9223372036854775807`, `-9223372036854775808` | `Big` | NFR-9 |
| `5` vs `5.0`, `1e-7`, `1.79e308`, `-0.0` | `Ratio` | NFR-9 |
| Decimal above 2^64 | `Money` | NFR-9 |
| Datetimes differing only in 100 ns ticks; year boundary; leap day | `Stamp` | SRT-5b, FLT-6 |
| Timespans: negative, day-prefixed, `10.00:00:00` (sorts wrongly as text), 1 tick | `Elapsed` | SRT-5c, FLT-6 |
| Natural order (`Step2`, `Step10`), case (`alpha`, `Zeta`), accents (`Éclair`, `eclair`), empty string vs null, repeated values (sort ties) | `Name` | SRT-5e, SRT-3, Q-12 |
| Search punctuation: `10:42:11`, `Retry 1/3 scheduled`, `café` | `Name` | SRC-2 |
| Dynamic values: object, array, key order (`{"b":1,"a":2}`, `{"10":..,"2":..}`), JSON held as text, duplicate keys as text, big number as text, scalar, `callStack` inside an object | `Payload` | JSN-1, NFR-9, JSN-5 |
| Copy escaping: tab, newline, CRLF, quotes, pipe, HTML, `<email@x.com>`, leading and trailing spaces, emoji, 5,000-character value | `Note` | CPY-2, CPY-4, GRD-3, GRD-4 |
| Three tables, one of them empty; badge total 44 | tables | PNL-1, PNL-2, PNL-4 |
| Saved layout: reordered columns, widths, gutter width 64 | `tableViews` | COL-4, PER-3 |
| Non-empty `parameters` | header | RUN-6 |

`synthetic-types.expected.json` gives, for `Small`, `Big`, `Ratio`, `Money`, `Stamp`, `Elapsed` and `Flag`, the row order for ascending and descending sort (null smallest, ties by source row), and matching rows for 15 filter cases plus one combined case. Five of the filter cases give a different answer in VS Code on purpose; the note on each says why (spec section 7, rows 4, 9 and 22).

### synthetic-trace-edge.ktt (table `PrimaryResult`, 525 rows)

Columns follow the real traces (`TIMESTAMP`, `MessageText`, `level`, `MarkerName`, `CustomData`, `ExecutionContext`, `CurrentActivityId`, `ParentActivityId`, `RootActivityId`, `ProcessName`), except the severity column is named `level` in lower case to exercise case-insensitive matching. Each activity's `MarkerName` is its scenario name, so the tree is self-describing.

| Area | Scenario names | Spec |
| --- | --- | --- |
| Two equally deep branches (`Deepest` cycles between them), depth 5 | `t1_*` | ACT-11 |
| Severity outcomes: final error, warning, critical; handled issue (30 %); worst earlier issue; verbose or normal final; no level; invalid levels `0`, `9`; child error under a normal parent | `t2_*` | ACT-9, ACT-10, SEV-1 |
| Orphan parent, conflicting parents, 2-cycle, 3-cycle, self-parent, parent named only on a later row, parent id with stray whitespace | `anom_*`, `cyc_*`, `cyc3_*`, `ws_*` | ACT-1..ACT-5 |
| Rows with null and empty `CurrentActivityId` (each its own root; a parent named on such a row is ignored) | `missing_*` | ACT-1 |
| High-volume parent (400 events) with three children that must appear under its first event | `hv_*` | ACT-4 |
| Multipart: complete, shuffled, incomplete, mismatched totals, duplicate part, spaced marker, `1/1`, ordinary row among parts, marker not at start, extra space after the colon, six-part plain text, two interleaved sets | `mp_*` | MPM-1..MPM-6 |
| Call stacks: framework noise, generated names, lambda frames, framework-only, every frame with a source location, a path with `\repos\node\` directories followed by an escaped `\n`, UNC path, POSIX path, nested and array exceptions, key casing, real newlines, callstack inside a dynamic column, JSON scalars, invalid JSON, plain text with an escaped newline | `cs_*` | EXC-1..EXC-8, JSN-1, JSN-5 |

`synthetic-trace-edge.expected.json` contains: every activity's parent, hierarchy issue, severity outcome (`level`, `strength` full or muted), warning triangle, event count, first source row, branch size and depth; the roots in order; the deepest activities; the untinted rows and the tinted counts per level; every multipart selection with its expected assembled text or a "not assembled" verdict; and every call stack with the expected formatted result.

### generated/synthetic-large.ktt

200,000 rows by 20 columns by default: every type, nulls (2 to 60 % per column), values up to 6,000 characters, 8 % JSON messages with call stacks, `dynamic` objects, 16,666 activities in 338 trees, tree depth up to 153. For the scale baseline in the spec (NFR-1): open, scroll, sort, filter, search, structured view, memory.

## How the expected results were checked

- **Activities (all 75):** compared with the VS Code fork's own activity projection, run on this file. Parent, hierarchy issue, severity level and strength, warning triangle, branch size, depth, event count, root order and the deepest activities all agree.
- **Multipart (14 cases):** compared with a copy of the VS Code assembly rule.
- **Call stacks (14 cases):** compared with the VS Code formatter. 12 agree. The two that differ are the two deliberate deviations: `C04` (VS Code chains every frame onto one line) and `C05` (VS Code corrupts the `\repos\node\` path). Each has a note in the expected file.
- **Filters (15 cases):** compared with the VS Code filter matcher. 5 differ, all for reasons documented in spec section 7.
- **Sorts:** computed from the spec rules (SRT-5, SRT-6, SRT-3), not from VS Code, because VS Code's sort is done inside its grid library and differs on purpose (timespan as text, tie order).

## What these files do not cover

- Column layouts saved from the structured grid (`<table>::activity-structured:<n>` view names).
- Charts (`charts`, `chartViews`), which are out of scope.
- Values a real server could send that are not representable here, such as `NaN` or infinity in a `real`.
