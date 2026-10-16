# Kusto editing spike

Install this directory as a Zed dev extension, then open `examples/highlighting.kql`.
The extension recognizes `.kql` files, and a local language server provides the colours, completion, hover,
diagnostics and the schema of your clusters.
It also launches a local language server for completion, hover information,
signature help, and syntax diagnostics. The server uses
`Microsoft.Azure.Kusto.Language` 12.4.0. It can load schema from your clusters
(see below) or from an offline snapshot.

Build and install the language server with the .NET 8 SDK:

```sh
./install-server.sh
```

Then run **Zed: Rebuild Dev Extension** in Zed. The language server binary is
installed in Zed's Kusto extension work directory. To use a different binary,
set `lsp.kusto-lsp.binary.path` in Zed settings. Restart the language server
after rebuilding it.

Completion includes Kusto keywords (including `declare`, which the library leaves out of the words a
statement may start with), built-in functions, and names declared in the current query. Hover shows Kusto's quick information for supported symbols.
Without a cluster's schema, diagnostics cover syntax only (see Diagnostics below).

## Where a query runs

A file can say where its queries run with comment lines like these. The space after the slashes is optional
(Zed adds `// ` when you press Enter in a comment, so `// :` is what you type; `//:` is what other Kusto extensions
write, and it works too):

```kql
// :setDefaultCluster("https://help.kusto.windows.net")
// :setDefaultDb("Samples")

StormEvents
| take 10
```

