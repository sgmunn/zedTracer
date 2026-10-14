# Inline functions in a query file (exploration, not built)

Asked for by the user: define functions in the file itself, and have them included automatically in the query that is run or edited, the way `// :setParam` values (see "Values in the file" in `extensions/kusto/README.md`) persist for the rest of the document.

## What was probed

A scratch project against `Microsoft.Azure.Kusto.Language` 12.4.0, the same library the server uses (nothing here is guessed from documentation). In the session scratchpad, not kept.

| Question | Answer |
| --- | --- |
| Can a function be added to the symbols a query is analysed with, without touching the query text? | Yes. `database.AddMembers(new FunctionSymbol(name, "(x:long)", "{ x * 2 }"))`, the same constructor `SchemaManager` uses for stored functions. Diagnostics, completion (`Double(x)`, kind `DatabaseFunction`), hover (`(function) Double(x): long`), classification (`Function`) all work, columns of a tabular function are known (`Recent(3) \| project Nope` is `KS142`), defaults work, and functions may call each other in any order. |
| Does a prepended `let` give the same editing features? | Yes, but every diagnostic, completion edit, hover and token position shifts by the prepended length, in five places in `Program.cs` and `SemanticTokens.cs` that all do `block.Start + offset`. The symbol route needs none of that. |
| An error inside a function's body, when the function is a symbol? | Reported at the call site as `KS108 Scalar value expected`, not at the body, and a syntax error in a body is not reported at all. A definition therefore has to be analysed where it is written. |
| Can a block of only `let` statements be analysed on its own? | Yes. `let f = (x:long) { x * 2 };` with no final expression has no diagnostics, and a bad name inside is `KS142` at the right position. No suffix is needed. |
| Can the functions be read out of such a block? | Yes. `GetDescendants<LetStatement>()` with `Expression is FunctionDeclaration` gives the name, `Parameters` (`(a:string, b:long = 5)`) and `Body` (`{ … }`) as text, ready for `FunctionSymbol`. |
| A query's own `let` with the name of an inline function? | Fine when the function is a symbol (the `let` shadows it). `KS201 already declared` when a `let` of the same name is prepended. |
| Unused `let` with a bad body? | Still an error (`KS142`). Including every function in scope in every query would break a query on another database, so include only what the query uses. |
| `declare query_parameters` and `let` order? | The library takes either order, but a function body that uses a declared parameter only binds when the `declare` comes first (`KS142` otherwise). Put the functions after the query's `declare`/`set` statements. |

## Proposal

**Syntax: a definition block.** A block (run of non-blank lines) that has the directive line `// :functions` is a block of definitions. Everything else in it is ordinary Kusto, so it gets colours, completion and diagnostics:

```kql
// :functions
let Normalize = (value:string) { tolower(trim(" ", value)) };
let Recent = (count:long) { Events | top count by At desc };

Events
| extend Key = Normalize(Name)
```

Like a block of only comments, it is not a query: no lenses, F5 does not run it. That needs one rule in both `query_blocks` (Rust) and `QueryBlocks.Find` (C#), tested by a shared cases file like the others. Rejected: a one-line directive holding the body as a string (a body is several lines and gets no editing support), and the body written inside comments (same).

**Scope: like parameters.** A definition applies to every query below it. A later definition of the same name replaces it, and `// :clearFunction("Name")` and `// :clearFunctions()` take it away. A function can use functions defined above it, and tables and functions of the query's database.

**Running.** The editor adds `let` statements for the functions the query uses, and those they use in turn (a token scan of the query outside comments and strings; a false hit only adds an unused `let`), after the query's `declare query_parameters` and `set` statements and before the rest, skipping a name the query defines itself. A control command gets none. This has to work without the language server, so it is done in Rust (`kusto_client`), with its own shared cases.

- The text sent is not the text written. `QueryRequest.query` is both today (the run log, the lens key, `cancel_newest`, the history all use it), so the request needs the written text and the text to send. The log, the history and the lens keep the written text, as they do for parameters.
- The query kept in the `.ktt` and given by Copy is the portable query, which already turns `declare query_parameters` into `let` statements; the functions go after those, so the copy runs anywhere.

**Editing (server).** For each query, the definitions above it become `FunctionSymbol`s in the database of its connection (in `SchemaManager.GlobalsFor`, or a wrapper that takes the document). Two things to look after:

1. `PortableQuery.Qualify` writes `cluster('…').database('…').` in front of any name that is a member of the database; an inline function must not count as one.
2. The definition block is analysed on its own against the functions above it, and its own functions are added to the scope only for what follows.

## Cost

Rough order, each step useful alone:

1. Rust: recognise definition blocks, collect them, add the `let`s on run and in the copy; request with written and sent text. About the size of the parameter directives plus the request change.
2. Server: recognise the block, build the symbols, analyse the definition block, keep inline functions out of `Qualify`; completion offers `// :functions`.
3. `clearFunction`, `clearFunctions`, a lens or hover that says where a function comes from.

## Decisions for the user

1. The marker: `// :functions` on the block, or another name (`// :define`, `// :let`).
2. Whether only the functions a query uses are added (recommended), or every function in scope.
3. Whether Copy and the stored query should show the added `let`s (recommended: yes, as for parameter values), or only the written query.
