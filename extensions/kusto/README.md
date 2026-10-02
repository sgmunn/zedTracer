# Kusto editing spike

Install this directory as a Zed dev extension, then open `examples/highlighting.kql`.
The extension recognizes `.kql` files and provides Tree-sitter highlighting.
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
Diagnostics cover syntax only.

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

The editor and the language server read the declarations and the files with the same rules, and agree on the declared names
through `fork-docs/samples/query-parameters.json`.

## History

Every run is saved as a result file in the `kusto/history` folder of Zed's data folder. **kusto: show history**
(in the command palette) lists the queries that were run, newest first, with the time, the number of rows, how
long it took, where it ran and the query's first line of code, after any comment lines the query starts with
(`// incident 123 — second try`), which makes a good label. Directive comments (`// :setDefaultDb(...)`) are not shown. Type to search the query text, the cluster, the
database or an error message. Enter shows that result in the Results panel, and Cmd-Enter on macOS (Ctrl-Enter elsewhere)
opens it in a tab. A failed run shows its error. The trash button that appears when you hover a row
deletes that result's file; the row stays in the list, marked `result deleted`.

The list comes from the run log, `runs.jsonl`, which keeps the last 200 or so runs, so older results are still
on disk but no longer listed.

The history cannot grow without limit. After a run, and when Zed starts, the oldest result files beyond these
settings are deleted (a limit of 0 means no limit):

```json
{ "kusto": { "history_max_results": 50, "history_max_megabytes": 1024 } }
```

Only files named like a run's result (`20261001-203917-<uuid>.ktt`) are ever deleted, so a result you saved into
that folder yourself is safe, and so is any result open in a tab or in the panel of the window that ran the query.

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
every time the server starts). Delete the `kusto/schema` folder to throw the cache away; an entry that cannot be
read, or was written by another version, is ignored and replaced. Names in file paths are escaped, so no
cluster or database name can write outside the folder. Files are written whole and then moved into place, so
several Zed windows can share the cache. The token's audience is also asked for once per cluster instead of on
every request.

Signature help appears inside the parentheses of a function call, including
database functions with their parameter names and types. It finds unqualified
function names only.

A file holds several queries separated by blank lines, and each is analysed on its own: a query
never continues into the next one, and a `let` in one is not visible in the next. Diagnostics are still syntax-only: table and column references are not
validated.

## Code lenses

Turn code lenses on in Zed settings (they are off by default):

```json
{ "code_lens": "on" }
```

Above each query (a run of non-blank lines) the server shows **▶ Run**, which runs that query. While the
query is running it shows **⠹ Running… 12 s** (a spinner and the elapsed time, refreshed four times a second
until nothing is running) and **Cancel** instead. Once the query has been run, the same
lens row also shows what the last run did, for example `Last run: 10:42:11, took 1.8 s, 1,240 rows`,
**Results** (shows that run's saved result in the Results panel) and **Copy CID** (copies the run's client
request id). A run that failed shows `Last run failed: <the first line of the message>`. The last run is
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

The grammar is pinned to `Willem-J-an/tree-sitter-kusto` at the commit in
`extension.toml`. To run the fixture check, install the `tree-sitter` CLI, clone
that grammar, and run:

```sh
./test-highlighting.sh /path/to/tree-sitter-kusto
```

The grammar has incomplete coverage of Kusto, so complex or newer query forms
may parse with errors and lose some
highlighting. Grammar coverage and cluster schema integration need evaluation
before shipping this as default language support.
