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

## Schema from your clusters

Tell the server which cluster and database unqualified names refer to in Zed
settings (this is separate from the `kusto` settings that choose where F5 runs
queries, so set both):

```json
{
  "lsp": {
    "kusto-lsp": {
      "initialization_options": {
        "cluster": "https://help.kusto.windows.net",
        "database": "Samples"
      }
    }
  }
}
```

The server signs in with `az account get-access-token`, so run `az login`
first. It fetches the schema in the background the first time a cluster or
database is referred to, so the first completion after opening a file may not
list tables yet; ask again a moment later. Tables, external tables,
materialized views and functions are loaded, including each function's
parameters and doc string. A query that names another cluster, such as
`cluster('other').database('Logs').Requests`, loads that cluster and database
the same way. A short name such as `help` means `help.kusto.windows.net`; other
clouds need the full host name. Schema is kept in memory only, so each server
start fetches it again, and a failed fetch is retried after a minute.

Signature help appears inside the parentheses of a function call, including
database functions with their parameter names and types. It finds unqualified
function names only.

Diagnostics are still syntax-only: table and column references are not
validated.

## Code lenses

Turn code lenses on in Zed settings (they are off by default):

```json
{ "code_lens": "on" }
```

Above each query (a run of non-blank lines) the server shows **▶ Run**, which runs that query. While the
query is running it shows **Running…** and **Cancel** instead. Once the query has been run, the same
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
`kusto::CopyClientRequestId`) and, optionally, the data the action takes. Zed moves the cursor to the lens
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
