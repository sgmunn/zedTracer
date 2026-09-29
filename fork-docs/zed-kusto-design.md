# zedTracer: design context and implementation handoff

Recommended repository location: `fork-docs/zedTracer-design.md`.

This handoff captures the direction from **Zed extensions overview** and the user's requested summary. It is design context, not a claim that the architecture has been implemented or verified against the current Zed source.

## Goal and fork boundary

Build a native investigation and KQL workbench using a small Zed fork rather than targeting a public extension. The intended experience needs custom native UI and editor integration; the fork provides room to prototype those capabilities directly.

Keep most implementation in new GPUI crates, with narrow registration and integration changes in existing Zed crates. Minimize upstream divergence, avoid unrelated refactors, and isolate any independently reproduced upstream fixes into separate commits.

A native `InvestigationPanel` and custom file viewer are the proposed integration points. Treat both as feasible directions to prove in a small prototype: validate panel registration, opening a custom viewer as a workspace item, focus, pane behavior, and lifecycle against the chosen Zed revision before building the full workbench.

## Proposed architecture

| Responsibility | Direction |
| --- | --- |
| KQL editing | Use Zed language support for authoring. Explore a .NET LSP backed by `Microsoft.Azure.Kusto.Language` for diagnostics, completion, and semantic assistance, with cluster/database schema context. The library is a potential foundation, not a ready-made LSP. |
| Query execution | Use a separate .NET service/process for Kusto connectivity, authentication, execution, cancellation, and typed results. Keep query execution distinct from LSP responsibilities. |
| Native UI | New GPUI crates own investigation controls, pane-associated results, grid state, and the cell inspector. Keep process communication behind a small interface. |
| Result presentation | Build on the native GPUI `data_table` direction; first verify the component's integration and scaling limits. No charts initially. |

Start with a small process protocol covering request identity, query/context, cancellation, schema, typed rows, completion, and errors. Preserve types across the boundary rather than turning every value into display text. Avoid choosing a complex transport before measuring actual needs.

## Result-grid behavior

- Keep an immutable typed result set and a separate visible-row index. Apply local sorting and filtering to the index, preserving the source values and source order.
- Assign each row a stable original `result_row_index` within its result set. Keep it separate from display position; use it to break sort ties and provide an explicit “Restore result order” action. Clearing a sort restores original order among rows that still pass the filter.
- Show active sort/filter state clearly. Compare numbers, timestamps, booleans, strings, and nulls according to documented type rules rather than formatted strings. Define handling for dynamic values explicitly.
- Support cell/range/row copy and column resizing. A cell inspector shows complete wide values and expandable or formatted dynamic/JSON content without forcing huge cells into the grid.
- Attach results to their originating editor panes/query context. A later response must not replace another pane's results accidentally. Retain query text, execution context, and run identity with each result set.
- Support cached and pinned result sets so useful runs survive subsequent executions. Specify memory limits, eviction, and pin behavior; persistence across app restarts remains a design choice.

### CSV ordering observation

The CSV preview appeared reversed, but the user later suspected an active column sort and confusion between display numbering and original row numbering. That is the likely explanation, **not a confirmed Zed bug**; the screenshot alone did not establish the exact behavior. Use this observation to motivate stable original/result row indexing, visible sort state, and reliable order restoration. Preserve the order received for each run; that does not promise identical server ordering across separate executions.

## Staged implementation

1. **Prove native integration.** Add isolated GPUI crate(s), a minimal `InvestigationPanel`, and a custom viewer with fixture data. Verify editor/pane association, focus, opening, and closing.
2. **Prove grid semantics and scale early.** Add typed fixtures, row indexing, sorting/filtering, copy, resizing, and the inspector. Test 100k–500k rows with 15–30 columns, including nulls, duplicate values, wide strings, and dynamic/JSON. Measure load time, memory, scrolling, sorting, and filtering; use virtualization and keep expensive work off the UI thread where needed.
3. **Connect execution.** Add the .NET process and a complete query-to-grid path, including cancellation, errors, typed transport, and correct routing of concurrent results to panes.
4. **Improve authoring.** Add KQL language support and prototype the optional .NET LSP with schema-aware diagnostics/completion. Validate library coverage before expanding the LSP surface.
5. **Make investigations durable.** Add bounded caching and pinning, settle pane lifecycle and persistence behavior, and repeat representative performance and UI checks before broader features.

Unit-test typed comparisons, filter behavior, stable ties, sort reset, and result routing where practical. Use Computer Use to exercise interactions that unit tests cannot establish. Set performance acceptance targets after recording a baseline on named hardware; the row/column ranges above are test workloads, not proven capacity.

## AGENTS.md versus design documentation

Keep this architecture, rationale, scope, open choices, and staged plan in `fork-docs/zedTracer-design.md`. Link to it from `AGENTS.md` rather than copying the discussion there.

Reserve `AGENTS.md` for durable working instructions: commit in logical, cohesive chunks with meaningful messages; add tests where possible; use Computer Use for behavior that cannot be unit-tested before handoff; use pragmatic engineering and refactor as needed. If adopted as a lasting project rule, also record the narrow fork boundary and preference for isolated GPUI crates there.


## Chat recommendations

What I'd build first

I think there's a very clean progression:

Slice	Work	Difficulty
1	.kql syntax highlighting	Low
2	Native GPUI static result table	Low
3	Sorting/filtering/resizing/copy	Low–Medium
4	Kusto connection + execute	Medium
5	Editor → execute selection/current query	Medium
6	Cluster/database selector	Low–Medium
7	Schema retrieval/cache	Medium
8	KQL LSP using Kusto.Language	Medium–High
9	Cell inspector / dynamic JSON	Medium
10	Pinned/cached result sets	Medium

