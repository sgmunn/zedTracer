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

Completion includes Kusto keywords, built-in functions, and names declared in
the current query. Hover shows Kusto's quick information for supported symbols.
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
lenses and F5 does not run it. A directive that does not parse gets a warning. Typing `// :` offers both, as soon as the colon is typed.

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
  the parameter values the file holds, and shows the new result in the Results panel. The file itself does not
  change.

Both are shown for any result file that names its query, and Run again also needs the file to name a cluster and
database. Files written before parameters were kept in them simply run without values.

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
materialized views and functions are loaded, including each function's
parameters and doc string. A query that names another cluster, such as
`cluster('other').database('Logs').Requests`, loads that cluster and database
the same way. A short name such as `help` means `help.kusto.windows.net`; other
clouds need the full host name.

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

#### Diagnostics

Syntax errors are always shown. Once the schema of the database a query runs on has arrived (from the cache or
the cluster), **names that are not in it are errors too**: an unknown table, column, function or variable
(`The name 'Nope' does not refer to any known table, tabular variable or function.`), and the other mistakes
Kusto's own analysis finds, such as a wrong argument type. Some rules keep this honest:

- A query is checked only when the schema of everything it refers to has loaded: the database it runs on, and
  any other cluster or database it names with `cluster(...)` or `database(...)`. While a schema is still arriving,
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

Above each query (a run of non-blank lines) the server shows **▶ Run**, which runs that query. While the
query is running it shows **⠹ Running… 12 s** (a spinner and the elapsed time, refreshed four times a second
until nothing is running) and **Cancel** instead. Once the query has been run and left a result, **Results**
comes straight after them, with the number of rows the result has, for example `Results (1,240 rows)` (it shows that
run's saved result in the Results panel, or in a tab in a query thread). It is placed ahead of the connection, schema and parameter lenses so that it stays in reach when
those are longer than the editor is wide. After them, the same lens row shows what the last run did, for example
`Last run: 10:42:11, took 1.8 s`, and **Copy CID** (copies the run's client request id). A run that failed shows `Last run failed: <the first line of the message>`. The last run is
matched by the text of the query without its comments and layout, so reformatting a query keeps its lens.

The lenses come from a log the editor keeps: every run appends a line when it starts and when it ends
to `kusto/history/runs.jsonl` in Zed's data folder, and the server watches that file and asks Zed to
refresh the lenses when it changes. The extension tells the server where the data folder is through
`KUSTO_ZED_DATA_DIR`, working it out from the extension's own working directory. If you run the
server some other way, set `dataDir` in `initialization_options` instead. A run that never reports an
end, such as when Zed was closed while it ran, stops counting as running after 15 minutes. The log keeps
about the last 200 runs.

A lens acts through `zed.dispatchAction`, a lens command this fork of Zed handles in the editor: its
arguments are the name of a Zed action (`kusto::RunQuery`, `kusto::CancelQuery`, `kusto::ShowResult`,
`kusto::CopyClientRequestId`, `kusto::SelectParameterProfile`) and, optionally, the data the action takes. Zed moves the cursor to the lens
before it runs the action, so `RunQuery` and `CancelQuery` act on that query.

After updating, run `./install-server.sh`, then **Zed: Rebuild Dev Extension**, then restart Zed.

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
