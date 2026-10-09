# Code references: from a trace to the code that wrote it

Design notes, written 2026-10-09. Local repositories come first; the other sources are listed so the first slice does not close them off. Status of the build is in [`kusto-results-workbench-status.md`](kusto-results-workbench-status.md).

## Goal

Select a trace row and quickly see the code behind it: where the code marker is used, which method a `Class.Method` message prefix or an exception frame names, and, later, the log call that wrote the message. It must not need an index built ahead of time, and the code may come from a local checkout or, later, from a search service such as an MCP server.

## The prior work

`/Users/gregm/Projects/dev/KustoTraceTools` (`KustoTraceTools.CodeAnalysis`, `KustoTraceTools.Core/Analytics/CodeIndex.cs`) solved it with a Roslyn pass over each solution, saved as two JSON files:

- `CodeMarkerReference.json`: for each `[CodeMarkerName("x")]` on a marker type, the first use site of that type (file, line, type, method), found with `SymbolFinder.FindReferencesAsync`.
- `TypeInformation.json`: every class, struct and record with its methods, and for each method the literal text of its log calls (`LoggedMessagePattern`).

Annotating a trace then looked a marker up exactly, matched a leading `A.B.Method` (or a bracketed `[A.B.Method]`) in the message against the type index, walking the class candidates from the nearest, and scored the message against the method's log patterns to find the line that wrote it.

What it costs: a built solution, a Roslyn run per repository and version, and an index that is stale as soon as the checkout moves. It also read only the attribute form of a marker (see below).

## What the data looks like

Measured on the two local sample traces (`samples/sample1.ktt`, `sample2.ktt`, not committed) and the eight repositories under `/Users/gregm/Projects/mwc`.

- **Message prefix.** In `sample2`, 2,976 of 6,413 events (46%) start with `Class.Method`, nearly all of two parts; in `sample1`, 44 of 1,002 (4%).
- **Stack frames.** Exception call stacks hold `Namespace.Class.Method` frames, but only 22 of about 2,200 frames in `sample2` carry a build path, and none in `sample1`. A frame gives a symbol, rarely a file.
- **Markers come in three forms in the code**, not one:
  1. `[CodeMarkerName("x")]` on a class (`sealed class X : SingletonMonitoredScopeCodeMarker<X>`), used as `CodeMarkers.X.ExecuteAsync(...)`.
  2. `public const string Name = "x";` in a `CodeMarkers` class, used as `new MonitoredScope(CodeMarkers.Name)`.
  3. `[CodeMarkerName(codeMarker: SomeConstName)]`, the attribute with a constant instead of a literal.

  Roughly 40% of the markers in the samples are the constant form, which the Roslyn indexer never saw.
- **A short marker type name is not unique.** `GetDatamartInformation` is a marker class in at least three `CodeMarkers.cs` files in one repository; the marker string (`Microsoft.Dms.Metadata.GetDatamartInformation`) is what is unique. Use sites have to be tied back to the definition by where they live, not by name alone.
- **One search per repository covers a whole trace.** The distinct markers of a trace (about 60) go into one `git grep -F -e '"m1"' -e '"m2"' ...` per repository. For both samples that took 0.8 s in total over eight repositories (about 14,000 `.cs` files), and 60 of 61 and 57 of 59 markers were found as a string literal. The misses (`WCL-CapacityContext-GetCapacityInfo`, `MWC.OneLakeService.TelemetryLoggingMiddleware.Invoke`) are in no local repository, as a search of every file type showed; that is what a remote source is for.
- **The repositories** have between 200 and 10,500 tracked files. A full `git grep` takes 0.15 to 0.3 s each.

## Options considered

