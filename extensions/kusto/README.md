# Kusto highlighting spike

Install this directory as a Zed dev extension, then open `examples/highlighting.kql`.
It recognizes `.kql` files and highlights basic Kusto queries without a language server.

The grammar is pinned to `Willem-J-an/tree-sitter-kusto` at the commit in
`extension.toml`. To run the fixture check, install the `tree-sitter` CLI, clone
that grammar, and run:

```sh
./test-highlighting.sh /path/to/tree-sitter-kusto
```

This is a syntax highlighting spike. The grammar has incomplete coverage of
Kusto, so complex or newer query forms may parse with errors and lose some
highlighting. Grammar coverage should be evaluated before expanding authoring
features or shipping this as default language support.
