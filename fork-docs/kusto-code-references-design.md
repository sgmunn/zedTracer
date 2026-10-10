# Code references: from a trace to the code that wrote it

Design notes, written 2026-10-09. Local repositories come first and are built; the other sources are listed so they stay possible. Status of the build is in [`kusto-results-workbench-status.md`](kusto-results-workbench-status.md).

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

**Repositories** (`search.rs`). A root such as `/Users/gregm/Projects/mwc` is scanned one level deep; each child holding a `.git` is a repository. Searching is `git grep` with `-n -z -I --no-color` over `*.cs`, one process per repository, run together. Two traps: `git grep -E` on macOS does not understand `\b` or `\s` (use `-w` and `[[:space:]]` if a regular expression is ever needed), and `git grep` reads the working tree, so uncommitted changes are searched too. Patterns are plain strings, never regular expressions: over these repositories an alternation such as `(class|struct)[[:space:]]+Name` took 4.3 s of CPU where four fixed strings (`class Name`, `struct Name`, ...) with `-w` take a fraction of that, and the hits are filtered in Rust. Output is capped, and `GIT_OPTIONAL_LOCKS=0` keeps it from taking the index lock.

**Resolution** (`resolve.rs`).

- *Marker.* Search for the quoted string. A hit on a `[CodeMarkerName(...)]` line names the class declared just below it; a hit on a `const string N = "..."` line names `N`; any other hit (a literal passed straight to `MonitoredScope`) is itself a use site. An attribute is recognised by its syntax (a line that starts with `[` and holds `CodeMarkerName(`); code also reaches a marker as `CodeMarkers.X.Instance.CodeMarkerName`, which is a use. The name found is then searched with `-w` in all repositories for its use sites. A use site that turns out to be a `[CodeMarkerName(N)]` attribute defines a class, and that class is searched in turn (one more level). Uses are ranked: lines that also contain the enclosing class (`CodeMarkers.X`) first, then the longest shared directory prefix with the definition, then non-test paths. A marker with no use site returns its definition.
- *Symbol (`Class.Method`).* Find the declaration of the nearest segment as a class, struct, record or interface (four fixed strings, then filtered), then the method declaration inside those files (partial classes declare the type in each file, so all are covered). If the nearest segment is not a type, the one before it is tried. If the type does not declare the method, its base classes are tried, up to three levels and without interfaces, and the result then names the class that declares it (`AzureSqlDataPlaneBase.GetNewConnectionAsync` for a message that said `AzureSqlDatabaseDataPlane.`). If no base class has it either, the type is returned.
- *Which repositories.* A symbol is searched first in the repositories whose name shares a word with the process name (`Microsoft.Dms.Service.EntryPoint` and `workload-dms`), and in the others only if the type is not found there. A marker's use sites are searched first in the repository that declares the marker, and in the others only if there are none. This is what makes a lookup fast: a search of one repository takes about 0.17 s, of all eight about 0.75 s.
- *Ranking among candidates.* Within what was found, files whose `namespace` shares the most leading segments with the chain or the process name come first.
- *Enclosing method of a use site* is a label, found from the file by indentation (the nearest line above with less indentation that reads as a method signature). It is best effort and is replaced by the syntax tree when tree-sitter refinement exists.
- *Cache.* Results are kept in memory for the session, keyed by the search text; there is no stored index.

Results are `CodeRef { repo, path, line, kind, symbol, enclosing }` with the kind one of marker use, marker definition, method, or type.

## In the app

`crates/kusto_results_ui/src/code_references.rs`. `CodeReferences` is an entity owned by `RowDetailsPanel`, which hands it the selection (`ActiveSelection`) each time the panel refreshes.

- **Keys.** `code_keys` takes the first selected row: the marker, message and actor columns by their roles (`TraceColumns`, so a user's trace schema applies), and every cell of the row as the text to look for call stacks in. A multipart message is read assembled.
- **Settings.** `kusto_results.code.roots` (`ResultsSettings::code_roots`, `~` expanded). Empty means off, and the section says how to turn it on when a row has something to look up.
- **Resolver.** One `CodeResolver` for the current roots, made on first use in a background task with the project's shell environment (`default_environment`, the same one queries use for `az`) and rebuilt when the roots change or after a failure. A failure (git missing, a root with no checkouts) is shown in the section and tried again the next time the row is looked at.
- **Lookups** run on the background executor; selecting another row drops the running one. The first selection in a result also prefetches the literals of all its markers (up to 500), and the first lookup waits for that rather than repeating the search.
- **Section.** Between the header and the field blocks, at most 220 px tall and scrolling. Each entry: what it is (`Marker used in`, `Marker defined as`, `Marker names`, `Message names`, `Stack frame`), the method for a marker use or else the symbol, `repository · path:line`, and the line's text in the buffer font. A click opens the file with `Workspace::open_abs_path` and moves the cursor to the line.
- **Cache.** Results and the marker literal table are reused for five minutes (`CodeResolver::with_cache_lifetime`).

## Not yet

- The log call that wrote a message: search the longest stable chunk of the message (split on digits, GUIDs and quoted values) inside the method found. This also covers the events that have no prefix.
- Resolving all the markers of a result at once. `prefetch_markers` already finds every marker's literal with one search per repository; the use sites are still searched one marker at a time (about 0.2 to 0.5 s each), which is fine for the row being looked at but not for badging a whole trace.
- Properties and fields named by a message prefix (`Class.Property`); only methods are matched as declarations, so these resolve to the type.
- A peek at the code in Row Details (several lines in a read-only editor), instead of the one line and a click.
- The same section for the activity tree, the sequence and the timeline; a place to refresh the cache by hand.
- The other sources in the table: Zed's own language server, tree-sitter for the enclosing method, an MCP code search for what is not on disk.

## Verification

- Unit tests on key extraction (every pattern above, including the ones that must not match), on the source reading (declarations, tuple return types, the enclosing method, base types) and on `git grep` output and repository discovery.
- Resolver tests (`tests/local_repositories.rs`) on three small repositories created in a temporary directory with `git init`: each marker form, a short name defined in two repositories, a marker declared in a library and used in another repository, a process name choosing between two classes of the same name, an inherited method, a method declared nowhere, a dotted marker that is really a method, stack frames, and the cache.
- A test, ignored by default, that resolves every distinct marker and prefix of a real trace against real repositories and prints how many resolved: `cargo test -p kusto_code --test local_repositories -- --ignored --nocapture` (`KUSTO_CODE_ROOT` and `KUSTO_CODE_SAMPLE` override the defaults, `/Users/gregm/Projects/mwc` and `sample2.ktt`).

Result of that test on 2026-10-09, `sample2.ktt` against the nine checkouts under `/Users/gregm/Projects/mwc` (a debug build, one lookup at a time):

- 59 distinct markers: the literals of all of them were searched in 0.8 to 1.2 s, and 57 have a use site (about 0.3 to 0.5 s each). The two without one are in no local repository. The first run found only 55: code that reaches a marker as `CodeMarkers.X.Instance.CodeMarkerName` was taken for the attribute because the line contains that word, so an attribute is now recognised by its syntax.
- 64 distinct message prefixes (with their process): 52 resolve to a method and 6 to a type only (properties, and classes whose base class is not on disk). The rest are classes from libraries that are not checked out. A type-only result can also be the wrong class: `MemoryCache.GetObjectAsync` found an unrelated generic `MemoryCache<TKey, TValue>` in a tool, because the framework's own class is not in any repository. It is the lowest tier for that reason.
- Before the searches were staged (home repository first) and the declaration search used fixed strings, the same run took 138 s instead of 42 s.
