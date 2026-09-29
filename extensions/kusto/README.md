# Kusto editing spike

Install this directory as a Zed dev extension, then open `examples/highlighting.kql`.
The extension recognizes `.kql` files and provides Tree-sitter highlighting.
It also launches a local language server for completion, hover information, and
syntax diagnostics. The server uses `Microsoft.Azure.Kusto.Language` 12.4.0 and
does not connect to a cluster.

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
Diagnostics cover syntax only. Without a cluster schema, the server cannot
complete or validate real table and column names; adding a schema source is the
next step toward full IntelliSense.

To run the language server protocol tests:

```sh
dotnet restore server/KustoLanguageServer.csproj --ignore-failed-sources
dotnet build server/KustoLanguageServer.csproj --no-restore
python3 server/test_server.py
```

The grammar is pinned to `Willem-J-an/tree-sitter-kusto` at the commit in
`extension.toml`. To run the fixture check, install the `tree-sitter` CLI, clone
that grammar, and run:

```sh
./test-highlighting.sh /path/to/tree-sitter-kusto
```

The grammar has incomplete coverage of
Kusto, so complex or newer query forms may parse with errors and lose some
highlighting. Grammar coverage and cluster schema integration need evaluation
before shipping this as default language support.