A directive applies to every query below it until a later directive of the same kind changes it, so one file can
move between clusters and databases. A directive in the same block as a query (the lines with no blank line
between them) applies to that query too. **Setting the cluster clears the database**, so a query after
`setDefaultCluster` needs a `setDefaultDb` of its own. A short cluster name such as `help` means
`help.kusto.windows.net`. A block of only comments, such as a directive on its own, is not a query: it gets no
lenses and F5 does not run it. A directive that does not parse gets a warning. Typing `// :` offers them, as soon as the colon is typed.
The same kind of line also gives values to query parameters, see [Values in the file](#values-in-the-file).

A query with no directive above it uses the defaults from Zed settings, which can be your global default or a
project's `.zed/settings.json`:

```json
{ "kusto": { "cluster": "https://help.kusto.windows.net", "database": "Samples" } }
```

The language server uses the same default for completion and the lenses. Zed writes the two settings to
`kusto/defaults.json` in its data folder, whenever they change, and the server follows that file, so there is one
place to set them and a change applies to open files at once. The server's own
`lsp.kusto-lsp.initialization_options` (`cluster` and `database`) still work for a server run without Zed, but the
file wins when it exists.

Each query shows where it will run in a lens, for example `help.kusto.windows.net / Samples`, and the last run
that lens row remembers is of that query on that cluster and database. Both the editor and the language server read
the directives with the same rule, and both are tested against `fork-docs/samples/connection-directives.json`.

## Colours

The colours come from the language server as LSP semantic tokens, which Zed uses on their own for Kusto
(`"semantic_tokens": "full"` for the language, set in Zed's default settings). Kusto's own analysis classifies
every part of a query, so everything is coloured: `declare query_parameters`, `print`, control commands such as
`.show`, every operator, functions, types, strings, numbers and comments. Once a schema has loaded, tables,
columns and functions get different colours, which a grammar that only reads the text cannot do, and the
colours are asked for again when a schema arrives or is refreshed.

| Kusto | Token type |
|---|---|
| keywords, query and scalar operators, commands | `keyword` |
| strings / numbers and timespans | `string` / `number` |
| comments | `comment` |
| types such as `string` | `type` |
| tables and materialized views | `class` |
| columns and schema members | `property` |
| functions | `function` |
| variables from `let` | `variable` |
| parameters | `parameter` |
| `==`, `+` and other symbols | `operator` |

The theme decides the colour of each type, with Zed's usual rules for them (see `semantic_token_rules` in Zed
settings to change one). Punctuation and a name the analysis cannot place keep the default text colour. The
colours exist once the language server has started, so a file shows plain text for a moment when it is first
opened, and without the server (for instance when it is not installed) there are no colours at all.

## Control commands

A query whose first line of code starts with a dot is a control command, such as `.show tables`,
`.show function MyFunction` or `.show database schema`, and runs with F5 like any other query: the same lenses,
the result in the Results panel, the history, Run again, and Save a copy… all work, and a command can be run
again from the history in the same way. Nothing here treats a command differently from a query except what is sent.

Commands go to Kusto's management endpoint, which answers in another format, so the result is read from that
format: the table the command returns is the result, and the properties and status tables the service adds are
left out. **Kusto rejects a command with a comment before the dot**, so the comment lines you start a command with
(`// what the functions are`) are not sent. They are kept with the command everywhere else, in the history, in the
saved result and in the Query tab, and still label the row in the history.

## Query parameters

A query takes values from a parameter profile when it declares what it needs with
`declare query_parameters(...)`:

```kql
declare query_parameters(raid:string);
ASEdog().ASTrace
| where RootActivityId == raid
```

The values come from YAML, in the profile marked active:

```yaml
active: Incident A
profiles:
  Incident A:
    raid: abc-123
    environment: prod
  Incident B:
    raid: def-456
    environment: prod
```

The file is `.kusto/parameters.yaml` in the project, shared by every query in it. A query file can have its own
profiles in a file beside it, `incident.parameters.yaml` for `incident.kql`, which is used instead of the
project's file when it exists. Values are sent to Kusto as query parameters, not pasted into the text of the query,
so a value cannot change what the query means, and only the parameters a query declares are sent. A value for a
declared parameter that has no default is needed: if the profile has none, Kusto says so when the query runs.
A profiles file that cannot be read stops the run with the reason.

Each query that declares parameters has a lens, `Params: Incident A`, that opens a picker of the profiles (a
click on it runs `kusto::SelectParameterProfile`); choosing one changes the `active:` line of the file and nothing
else. The lens says `Params: none` when no profile is active and `Params: Incident A (no value for raid)` when the
active profile lacks a declared parameter. These actions are also in the command palette:

- `kusto: select parameter profile`
- `kusto: open parameters` (the project's file, started from an example when it does not exist)
- `kusto: open query parameters` (the file beside the query, started from the profiles that apply)

Open a profiles file and each profile in it has a lens: **✓ Active** above the profile that is active and
**Make Active** above each of the others. Clicking Make Active changes the `active:` line in the open file
(only that line, so your cursor, comments and other unsaved edits stay) and saves it. The lens follows what is in
the editor, not what is on disk. To do this the language server is also attached to YAML files; it ignores every
YAML file except `.kusto/parameters.yaml` and `<name>.parameters.yaml`, for which it gives nothing but these
lenses, but it does mean opening any YAML file starts the language server. Zed's own YAML support is
unaffected.

The editor and the language server read the declarations and the files with the same rules, and agree on the declared names
through `fork-docs/samples/query-parameters.json`.

### Values in the file

A file can give values to parameters itself, with directives like the ones that say where a query runs:

```kql
// :setParam("raid", "abc-123")

declare query_parameters(raid:string);
ASEdog().ASTrace
| where RootActivityId == raid
```

`// :setParam("name", "value")` gives the parameter a value for every query below it, until a later `setParam` of the same
name gives another, `// :clearParam("name")` takes it away, or `// :clearParams()` takes away every value the file gave. A
directive in the same block as a query (no blank line between them) applies to that query too, and a selection takes the values of the
query it starts in. These values come before the active profile's, and only for the names they give: the profile still supplies
the other parameters, and clearing a name hands it back to the profile. Like the profile's they are sent as native query parameters, only
to a query that declares the name, and read as the declared type, so `"5"` is a `long`. An argument is a string in double or single
quotes, written as it is, with no escapes: a value with a quote in it uses the other kind of quote around it. A directive with
a name that cannot be a parameter, the wrong number of arguments or an unquoted value gets the same warning as any other directive
that does not parse. Typing `// :` offers every directive.

Inside `declare query_parameters(`, where a name is written, completion offers the parameters you have values for: first
those the file gives with `// :setParam` above the query, then those in the active profile, then those only another
profile has (each says which in its detail). A name the declaration already has is not offered again, and a typed
start (`co`) is replaced whole, also when the cursor is inside a name. Only the name is inserted; the type is yours to
write. Nothing is offered after a colon, in a default value or outside the parentheses, and when there is nothing to
offer the usual `:` and `[` appear.

The lens says where each declared parameter's value comes from: `Params: Incident A + file: raid` when the file gives `raid` and the
active profile gives the rest, `Params: file: raid` with no active profile, and as before `(no value for count)` for a declared
parameter with neither a value nor a default. The language server and the editor read the directives with the same rule, and agree
through `fork-docs/samples/parameter-directives.json`.

## Functions in the file

A block (lines with no blank line between them) that has the line `// :functions` defines functions and values for the
queries below it:

```kql
// :functions
let Window = 7d;
let Normalize = (value:string) { tolower(trim(" ", value)) };
let Recent = (count:long) { Events | where At > ago(Window) | top count by At desc };

Events
| where At > ago(Window)
| extend Key = Normalize(Name)
```

The block is not a query: it has no lenses and F5 does not run it. It is Kusto code all the same, so it is coloured, completes,
and a mistake in a definition is reported where it is written. Every `let name = …;` statement is a definition: a function
(`(parameters) { body }`, and `view` before the parameters is fine), a scalar such as `5` or `ago(7d)`, or a table such as
`Events | take 3`. Anything else in the block is ignored. A definition can use the ones defined above it, tables and
functions of the database, and the parameters the query declares. A value is used by its name, without parentheses, as
`let` does: completion offers it that way, and `Window()` is reported as an error, because the service would reject it.

A definition applies to every query below its block, until a later definition of the same name replaces it,
`// :clearFunction("name")` takes that one away, or `// :clearFunctions()` takes away every definition the file made.
Several blocks add up, and the clearing directives work like `// :clearParam` and `// :clearParams`.

While you edit, the functions a query can use are known to completion, hover, signature help, the colours and the check of
names, as if they were stored in the query's database, and a call is not reported as an unknown function. Nothing is added
to the text you see.

When a query is run or copied, the editor adds a `let` for each definition the query uses, and each definition those use, and
no others, because a definition written for another database could otherwise fail a query that never uses it. A name is
used when it appears as a word outside comments and strings, so a column with the same name costs an unused `let`, which
only matters when its body needs something this query's database lacks. They go after the query's `declare query_parameters`
and `set` statements, so a definition can use a parameter, and a name that the query defines itself with `let` is the
query's own and is not added. A control command gets none.

The service is sent the query with the definitions, and the run log, the history list and the lenses keep the query as you wrote
it. **Copy** and the **Query** tab of a result show the added `let`s (after the parameter values, and with the names
qualified as for any query), so the copy runs anywhere. A history Rerun sends the same functions the run had, also when the file has
changed since: the run log keeps them.

## History

Every run is saved as a result file in the `kusto/history` folder of Zed's data folder. **kusto: show history**
(in the command palette) lists the queries that were run, newest first, with the time, the number of rows, how
long it took, where it ran and the query's first line of code, after any comment lines the query starts with
(`// incident 123 — second try`), which makes a good label. Directive comments (`// :setDefaultDb(...)`) are not shown. Type to search the query text, the cluster, the
database or an error message. Enter shows that result in the Results panel, and Cmd-Enter on macOS (Ctrl-Enter elsewhere)
opens it in a tab. A failed run shows its error. The trash button that appears when you hover a row
deletes that result's file; the row stays in the list, marked `result moved or deleted`.

The play button that appears when you hover a row runs that query again: the same text, on the same cluster
and database, with the same values for its parameters as the original run. It does not use today's active
parameter profile, so a rerun repeats the run even if you have switched profiles since. The values are kept
in the run log (`runs.jsonl`) next to the query text. A rerun is a run like any other: it appears in the history,
can be cancelled, and shows its result in the Results panel.

The list comes from the run log, `runs.jsonl`, which keeps the last 200 or so runs, so older results are still
on disk but no longer listed.

The history cannot grow without limit. After a run, and when Zed starts, the oldest result files beyond these
settings are deleted (a limit of 0 means no limit):

```json
{ "kusto": { "history_max_results": 50, "history_max_megabytes": 1024 } }
```

Only files named like a run's result (`20261001-203917-<uuid>.ktt`) are ever deleted, so a result you saved into
that folder yourself is safe, and so is any result open in a tab or in the panel of the window that ran the query.

### Keeping a result

Use **Save a copy…** (at the right of the tabs of a result, or `kusto: save result` in the command palette; the
history has a download button on each row). It opens a save prompt in the folder of the file you were working on,
with a name made from the query and the time it ran, such as `incident-123-t-where-id-raid-2026-10-01-1030.ktt`
(the query's leading comments and then its first line of code, lower case, with the time in your time zone), which
you can change before saving. A toast then offers to open the saved file.

A result is a normal `.ktt` file, so you can also move or copy its file out of the history folder (the
`kusto/history` folder in Zed's data folder) yourself. Pruning only looks at that folder, so a saved file is
yours to keep. A kept file opens in a tab like any other result, and the file carries what it needs to be
understood and repeated, so it needs nothing from the history:

- The **Query** tab (next to Data) shows the query the result came from, read-only and selectable, with where and when it ran and the values its parameters had.
- **Run again** (at the right of the tabs) runs that query again on the cluster and database the file names, with
  the parameter values the file holds, and puts the new data **in the same file**. The charts you made, the saved
  column layout of a table that still has the same columns, and the query, cluster, database and parameter values
  stay as they are; the rows, the run time and the duration are the new run's. The tab shows the new data as soon as
  the file is written. If the run fails or is cancelled, or the file cannot be written, the file is not touched and
  you are told why. A file is refreshed by one run at a time, and a refresh is not a run of the history: it is not
  listed there and no lens shows it.

Both are shown for any result file that names its query, and Run again also needs the file to name a cluster and
database. Files written before parameters were kept in them simply run without values.

A result that is still in the history folder is a record of one run, so Run again there makes a new result (shown
in the Results panel, or a tab) and leaves the old one alone. To keep a result you can refresh, use **Save a copy…**
first and open the copy.

## Results with several tables

A query that ends in more than one statement that returns rows, or a command that answers with more than one table,
gives a result with several tables. The first is the primary result and is shown in the **Data** tab; each of the
others has a tab of its own right after it, **Data 2**, **Data 3** and so on, which shows its name and row count
when you hover it. These grids sort, filter, search and keep their column widths like the first one, and Row Details
follows a selection in any of them.

The structured, sequence and timeline views are built from the primary result only, and so is **Focus on this row's
activity**, which the other tables do not offer.

## Charts

A result can hold charts. Each one is a tab after the data tabs, in the Results panel and in a `.ktt` tab alike, and a
result that has a chart opens on it (set `kusto_results.chart.open_on_chart` to `false` to open on the data).

**Where a chart comes from.** A query that ends in `render` gives its result a chart of that kind, saved with the result:
`timechart`, `linechart`, `barchart` and `columnchart`, with `kind=stacked` or `stacked100` for the stacked forms, and
the `with (title=…, xtitle=…, ytitle=…, xcolumn=…, ycolumns=…, series=…, xmin=…, xmax=…, ymin=…, ymax=…, legend=hidden, xaxis=log, yaxis=log, accumulate=true)`
options (`ysplit` is read and saved, but not drawn yet). Put `kind=` inside the `with (…)`: the service ignores the older `render columnchart kind=stacked` when a
`with (…)` follows it. A result with no `render` gets a chart from **Add chart** in the tab bar (a plus once there is one),
or the `kusto: add chart` command. It starts from the type the columns suggest, a time chart for a datetime and a number
and columns for a name and a number, and adds the chart for the table of the tab you are on.

**Types.** Line, Time line, Bar (horizontal), Column, and the stacked and 100% stacked forms of bars and columns. A chart
of another type that a result file holds (area, pie, scatter, graph, …) is kept exactly as it is, and its tab says it cannot
be drawn here.

**How it looks.** A chart has light gridlines across the axis of its values, one axis line along the base the data stands on or runs along (the bottom, or the left side of a horizontal bar chart), and tick marks under the labels of a time or number axis. Nothing is drawn up the other side. The **X gridlines**, **Y gridlines** and tick choices of the pane, and the `gridlines` setting, change that. A line chart keeps its first and last points a few pixels in from the ends of its axis so the marker you hover is whole, and a line past a limit you set on the value axis is cut at the edge of the plot.

**Reading a chart.** The X column is the first datetime column for a time chart, otherwise the first column; the values
are the numeric columns; and the rows are split into series by the first text column. The options of a chart can name any of
them, and the pane below changes them. Times are in UTC. Move the pointer over a line chart and a tooltip with a crosshair
lists every series at the nearest X; over a bar it names the bar. Click an entry of the legend to switch that series off and
on; the axes fit what is left. When several rows of a series have the same X (a trace with a row for each of several sub-activities at one time, say) each is a point, so a line goes up and down at that X, and a note under the chart says how many rows do. **Rows at the same X** in the pane combines them into one point: the sum, average, smallest or largest value, or the number of rows. A series with more points than the chart has pixels keeps the extremes of each pixel column,
so a spike is not lost.

**Editing.** **Edit chart** opens a pane beside the chart: the type, the table, the X column, the value and series columns,
the sort, running totals, how rows at the same X are combined, the titles, the scale (linear or log) and the **From** and **To**
of each axis, the legend, values on bars, gridlines, aspect ratio and text size, and a name for the tab. X is the axis along the
bottom of the chart and Y the one up its side, for a horizontal bar chart too. A change draws the chart at once. Only the
options shown there can be changed; any other option a chart has is kept as it is. **Remove chart** is at the bottom of the pane.

From and To can be set one at a time: the end you leave empty fits the data. On the X axis of a time or number chart they cut the
axis, and on bars over dates or numbers they leave out the bars outside the range; a text axis has no range, so they do nothing
there. A time, for a limit or a line, is a date like `2026-08-29`, or a time like `2026-08-29 12:30` or `2026-08-29T12:30:00Z`, or a
`datetime(2026-08-29)` pasted from a query; without a zone it is UTC.

The pane works from the keyboard: opening it puts the cursor in its first field, **Tab** and **Shift-Tab** move through every field
in the order they are on the screen (also from inside a text field), **Space** or **Enter** changes the field you are on, and
**Escape** closes the pane and returns to the chart.

**Lines.** A chart can have lines across it at values you choose, such as a threshold or the moment of a release. A
horizontal line is at a value of the Y axis, and a vertical line at a value of the X axis (a date or a time on a time axis, as above, or a
category's name on a text axis). **Right-click** the chart and choose *Add horizontal line at …* or *Add vertical line at …* to put
one where the pointer is, or use **Add horizontal line** and **Add vertical line** under *Lines* in the pane, which put the cursor in
the new line's value. Each line can have a label, be dashed (the default) or solid, and be grey, red, orange, green or blue; its
colour follows the theme. Lines are drawn under the data, with their label at the end of the line. A line whose value cannot be read as a value of its axis is not drawn, and a note under the chart says which and what a value looks
like. A line is always in view: an
axis grows to include it, unless you have set From and To on that axis, and then a line outside them is cut off. They are saved
in the chart as `referenceLines`, which the VS Code extension does not know, so a file it saves drops them.

**Saving.** Every change is written to the result's file a moment after you stop, as a column layout is, and only the
`charts` of the file change. In the Results panel that file is the run's history file, so a chart you edit there is kept
until the history prunes the run; **Save a copy…** keeps it longer. A file that cannot be written keeps your changes
while it is open. Charts do not carry over to a run of the query again: the new result gets the charts its own `render`
asks for.

**Copy as a picture.** The copy button of the chart (or `kusto: copy chart`) puts a PNG at twice the size on screen on
the clipboard, in the colours of the theme, or light or dark if the chart's `mode` says so.

**Settings.** `kusto_results.chart` sets how a chart looks where it does not say itself: `legend`, `show_values`,
`gridlines` (`values`, `both` or `none`), `text_size`, `aspect_ratio` and `open_on_chart`. What a chart saves wins, and a default is never written
into a file.

**Not built yet.** Zooming into a range, panels or a second axis for several value columns (`ysplit`), binning in the
chart (use `bin()` in the query; combining rows at the same X is built), a local-time axis, saving a picture to a file, and tilted axis labels (a label that does
not fit is dropped instead), and moving a line by dragging it. Area, pie, scatter and the other types are not drawn. The design and the requirement list
are in `fork-docs/kusto-charts-design.md`.

## Trace columns

The structured, sequence and timeline views, the severity tints and the code behind a row all read a trace by its
parts: the activity id, the parent activity id, the root activity id, the marker, the actor (the process), the
timestamp, the severity and the message. A part is looked for under a built-in column name, exactly and then ignoring
case: `CurrentActivityId`, `ParentActivityId`, `RootActivityId`, `MarkerName`, `ProcessName`, `TIMESTAMP`, `level` then
`severity`, and `MessageText`. The root activity id is optional. When the table has one, an activity whose parent is not
in the result is placed under the root that carries that parent's id as its root activity id (the first one, when
several do), so a request that was handed on from another one shows as part of it. It stays marked as having no parent
in the result, and its tooltip in the Structured tab says where it was placed.

A trace that calls a column something else is covered by listing the names in `kusto_results.trace_columns`, most
preferred first:

```json
"kusto_results": {
  "trace_columns": {
    "message":  ["MessageText", "EventText"],
    "marker":   ["MarkerName", "ActivityType"],
    "severity": ["Severity", "Level"]
  }
}
```

The listed names are tried in order before the built-in ones, and the first that is a column of the table is the one
used, so a single setting serves tables that name the same part differently (there is no need to list a built-in name
unless it should come after another). The parts are `activity_id`, `parent_activity_id`, `root_activity_id`, `marker`, `actor`,
`timestamp`, `severity` and `message`. For a trace whose columns only make sense together, `kusto_results.trace_schemas` names one
column per part and the columns that must be present; the first schema that applies decides the parts it names, and the
other parts use the listed and the built-in names. A change applies to results opened, and rows selected, afterwards.

## Zooming the sequence diagram

The **Sequence** tab zooms with Ctrl (Cmd on macOS) and the mouse wheel, or with a pinch on a trackpad, over the
diagram. It goes from a tenth of the natural size, which shows the shape of a long trace, to twice it. A zoom close to
the natural size snaps back to it, and the **Zoom** label that appears at the top right of the diagram has a button that
resets it. The diagram scrolls in both directions at any zoom.

## Row Details

Row Details shows the values of the row selected in a result. It opens by itself, in the dock on the right (the
`kusto_results.dock` setting moves it to the left), whenever a result is shown: after a run that shows its result in the
Results panel, and when a result opens as a tab, which is what happens to a `.ktt` or `.kqr` file you open, to a result
you open from the history, and to a run when `kusto.results_location` is `editor`. It does not take the focus. A file
that holds no result and a failed run leave it as it is. If you close it, it stays closed until the next result is
shown, and moving a tab to another pane does not bring it back.

### The code behind a row

Set `kusto_results.code.roots` to the folders that hold your git checkouts, and Row Details lists the code that the
selected row points at (the setting is empty by default, which leaves this off; Row Details then says how to turn it on):

```json
"kusto_results": { "code": { "roots": ["~/Projects/mwc"] } }
```

A root that is a checkout is searched itself, and a root that is not is searched for checkouts one level below it. A
leading `~` is your home folder. The section shows, best first:

- **Marker used in** and **Marker defined as**: the method that uses the row's code marker, and where the marker is
  declared. A marker can be a class marked `[CodeMarkerName("…")]`, a `const string`, or an attribute that names a
  constant; all three are followed.
- **Message logged at**: the line of code that logged the message, found by searching for the text the message is
  made of. What a program fills in is left out: GUIDs, numbers, quoted values; and because code often logs only part of
  what the message shows, every stretch of its words is tried, including ones that end at a `:` or `=` where a value
  follows. So `Successfully fetched cluster details for [MetadataLocator: [TenantId=…` is found by
  `Successfully fetched cluster details for`, and `Memory backed cloud cache already initialized. Taking fast path.` by
  the whole of it. The text has to be in a string on a line of code (comments are skipped), and the most of it that any line
  holds wins. A line whose string goes on with text the message does not have, such as
  `$"{name} => workspace ID: {id} cloned execution context"` for a message that has `=> workspace ID:` but not
  `cloned execution context`, did not log it and is left out; what the string fills in (`{…}`, `%s`, escapes) may be
  anything. When several lines hold the same text it says **Message text in one of N places** and shows the best three
  (a repository named like the process first, tests last); when more than sixteen do, it is too common to say anything.
  Rows that only say something started or ended (`kusto_results.structural_messages`) are not searched.
- **Message names**: a `Class.Method` at the start of the message, in square brackets, or anywhere in it (a lambda like
  `Store.<Save>b__3_0` reads as `Store.Save`), found as a method, or as a class if the method is not there. A method the
  class inherits is found in its base class.
- **Stack frame**: the first application frames of a call stack in the row, such as the `callStack` of an exception.

**Call stacks.** In a call stack, whether it is a `callStack` property or a stack in the text of a message, a frame that
names a source file, such as `at Contoso.Orders.Checkout.Charge() in Checkout.cs:line 17`, has the file and line
underlined (a stack that kept its build path, `in F:\\1\\s\\src\\Checkout.cs :line 17`, is a link too). Clicking it opens that
file in the checkouts under `kusto_results.code.roots`, at that line (a plain click; a drag still selects text). The stack
gives the file as the build machine knew it, so what is looked for is the file name, ignoring case and the build
folders, among the files git tracks in the checkouts. Of several files with that name, the one that declares the frame's
class comes first, then one in a repository named like the process, then the one whose namespace is nearest the frame's.
If the checkout has moved on from the build and the line is not inside the frame's method (a local function or a lambda
inside it counts as inside), the method's declaration is opened instead. A file that is in none of the checkouts is reported. Frames without a file name, which is most of them in
a release build, are not links; the top application frames of the stack are in the list above, found by method name.

Each entry gives the method or class, its repository and line, and the text of that line; clicking it opens the file at
that line.

**Where code opens.** A click on a place in the code, whether the file of a call stack frame or an entry of the list, opens
it in a pane beside the trace. That pane is found or made the first time, and every later click uses it whichever pane
has the focus, so the trace tab is not covered by the code it points to. If you have already split the editor, the pane
beside the trace is used; a pane you close is made again by the next click. The code opens as a preview tab (the next
place replaces it, so going down a stack does not leave a tab for every frame) and takes the focus. With a modifier key:
**alt** opens in a new split beside the code pane, which is the code pane from then on, and **command** (control on Linux
and Windows) opens in the pane that was last active, which is where Zed opens any file. Shift does nothing, so it can
still extend a selection. Nothing is indexed: the lookup is a `git grep` made when you select a row (about half a second, or a second
when the text of the message is in no repository named like the process, and the
markers of a whole result are looked up together the first time you select in it), and what it finds is kept for five
minutes. Only `.cs` files are searched for text and names (a call stack's file can be of any kind), and `git` has to be on the path of your shell. When a name is in none of the
checkouts, the section says nothing was found; for example a class from a library you have not cloned.

## Query threads

A query thread keeps the queries of one investigation together without a file to name or a long file of
unrelated queries. It is an entry in the Threads sidebar, beside agent threads and terminals, and the agent panel
shows its query editor. Make one from the agent panel's new thread menu (**Kusto query**) or with `agent: new query thread`
in the command palette; it is offered for local projects.

The editor is an ordinary Kusto editor, with colours, completion, diagnostics and lenses, and holds as many queries as
you write, separated by blank lines. F5 or Shift-Enter runs the query at the cursor, or the selection, and the result
opens as a tab in the editor area, whatever `kusto.results_location` says, so a large trace gets the whole window.
Each run opens its own tab, and the history, rerun and the lenses see it like any other run. The lenses above
a query work as in a file, and **Results** shows the result of a past run in a tab.

The queries are a file, `<id>.kql` in the `kusto/threads` folder of Zed's data folder, saved half a second after you
stop typing. A thread finds its parameter profiles beside that file (`<id>.parameters.yaml`) and in the project's
`.kusto/parameters.yaml`. The default title is `Kusto query`; **Rename Title** in the right-click menu of its row changes it.
Closing a thread (the button on its row, or the archive key) forgets it and leaves its file, so a query worth keeping
is one you copy into a `.kql` file in the project.

Not yet: a thread is not restored as the shown entry when Zed starts (click its row), only the threads of projects
that are open are listed, the ctrl-tab switcher does not include threads, and a thread is renamed from the sidebar
only. The full list is in section 5.10 of `fork-docs/kusto-results-workbench-feature-spec.md`.

## Schema from your clusters

The server signs in with `az account get-access-token`, so run `az login`
first. It fetches the schema in the background the first time a cluster or
database is used by a query, so the first completion after opening a file may not
list tables yet; ask again a moment later. Tables, external tables,
materialized views, functions and entity groups are loaded, including each function's
parameters and doc string. A query that names another cluster, such as
`cluster('other').database('Logs').Requests`, loads that cluster and database
the same way. A short name such as `help` means `help.kusto.windows.net`; other
clouds need the full host name.

### Entity groups and `macro-expand`

The entity groups stored in a database are offered after `macro-expand`, and
`macro-expand AllRegions as Region (Region.Requests | where Dur…)` completes and is checked like a query on
one database: `Region.` offers the tables of the databases in the group, and the columns after it are theirs.
The clusters and databases a group names are loaded like ones a query names, as soon as the group's own
database has arrived, and a query over a group is checked once they have all arrived, so a name is not called
wrong while they are still on their way. A group written in the query itself, as `macro-expand entity_group [...] as X (...)` or
`let G = entity_group [...]`, needs nothing stored.

The listing of a database's entities gives the definition of a stored group. If it does not, the server asks
for `.show entity_groups`, which needs more than the permission to read the schema. When the cluster refuses
that, the group's name is known but its tables are not, and nothing under it is called wrong.

### Schema cache

Each cluster's database list and each database's schema is also kept on disk, in `kusto/schema/<cluster>/` in
Zed's data folder (the folder that holds the run log), so completion works as soon as the server starts and
without the network. A cached schema is used at once, however old it is. If it is more than an hour old the
cluster is asked too, in the background, and the cluster's copy replaces it when it arrives; if the cluster cannot
be reached the cached copy stays, and the fetch is tried again after a minute. A schema with no cached copy is
fetched as before. Set the `schemaCacheMinutes` initialization option to change the hour (`0` asks the cluster
every time the server starts). Click the schema lens to fetch a schema again now, or delete the `kusto/schema` folder to throw the cache away; an entry that cannot be
read, or was written by another version, is ignored and replaced. Names in file paths are escaped, so no
cluster or database name can write outside the folder. Files are written whole and then moved into place, so
several Zed windows can share the cache. The token's audience is also asked for once per cluster instead of on
every request.

#### Refreshing the schema

Each query has a lens, `↻ Schema: 3 h ago`, after the one that says where it runs: it says how old the schema
the query is checked against is (`just now`, `12 min ago`, `3 h ago`, `2 d ago`), `loading…` while it is first
fetched, `refreshing…` while it is fetched again, and `not loaded` when it could not be fetched and nothing is
cached. **Click it to fetch the schema of that query's cluster and database again, whatever the cache says.** The
fetch happens in the background and a message in the editor says how it went (`Refreshed the schema of
help.kusto.windows.net / Samples: 120 tables, 33 functions.`, or why it could not). Completion uses the new
schema as soon as it arrives. If the cluster cannot be reached the schema you had stays, and the lens keeps
showing its age.

Signature help appears inside the parentheses of a function call, including
database functions with their parameter names and types. It finds unqualified
function names only.

A file holds several queries separated by blank lines, and each is analysed on its own: a query
never continues into the next one, and a `let` in one is not visible in the next.

A blank line inside parentheses or braces that are still open does not end the query, so a function with a gap
in its body, or a call written over several lines with a gap in it, stays one query with one set of lenses above its
first line. Brackets in comments and strings do not count, and a multi-line string (```` ``` ```` or `~~~`) holds a
blank line too. Brackets that are never closed do not hold the queries below them: the query ends at its first blank
line, as it does for a query with no brackets. The editor and the server read the file the same way
(`fork-docs/samples/query-blocks.json` is tested on both).

#### Diagnostics

Syntax errors are always shown. Once the schema of the database a query runs on has arrived (from the cache or
the cluster), **names that are not in it are errors too**: an unknown table, column, function or variable
(`The name 'Nope' does not refer to any known table, tabular variable or function.`), and the other mistakes
Kusto's own analysis finds, such as a wrong argument type. Some rules keep this honest:

- A query is checked only when the schema of everything it refers to has loaded: the database it runs on, and
  any other cluster or database it names with `cluster(...)` or `database(...)`, or through an entity group it runs
  `macro-expand` over. While a schema is still arriving,
  or when the cluster cannot be reached and nothing is cached, a query gets syntax errors only, so a name is never
  called wrong just because its schema is late. A cached schema counts, so a stale one can flag a table that
  is new: click the `↻ Schema` lens to fetch it again, and the file is checked again when it arrives.
- Each query is checked against the cluster and database its own directives name, so queries on different
  clusters in one file are each checked against their own.
- When the table or function a query starts from is not in the schema, that is the one error shown for the
  query. The analysis cannot say what the columns of an unknown source are, so it would otherwise also report
  every column the rest of the query uses and every argument whose type depends on one; those are left out.
  A wrong name in a different statement, or in a query whose source is known, is still reported. (Names after an
  unknown source also stay in the default text colour, since nothing can say what they are.)
- Control commands (`.show ...`) are only checked for syntax.
- The files are checked again whenever a schema arrives or is refreshed, with no keystroke needed.
- If a name the service accepts is reported wrong, for example a function newer than the analysis library
  the server uses, set the `schemaDiagnostics` initialization option to `false` in `lsp.kusto-lsp` to go back to
  syntax errors only.


## Code lenses

Turn code lenses on in Zed settings (they are off by default):

```json
{ "code_lens": "on" }
```

Above each query (a run of non-blank lines, or more where a bracket stays open across a blank line) the lenses are on two lines. The first line is what to do. It starts with **▶ Run**, which runs that query. While the
query is running it shows **⠹ Running… 12 s** (a spinner and the elapsed time, refreshed four times a second
until nothing is running) and **Cancel** instead. Once the query has been run and left a result, **Results**
comes straight after them, with the number of rows the result has, for example `Results (1,240 rows)` (it shows that
run's saved result in the Results panel, or in a tab in a query thread), then **Params** for a query that declares parameters, then what the last run did, for example
`Last run: 10:42:11, took 1.8 s`. A run that failed shows `Last run failed: <the first line of the message>`. The last run is
matched by the text of the query without its comments and layout, so reformatting a query keeps its lens.
The second line starts with **Copy**, which copies the query (see below), then says where the query runs: the connection, the schema lens, **Snapshot functions** (see [Functions as files](#functions-as-files)), and **Copy CID** (copies the last run's client request id).

### Copying a query

**Copy** puts the query on the clipboard written so that it means the same wherever it is pasted, in another tool or in a message:

- A table, function, materialized view or external table of the query's own database is written with its cluster and database,
  `cluster('help.kusto.windows.net').database('Samples').StormEvents`, and so is a function that returns a database, as in
  `ASAz().ASTrace`, which becomes `cluster('…').database('…').ASAz().ASTrace`. Names that are already qualified, columns, `let`
  variables and what comes after a dot are left as they are. The language server decides this with the schema, so it needs the schema
  of the database to have loaded (the `↻ Schema` lens says when).
- When the server is not running, has no schema for the database yet, or does not answer within five seconds, the query is not
  changed except for two lines above it, `// :setDefaultCluster("…")` and `// :setDefaultDb("…")`, the directives a Kusto editor here
  reads, so the cluster and database are known.
- A `declare query_parameters(...)` statement becomes a `let` for each parameter, with the value the run would use: the one the file gives with `// :setParam`, else the active profile's, else the declared default
  (`let raid = 'abc-123';`, `let count = long(5);`). A parameter with neither stays declared. Values are written as literals of the
  declared type, and a string is quoted, so a value cannot change what the query does.
- The functions the query uses that the file defines with `// :functions` are written as `let` statements, after the parameters' (see [Functions in the file](#functions-in-the-file)).
- A control command (`.show tables`) is copied as it is.

The clipboard holds plain text, without colours, because Zed's clipboard has no rich text.

The same text is the **Query** a run keeps in its `.ktt` file and shows in the Query tab. The service is still sent the query as you
wrote it, with its parameters as native query parameters, and the run log, the history list and the Rerun in the history keep the text you wrote.
Rerun in a result tab runs the text the file keeps.

The lenses come from a log the editor keeps: every run appends a line when it starts and when it ends
to `kusto/history/runs.jsonl` in Zed's data folder, and the server watches that file and asks Zed to
refresh the lenses when it changes. The extension tells the server where the data folder is through
`KUSTO_ZED_DATA_DIR`, working it out from the extension's own working directory. If you run the
server some other way, set `dataDir` in `initialization_options` instead. A run that never reports an
end, such as when Zed was closed while it ran, stops counting as running after 15 minutes. The log keeps
about the last 200 runs.

A lens acts through `zed.dispatchAction`, a lens command this fork of Zed handles in the editor: its
arguments are the name of a Zed action (`kusto::RunQuery`, `kusto::CancelQuery`, `kusto::CopyQuery`, `kusto::ShowResult`,
`kusto::CopyClientRequestId`, `kusto::SelectParameterProfile`) and, optionally, the data the action takes. Zed moves the cursor to the lens
before it runs the action, so `RunQuery` and `CancelQuery` act on that query.

A lens is on the first line unless its `data` says `{"zedLine": 1}` (a number for each further line), which this fork of Zed reads to
put it on a line below. The lenses of one line are joined with ` | `, so the server decides how many lines there are and what goes on each.

After updating, run `./install-server.sh`, then **Zed: Rebuild Dev Extension**, then restart Zed.

## Saving a query as a snippet

Right-click in a Kusto file and choose **Save Query as Snippet**, or run `kusto: save query as snippet` from the command palette.
It saves the selected text, or else the query around the cursor, as a snippet of the Kusto language. Zed asks for a prefix, and
typing that prefix in a Kusto file afterwards offers the query in the completion list. The command has no key binding by default.

- The query is saved as written, comments included. Nothing is qualified or filled in as **Copy** does it. `$` and `\` are escaped,
  so `$left.Id` in a join is inserted as typed and not read as a tab stop.
- The snippet goes into `kusto.json` in Zed's snippets folder (`~/.config/zed/snippets`, which `snippets: configure snippets` also
  opens). The file is created when there is none. A file you have written is not rewritten: the new snippet is added after the last
  one, and the comments and the other snippets stay where they are. A file that is not valid JSON is left alone, and the prompt says so.
- The prefix is also the name of the snippet. A name the file already has is refused, and the prompt stays open for another.
- The notice after saving has an **Open** button for `kusto.json`, to give the snippet tab stops (`$1`, `${1:StormEvents}`) or a description.

## Functions as files

The functions stored in a database can be kept as files in your project, one `.create-or-alter function` command
each, so that git shows what changed on the cluster and there is a file to go to from a call. They are not
the offline schema below, which is for completion; these are for reading and deploying functions.

**Go to definition** (F12 in Zed) on the name of a function the database stores opens its file. If the project has no
files for that database yet, the functions of the whole database are read from the cluster (`.show functions`) and
written first, and a message in the editor says where. After that going to a definition never reaches the cluster and
never replaces a file: what you changed in it, or committed, stays. A function a file defines itself with
`// :functions`, and the language's own functions, have no definition to go to.

**Snapshot functions**, the lens on the second line of a query, reads the functions of that query's database again and
writes every file whatever it holds, so that `git diff` shows what the cluster has that your files do not. The message
says how many files are new, changed and as they were, and how many files have no function on the cluster any more
(those are reported and left in place). A file is written only when its text differs, and it is written the same way
every time, so a difference is a difference in the function.

A file looks like this, in `<folder>/<cluster>/<database>/functions/<the function's folder>/<name>.kql`
(a function's folder, which Kusto takes as free text, becomes folders, whichever slash it uses):

```kql
// :setDefaultCluster("help.kusto.windows.net")
// :setDefaultDb("Samples")
.create-or-alter function with (docstring = 'States with more than a number of events.', folder = 'Utilities\\Storm')
StatesOver(minimum:long, region:string)
{ StormEvents | summarize n = count() by State | where n > minimum }
```

The two directives at the top mean that running the file with F5 deploys the function to the database it was read
from, not to the window's defaults. A body with blank lines in it stays one command (a blank line inside braces does not end a
query), so a file runs as it is.

**Where the files go** is the `snapshotDirectory` initialization option of the language server, in Zed settings:

```json
{ "lsp": { "kusto-lsp": { "initialization_options": { "snapshotDirectory": "docs/kql" } } } }
```

- A **full path** (`/Users/me/kql-snapshots`, or `~/kql-snapshots`) is used as it is, with or without a project open.
- A **relative path** (`docs/kql`) is relative to the project that holds the query, and so is the default when the
  option is not set: `kusto-schema`. A file outside every project folder, such as a query thread's, uses the project's
  first folder.
- With **no project open** and no full path, nothing is written: a message says there is no project and asks for a
  full path. The server cannot tell a project from a single file opened on its own, because Zed gives it the file's
  folder as the project, so a lone file gets its own folder as the project. Set a full path if you open files that way.

The option is read when the server starts, like the others: restart the language server after changing it. A project's
`.zed/settings.json` can set it for that project.

**Cmd+T** (project symbols) finds, by name, two kinds of things in the open projects, best match first (the name
itself, then names that start with what you typed, then names that contain it, then names that have its letters in
that order):

- **The functions that have a file under the snapshot folder**, with the database and the function's folder beside the
  name. It reads the folder and not the cluster, so it lists only functions that already have a file: go to a
  definition, or click **Snapshot functions**, to make the files.
- **The queries and the `// :functions` definitions of every `.kql` file in the project**, named as in
  [the outline](#outline) (a query by the comment above it, or its first line), with the file's path beside the name. A
  file that is open is read from the editor, so what you have not saved is found too, and so is a file that is open
  from outside the project, such as a query thread's. A file is read again only when its time of modification or its
  size has changed. Folders that start with a dot, `bin`, `obj`, `node_modules` and `target`, and the snapshot folder
  (those functions are listed above), are left out, and a project is looked at up to 5,000 files and 20,000 folders.
  The list of files is kept for 5 seconds, so a file you made a moment ago may not be found yet.

Zed filters what the server returns by the name alone, so a query is found by its label or first line and not by
what is further down in it: for that, search the project (Cmd+Shift+F). Zed asks only the language servers of visible
projects, so Cmd+T does nothing for a file opened on its own.

## Outline

**Cmd+Shift+O** (the outline), the breadcrumbs and the outline panel list what a file holds, in the order written, from
the language server (the language setting `"document_symbols": "on"`, which Zed's default settings have for Kusto):

- **A query**, called by the first comment above its code, or by its first line of code when there is none. A directive,
  a line of dashes and an empty comment are not names, and a comment after the code has started (a line commented out of
  the query) is not either. A long first line is cut at 100 characters.
- **A definition in a `// :functions` block**, each function or value with its own entry.
- **A command that defines a function** (`.create-or-alter function`, so every file of the snapshot), called by the
  function's name.

A block of only comments or directives has no entry.


## Offline schema

For offline table and column completion, put a `.kusto-schema.json` file in the
Zed project root and restart the language server:

```json
{
  "database": "Samples",
  "tables": {
    "StormEvents": {
      "State": "string",
      "StartTime": "datetime",
      "DamageProperty": "long"
    }
  }
}
```

Open `examples/offline-schema` as a Zed project to try the included snapshot
and query. The server reads the first workspace root at startup and reports an
invalid schema in the language server log. The snapshot is not refreshed
automatically. It suits a project that should not need a cluster or a sign-in.

To run the language server protocol tests:

```sh
dotnet restore server/KustoLanguageServer.csproj --ignore-failed-sources
dotnet build server/KustoLanguageServer.csproj --no-restore
python3 server/test_server.py
```

The schema tests run against a fake cluster in the test file, so they need no
network or sign-in (the server reads `KUSTO_LSP_TEST_TOKEN` and
`KUSTO_LSP_TEST_ENDPOINTS` for that, which nothing else should set).

There is no Tree-sitter grammar: the one this extension started with covered only part of Kusto, and the
language server's colours (see Colours above) replace it. `examples/highlighting.kql` is a file to open to see
them. The server's tests check the tokens themselves, so no fixture check is needed.