| Source | How it finds code | Verdict |
| --- | --- | --- |
| Text search on demand (`git grep`) | Marker string, then the marker's name, then its use sites; class declaration, then method declaration; later the longest literal chunk of the message. | **Chosen for the first slice.** No setup, works on any checkout, deterministic. |
| Zed's own LSP (`Project::symbols`, references on a marker type) | The closest thing to what Roslyn did. | Later, as a precision upgrade. Needs the repo open and the language server loaded. |
| Tree-sitter on the one file a search found | Enclosing method; the log calls of that method scored against the message. | Later. Refines a hit, cannot find one. |
| MCP code search (for example bluebird, through `crates/context_server`) | A configured tool call per key. | Later, for repositories that are not on disk. The tool's schema is not known yet. |
| Hand the keys to the query-thread agent | A prompt built from the keys. | Later, as a fallback for what is still unresolved. |
| Source paths in stack frames | Strip the build root, match the suffix. | A bonus; the paths are rare in the data. |

## Design: local repositories

A new UI-free crate, `crates/kusto_code`, in three parts. It takes plain strings and knows nothing about tables or windows, so it can be tested without either.

**Keys** (`keys.rs`). From one event: the marker; a `Class.Method` chain at the start of the message, or in brackets (the two patterns of `CodeIndex.MatchTypeReference`); and the first application frames of any call stack in the event's text. A chain counts only if its last two segments start with an upper-case letter, which is the C# convention and keeps host names and "e.g." out. Compiler-generated names (`<Method>d__5.MoveNext`, `<>c__DisplayClass`) are reduced to the method; framework frames are dropped.

**Repositories** (`search.rs`). A root such as `/Users/gregm/Projects/mwc` is scanned one level deep; each child holding a `.git` is a repository. Searching is `git grep` with `-n -z -I --no-color` over `*.cs`, one process per repository, run together. Two traps: `git grep -E` on macOS does not understand `\b` (use `-w`) or `\s` (use `[[:space:]]`), and `git grep` reads the working tree, so uncommitted changes are searched too. Output is capped, and `GIT_OPTIONAL_LOCKS=0` keeps it from taking the index lock.

**Resolution** (`resolve.rs`).

- *Marker.* Search for the quoted string. A hit on a `[CodeMarkerName(...)]` line names the class declared just below it; a hit on a `const string N = "..."` line names `N`; any other hit (a literal passed straight to `MonitoredScope`) is itself a use site. The name found is then searched with `-w` in all repositories for its use sites. A use site that turns out to be a `[CodeMarkerName(N)]` attribute defines a class, and that class is searched in turn (one more level). Uses are ranked: lines that also contain the enclosing class (`CodeMarkers.X`) first, then the longest shared directory prefix with the definition, then non-test paths. A marker with no use site returns its definition.
- *Symbol (`Class.Method`).* Find the declaration of the nearest segment as a class, struct, record or interface (`-w -E`), then the method declaration inside those files. If the nearest segment is not a type, the one before it is tried. If the type is found but not the method (inherited, extension), the type is returned.
- *Ranking among candidates.* Repositories whose name shares a word with the process name (`Microsoft.Dms.Service.EntryPoint` and `workload-dms`) first, then the files whose `namespace` shares the most leading segments with the chain. This only orders results; nothing is excluded.
- *Enclosing method of a use site* is a label, found from the file by indentation (the nearest line above with less indentation that reads as a method signature). It is best effort and is replaced by the syntax tree when tree-sitter refinement exists.
- *Cache.* Results are kept in memory for the session, keyed by the search text; there is no stored index.

Results are `CodeRef { repo, path, line, kind, symbol, enclosing }` with the kind one of marker use, marker definition, method, or type.

## Not in the first slice

- Row Details "Code" section and opening the file at the line (the selection it follows is `ActiveSelection` in `row_details_panel.rs`).
- Settings for the repository roots (the crate takes them as an argument).
- The log call that wrote a message: search the longest stable chunk of the message (split on digits, GUIDs and quoted values) inside the method found. This also covers the events that have no prefix.
- The other sources in the table.

## Verification

- Unit tests on key extraction (every pattern above, including the ones that must not match).
- Resolver tests on small repositories created in a temporary directory with `git init`, one for each marker form and for a short name defined in two places.
- A test, ignored by default, that resolves every distinct marker and prefix of the local samples against `/Users/gregm/Projects/mwc` and prints how many resolved.
