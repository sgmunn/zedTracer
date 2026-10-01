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
