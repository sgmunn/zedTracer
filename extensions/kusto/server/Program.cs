using System.Text;
using System.Text.Json;
using Kusto.Language;
using Kusto.Language.Editor;
using Kusto.Language.Symbols;

await new KustoLanguageServer(Console.OpenStandardInput(), Console.OpenStandardOutput()).RunAsync();

internal sealed partial class KustoLanguageServer(Stream input, Stream output)
{
    /// <summary>The text of the open profiles files, which are YAML and not queries.</summary>
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, string> profilesFiles = new(StringComparer.Ordinal);
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, DocumentSnapshot> documents = new(StringComparer.Ordinal);
    private readonly SemaphoreSlim writeLock = new(1, 1);
    private SchemaManager schema = new(new KustoRestClient(), GlobalState.Default, new SchemaCache(null));
    private RunLog runLog = new(null);
    private DefaultsFile defaultsFile = new(null);
    private IReadOnlyList<string> workspaceFolders = [];
    private volatile bool clientReady;

    /// <summary>Whether names are checked against the schema, which the `schemaDiagnostics` option can turn off.</summary>
    private bool schemaDiagnostics = true;
    private readonly Dictionary<string, FileChangeWatcher> parameterWatchers = new(StringComparer.Ordinal);
    private int nextRequestId;
    private Timer? spinner;
    private readonly object spinnerGate = new();

    public async Task RunAsync()
    {
        while (await ReadMessageAsync(input) is { } message)
        {
            using (message)
            {
                var root = message.RootElement;
                if (!root.TryGetProperty("method", out var methodElement))
                    continue;

                var method = methodElement.GetString();
                var hasId = root.TryGetProperty("id", out var id);
                try
                {
                    if (method == "exit")
                        return;

                    var parameters = root.TryGetProperty("params", out var value) ? value : default;
                    var result = await HandleAsync(method, parameters);
                    if (hasId)
                        await SendAsync(new { jsonrpc = "2.0", id, result });
                }
                catch (Exception exception)
                {
                    Console.Error.WriteLine($"{method}: {exception}");
                    if (hasId)
                    {
                        await SendAsync(new
                        {
                            jsonrpc = "2.0",
                            id,
                            error = new { code = -32603, message = exception.Message }
                        });
                    }
                }
            }
        }
    }

    private async Task<object?> HandleAsync(string? method, JsonElement parameters)
    {
        switch (method)
        {
            case "initialize":
                var dataDirectory = ResolveDataDirectory(parameters);
                schema = new SchemaManager(new KustoRestClient(), LoadGlobals(parameters), new SchemaCache(dataDirectory));
                schema.Changed += () =>
                {
                    // Nothing may be asked of the client before it has said it is ready.
                    if (!clientReady)
                        return;
                    _ = RequestCodeLensRefreshAsync();
                    // A schema that has arrived, or changed, decides what a name is and what is a wrong one.
                    _ = RequestSemanticTokensRefreshAsync();
                    _ = RepublishDiagnosticsAsync();
                };
                ApplyInitializationOptions(parameters);
                workspaceFolders = WorkspaceFolders(parameters);
                StartRunLog(dataDirectory);
                StartDefaultsFile(dataDirectory);
                return new
                {
                    capabilities = new
                    {
                        textDocumentSync = 1,
                        completionProvider = new { triggerCharacters = new[] { "|", ".", "(", ":" } },
                        signatureHelpProvider = new { triggerCharacters = new[] { "(", "," } },
                        codeLensProvider = new { resolveProvider = false },
                        executeCommandProvider = new { commands = new[] { CodeLenses.NoopCommand, CodeLenses.ConnectionCommand, CodeLenses.RefreshSchemaCommand } },
                        hoverProvider = true,
                        semanticTokensProvider = new
                        {
                            legend = new { tokenTypes = SemanticTokens.Legend, tokenModifiers = Array.Empty<string>() },
                            full = true
                        }
                    },
                    serverInfo = new { name = "kusto-lsp", version = "0.1.0" }
                };
            case "initialized":
                clientReady = true;
                return null;
            case "shutdown":
                return null;
            case "textDocument/didOpen":
            {
                var textDocument = parameters.GetProperty("textDocument");
                var uri = textDocument.GetProperty("uri").GetString()!;
                // Zed offers this server every YAML file, and only the profiles files are its business.
                if (QueryParameters.IsYaml(uri))
                {
                    if (QueryParameters.IsProfilesFile(uri))
                        profilesFiles[uri] = textDocument.GetProperty("text").GetString() ?? "";
                    return null;
                }
                var document = new DocumentSnapshot(textDocument.GetProperty("text").GetString() ?? "");
                documents[uri] = document;
                WatchParameterFiles();
                await PublishDiagnosticsAsync(uri, document);
                LoadReferencedSchema(document);
                return null;
            }
            case "textDocument/didChange":
            {
                var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString()!;
                var changes = parameters.GetProperty("contentChanges");
                if (QueryParameters.IsYaml(uri))
                {
                    if (profilesFiles.ContainsKey(uri) && changes.GetArrayLength() > 0)
                        profilesFiles[uri] = changes[changes.GetArrayLength() - 1].GetProperty("text").GetString() ?? "";
                    return null;
                }
                if (changes.GetArrayLength() > 0)
                {
                    var document = new DocumentSnapshot(changes[changes.GetArrayLength() - 1]
                        .GetProperty("text").GetString() ?? "");
                    documents[uri] = document;
                    await PublishDiagnosticsAsync(uri, document);
                    LoadReferencedSchema(document);
                }
                return null;
            }
            case "textDocument/didClose":
            {
                var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString()!;
                if (QueryParameters.IsYaml(uri))
                {
                    profilesFiles.TryRemove(uri, out _);
                    return null;
                }
                documents.TryRemove(uri, out _);
                WatchParameterFiles();
                await SendAsync(new
                {
                    jsonrpc = "2.0",
                    method = "textDocument/publishDiagnostics",
                    @params = new { uri, diagnostics = Array.Empty<object>() }
                });
                return null;
            }
            case "textDocument/completion":
                return Complete(parameters);
            case "textDocument/hover":
                return Hover(parameters);
            case "textDocument/signatureHelp":
                return GetSignatureHelp(parameters);
            case "textDocument/codeLens":
                return GetCodeLenses(parameters);
            case "textDocument/semanticTokens/full":
                return GetSemanticTokens(parameters);
            case "workspace/executeCommand":
                // The lenses that only show text name a command so that Zed treats them as clickable.
                if (parameters.TryGetProperty("command", out var command)
                    && command.GetString() == CodeLenses.RefreshSchemaCommand)
                {
                    var arguments = parameters.TryGetProperty("arguments", out var list) ? list : default;
                    string? Argument(int index) =>
                        arguments.ValueKind == JsonValueKind.Array && arguments.GetArrayLength() > index
                        && arguments[index].ValueKind == JsonValueKind.String
                            ? arguments[index].GetString()
                            : null;
                    // The request must not wait for the network, or completion would wait for it too.
                    _ = RefreshSchemaAsync(Argument(0), Argument(1));
                }
                return null;
            default:
                return null;
        }
    }

    private object[] Complete(JsonElement parameters)
    {
        if (!TryGetDocumentAndPosition(parameters, out var document, out var offset))
            return [];

        if (CompleteDirective(document, offset) is { } directives)
            return directives;

        var (queryText, queryStart) = QueryBlocks.Around(document.Text, offset);
        var completions = new KustoCodeService(queryText, GlobalsAt(document, offset))
            .GetCompletionItems(offset - queryStart);
        var start = Math.Clamp(queryStart + completions.EditStart, 0, document.Text.Length);
        var end = Math.Clamp(start + completions.EditLength, start, document.Text.Length);
        var range = new { start = document.PositionAt(start), end = document.PositionAt(end) };

        return completions.Items
            .Select(item =>
            {
                // Text after the cursor, such as the closing parenthesis, makes the item a snippet so the
                // cursor lands between the two parts instead of after all of the text.
                var hasAfterText = !string.IsNullOrEmpty(item.AfterText);
                var insertedText = hasAfterText
                    ? EscapeSnippet(item.BeforeText) + "$0" + EscapeSnippet(item.AfterText)
                    : string.Concat(item.ApplyTexts.Select(part => part.Text));
                return (object)new
                {
                    label = item.DisplayText,
                    kind = CompletionKind(item.Kind.ToString()),
                    sortText = item.OrderText,
                    filterText = item.MatchText,
                    insertTextFormat = hasAfterText ? 2 : 1,
                    textEdit = new { range, newText = insertedText }
                };
            })
            .ToArray();
    }

    private object? Hover(JsonElement parameters)
    {
        if (!TryGetDocumentAndPosition(parameters, out var document, out var offset))
            return null;

        var (queryText, queryStart) = QueryBlocks.Around(document.Text, offset);
        var info = new KustoCodeService(queryText, GlobalsAt(document, offset))
            .GetQuickInfo(offset - queryStart);
        return string.IsNullOrWhiteSpace(info.Text)
            ? null
            : new { contents = new { kind = "plaintext", value = info.Text } };
    }

    private object? GetSignatureHelp(JsonElement parameters)
    {
        if (!TryGetDocumentAndPosition(parameters, out var document, out var offset))
            return null;
        var (queryText, queryStart) = QueryBlocks.Around(document.Text, offset);
        return SignatureHelp.Get(queryText, GlobalsAt(document, offset), offset - queryStart);
    }

    private object GetSemanticTokens(JsonElement parameters)
    {
        var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString();
        var tokens = uri is not null && documents.TryGetValue(uri, out var document)
            ? SemanticTokens.Build(document, schema.GlobalsFor, schema.Defaults)
            : [];
        return new { data = tokens };
    }

    private object[] GetCodeLenses(JsonElement parameters)
    {
        var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString();
        if (uri is not null && profilesFiles.TryGetValue(uri, out var profilesText))
            return CodeLenses.ForProfilesFile(profilesText);
        if (uri is null || !documents.TryGetValue(uri, out var document))
            return [];
        // A project's .kusto folder may have appeared since the file was opened.
        WatchParameterFiles();
        var profiles = QueryParameters.Load(QueryParameters.FilesFor(uri, workspaceFolders));
        return CodeLenses.For(
            document,
            runLog.Read(),
            DateTimeOffset.UtcNow,
            schema.Defaults,
            query => QueryParameters.Describe(query, profiles),
            schema.StatusOf);
    }

    /// <summary>Fetches the schema again, and says in a message in the editor how that went.</summary>
    private async Task RefreshSchemaAsync(string? cluster, string? database)
    {
        string message;
        var type = 3;
        try
        {
            var (host, name, tables, functions) = await schema.RefreshAsync(cluster, database);
            message = name is null
                ? $"Refreshed the databases of {host}."
                : $"Refreshed the schema of {host} / {name}: {Count(tables, "table")}, {Count(functions, "function")}.";
        }
        catch (Exception exception)
        {
            type = 1;
            message = $"Could not refresh the schema: {exception.Message}";
            Console.Error.WriteLine($"workspace/executeCommand {CodeLenses.RefreshSchemaCommand}: {exception}");
        }

        try
        {
            await SendAsync(new { jsonrpc = "2.0", method = "window/showMessage", @params = new { type, message } });
            await RequestCodeLensRefreshAsync();
        }
        catch (Exception exception) when (exception is IOException or ObjectDisposedException)
        {
            Console.Error.WriteLine($"Could not report the schema refresh: {exception.Message}");
        }

        static string Count(int count, string noun) => $"{count} {noun}{(count == 1 ? "" : "s")}";
    }

    private static IReadOnlyList<string> WorkspaceFolders(JsonElement parameters)
    {
        var uris = new List<string?>();
        if (parameters.TryGetProperty("workspaceFolders", out var folders) && folders.ValueKind == JsonValueKind.Array)
            uris.AddRange(folders.EnumerateArray().Select(folder =>
                folder.TryGetProperty("uri", out var uri) && uri.ValueKind == JsonValueKind.String ? uri.GetString() : null));
        if (parameters.TryGetProperty("rootUri", out var root) && root.ValueKind == JsonValueKind.String)
            uris.Add(root.GetString());
        return uris
            .Select(uri => Uri.TryCreate(uri, UriKind.Absolute, out var parsed) && parsed.IsFile ? parsed.LocalPath : null)
            .OfType<string>()
            .Distinct()
            .ToList();
    }

    /// <summary>
    /// Follows the profile files of the open documents, so choosing another active profile, or
    /// editing a value, changes the lenses. Files nothing open uses are let go.
    /// </summary>
    private void WatchParameterFiles()
    {
        var wanted = documents.Keys
            .SelectMany(uri => QueryParameters.FilesFor(uri, workspaceFolders))
            .ToHashSet(StringComparer.Ordinal);
        lock (parameterWatchers)
        {
            foreach (var path in parameterWatchers.Keys.Where(path => !wanted.Contains(path)).ToList())
            {
                parameterWatchers[path].Dispose();
                parameterWatchers.Remove(path);
            }
            foreach (var path in wanted.Where(path => !parameterWatchers.ContainsKey(path)))
            {
                var watcher = new FileChangeWatcher(path);
                watcher.Changed += () => _ = RequestCodeLensRefreshAsync();
                if (watcher.Start(createDirectory: false))
                    parameterWatchers[path] = watcher;
                else
                    watcher.Dispose();
            }
        }
    }

    /// <summary>
    /// Where Zed keeps its data, which holds the run log and the schema cache. It comes from
    /// `KUSTO_ZED_DATA_DIR`, which the extension sets, or from the `dataDir` initialization option.
    /// </summary>
    private static string? ResolveDataDirectory(JsonElement parameters)
    {
        string? directory = Environment.GetEnvironmentVariable("KUSTO_ZED_DATA_DIR");
        if (parameters.TryGetProperty("initializationOptions", out var options)
            && options.ValueKind == JsonValueKind.Object
            && options.TryGetProperty("dataDir", out var configured)
            && configured.ValueKind == JsonValueKind.String)
            directory = configured.GetString();
        return directory;
    }

    /// <summary>Follows what the editor records about its runs.</summary>
    private void StartRunLog(string? directory)
    {
        runLog.Dispose();
        runLog = new RunLog(directory);
        runLog.Changed += OnRunLogChanged;
        runLog.Watch();
    }

    /// <summary>
    /// Follows the cluster and database the editor's settings name. They win over the
    /// initialization options, because they are what a run uses.
    /// </summary>
    private void StartDefaultsFile(string? directory)
    {
        defaultsFile.Dispose();
        defaultsFile = new DefaultsFile(directory);
        ApplyDefaultsFile();
        defaultsFile.Changed += OnDefaultsFileChanged;
        defaultsFile.Watch();
    }

    private bool ApplyDefaultsFile()
    {
        if (defaultsFile.Read() is not { } connection || connection == schema.Defaults)
            return false;
        schema.SetDefaults(connection.Cluster, connection.Database);
        return true;
    }

    private void OnDefaultsFileChanged()
    {
        if (!ApplyDefaultsFile())
            return;
        _ = RepublishAsync();
    }

    /// <summary>Analyses every open file again and asks for its lenses, for a change of defaults.</summary>
    private async Task RepublishAsync()
    {
        foreach (var (uri, document) in documents.ToArray())
        {
            try
            {
                await PublishDiagnosticsAsync(uri, document);
                LoadReferencedSchema(document);
            }
            catch (Exception exception) when (exception is IOException or ObjectDisposedException)
            {
                Console.Error.WriteLine($"Could not publish diagnostics for {uri}: {exception.Message}");
            }
        }
        await RequestCodeLensRefreshAsync();
        await RequestSemanticTokensRefreshAsync();
    }

    private void OnRunLogChanged()
    {
        _ = RequestCodeLensRefreshAsync();
        UpdateSpinner();
    }

    /// <summary>
    /// While a query runs its lens shows a spinner and the elapsed time, which only moves if Zed
    /// is asked for the lenses again, so a timer asks four times a second until nothing runs.
    /// </summary>
    private void UpdateSpinner()
    {
        var anyRunning = runLog.Read().Values.Any(runs => runs.Running is not null);
        lock (spinnerGate)
        {
            if (anyRunning && spinner is null)
            {
                spinner = new Timer(_ => OnSpinnerTick(), null, 250, 250);
            }
            else if (!anyRunning && spinner is not null)
            {
                spinner.Dispose();
                spinner = null;
            }
        }
    }

    private int spinnerTicks;

    private void OnSpinnerTick()
    {
        _ = RequestCodeLensRefreshAsync();
        // A run that never reported an end stops counting as running, which no file change announces.
        if (Interlocked.Increment(ref spinnerTicks) % 20 == 0)
            UpdateSpinner();
    }

    private Task RequestCodeLensRefreshAsync() => RequestRefreshAsync("workspace/codeLens/refresh");

    /// <summary>What a name is, and so its colour, depends on the schema, so the colours are asked for again when it changes.</summary>
    private Task RequestSemanticTokensRefreshAsync() => RequestRefreshAsync("workspace/semanticTokens/refresh");

    private async Task RequestRefreshAsync(string method)
    {
        try
        {
            await SendAsync(new
            {
                jsonrpc = "2.0",
                id = Interlocked.Increment(ref nextRequestId),
                method
            });
        }
        catch (Exception exception) when (exception is IOException or ObjectDisposedException)
        {
            Console.Error.WriteLine($"Could not ask for {method}: {exception.Message}");
        }
    }

    /// <summary>The cluster and database queries run on, from `lsp.kusto-lsp.initialization_options`.</summary>
    private void ApplyInitializationOptions(JsonElement parameters)
    {
        if (!parameters.TryGetProperty("initializationOptions", out var options)
            || options.ValueKind != JsonValueKind.Object)
            return;

        string? Text(string name) =>
            options.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
                ? value.GetString()
                : null;
        if (options.TryGetProperty("schemaCacheMinutes", out var minutes) && minutes.TryGetInt32(out var value))
            schema.CacheMinutes = Math.Max(0, value);
        if (options.TryGetProperty("schemaDiagnostics", out var checkNames)
            && checkNames.ValueKind is JsonValueKind.True or JsonValueKind.False)
            schemaDiagnostics = checkNames.GetBoolean();
        schema.SetDefaults(Text("cluster"), Text("database"));
    }

    /// <summary>Starts fetching the schema of every cluster and database the query mentions.</summary>
    private void LoadReferencedSchema(DocumentSnapshot document)
    {
        foreach (var block in QueryBlocks.Find(document.Text).Where(block => block.IsQuery))
        {
            // Where the query runs, from the file or the defaults, and anything it names itself.
            var connection = ConnectionDirectives.Before(document.Text, block.End, schema.Defaults);
            if (connection.Cluster is not null)
                schema.EnsureReference(connection.Cluster, connection.Database);

            var service = new KustoCodeService(block.Text, schema.GlobalsFor(connection));
            foreach (var reference in service.GetClusterReferences())
                schema.EnsureReference(reference.Cluster ?? connection.Cluster, null);
            foreach (var reference in service.GetDatabaseReferences())
                schema.EnsureReference(reference.Cluster ?? connection.Cluster, reference.Database);
        }
    }

    /// <summary>The symbols for the query at a position, with where that query runs applied.</summary>
    private GlobalState GlobalsAt(DocumentSnapshot document, int offset) =>
        schema.GlobalsFor(ConnectionDirectives.At(document.Text, offset, schema.Defaults));

    /// <summary>On a `// :` line, the directives the editor knows, with the cursor placed inside the quotes.</summary>
    private object[]? CompleteDirective(DocumentSnapshot document, int offset)
    {
        var lineStart = offset == 0 ? 0 : document.Text.LastIndexOf('\n', offset - 1) + 1;
        var typed = DirectivePrefix().Match(document.Text[lineStart..offset]);
        if (!typed.Success)
            return null;

        var range = new
        {
            start = document.PositionAt(offset - typed.Groups[1].Length),
            end = document.PositionAt(offset)
        };
        return new (string Name, string Detail)[]
        {
            (ConnectionDirectives.SetCluster, "The cluster for the queries below"),
            (ConnectionDirectives.SetDatabase, "The database for the queries below")
        }
        .Select(directive => (object)new
        {
            label = directive.Name,
            kind = 3,
            detail = directive.Detail,
            filterText = directive.Name,
            insertTextFormat = 2,
            textEdit = new { range, newText = $"{directive.Name}(\"$0\")" }
        })
        .ToArray();
    }

    [System.Text.RegularExpressions.GeneratedRegex(@"^\s*//\s*:\s*([A-Za-z]*)$")]
    private static partial System.Text.RegularExpressions.Regex DirectivePrefix();

    private bool TryGetDocumentAndPosition(
        JsonElement parameters,
        out DocumentSnapshot document,
        out int offset)
    {
        var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString();
        if (uri is null || !documents.TryGetValue(uri, out document!))
        {
            document = null!;
            offset = 0;
            return false;
        }

        var position = parameters.GetProperty("position");
        offset = document.OffsetAt(
            position.GetProperty("line").GetInt32(),
            position.GetProperty("character").GetInt32());
        return true;
    }

    private async Task RepublishDiagnosticsAsync()
    {
        foreach (var (uri, document) in documents.ToArray())
        {
            try
            {
                await PublishDiagnosticsAsync(uri, document);
            }
            catch (Exception exception) when (exception is IOException or ObjectDisposedException)
            {
                Console.Error.WriteLine($"Could not publish diagnostics for {uri}: {exception.Message}");
            }
        }
    }

    private async Task PublishDiagnosticsAsync(string uri, DocumentSnapshot document)
    {
        var diagnostics = QueryBlocks.Find(document.Text)
            .SelectMany(block => DiagnosticsOf(document, block))
            .Concat(ConnectionDirectives.Problems(document.Text).Select(problem => (object)new
            {
                range = new
                {
                    start = new { line = problem.Line, character = 0 },
                    end = new { line = problem.Line, character = problem.Length }
                },
                severity = 2,
                code = "directive",
                source = "Kusto",
                message = "Not a directive the editor knows. Use // :setDefaultCluster(\"…\") or // :setDefaultDb(\"…\")."
            }))
            .ToArray();

        await SendAsync(new
        {
            jsonrpc = "2.0",
            method = "textDocument/publishDiagnostics",
            @params = new { uri, diagnostics }
        });
    }

    /// <summary>
    /// What is wrong with one query: its syntax, and, when the schema of everything it refers to
    /// has arrived, the names that are not in it. A name is never called wrong while the schema
    /// that would have it may still be on its way.
    /// </summary>
    private IEnumerable<object> DiagnosticsOf(DocumentSnapshot document, QueryBlock block)
    {
        if (!block.IsQuery)
            return [];
        var connection = ConnectionDirectives.Before(document.Text, block.End, schema.Defaults);
        var globals = schema.GlobalsFor(connection);
        var checkNames = schemaDiagnostics && !QueryBlocks.IsControlCommand(block.Text) && SchemaIsLoaded(block, connection, globals);
        var code = checkNames ? KustoCode.ParseAndAnalyze(block.Text, globals) : KustoCode.Parse(block.Text);
        var found = (checkNames ? code.GetDiagnostics() : code.GetSyntaxDiagnostics())
            .Where(diagnostic => diagnostic.HasLocation)
            .ToList();
        if (checkNames)
            found = KnockOnErrors.Without(code, found);
        return found
            .Select(diagnostic => (object)new
            {
                range = new
                {
                    start = document.PositionAt(block.Start + diagnostic.Start),
                    end = document.PositionAt(block.Start + diagnostic.Start + diagnostic.Length)
                },
                severity = diagnostic.Severity.ToString() switch
                {
                    "Warning" => 2,
                    "Information" => 3,
                    "Suggestion" => 4,
                    _ => 1
                },
                code = diagnostic.Code,
                source = "Kusto",
                message = diagnostic.Message
            })
            .ToList();
    }

    /// <summary>Whether the schema of the query's own database and of every cluster and database it names has arrived.</summary>
    private bool SchemaIsLoaded(QueryBlock block, Connection connection, GlobalState globals)
    {
        if (connection.Cluster is null || connection.Database is null
            || !schema.IsLoaded(connection.Cluster, connection.Database))
            return false;
        var service = new KustoCodeService(block.Text, globals);
        return service.GetClusterReferences().All(reference => schema.IsLoaded(reference.Cluster ?? connection.Cluster, null))
            && service.GetDatabaseReferences().All(reference =>
                schema.IsLoaded(reference.Cluster ?? connection.Cluster, reference.Database));
    }

    private static string EscapeSnippet(string text) =>
        text.Replace("\\", "\\\\").Replace("$", "\\$").Replace("}", "\\}");

    private static int CompletionKind(string kind) => kind switch
    {
        "Keyword" or "QueryPrefix" or "TabularPrefix" or "TabularSuffix" => 14,
        "BuiltInFunction" or "LocalFunction" or "DatabaseFunction" or "AggregateFunction" => 3,
        "Column" => 5,
        "Table" or "MaterializedView" => 6,
        "Variable" or "Parameter" => 6,
        "ScalarType" => 7,
        "Database" or "Cluster" => 9,
        _ => 1
    };

    private static GlobalState LoadGlobals(JsonElement parameters)
    {
        string? rootUriText = null;
        if (parameters.TryGetProperty("rootUri", out var rootUriElement)
            && rootUriElement.ValueKind == JsonValueKind.String)
            rootUriText = rootUriElement.GetString();
        else if (parameters.TryGetProperty("workspaceFolders", out var folders)
            && folders.ValueKind == JsonValueKind.Array
            && folders.GetArrayLength() > 0)
            rootUriText = folders[0].GetProperty("uri").GetString();

        if (!Uri.TryCreate(rootUriText, UriKind.Absolute, out var rootUri)
            || !rootUri.IsFile)
            return GlobalState.Default;

        var path = Path.Combine(rootUri.LocalPath, ".kusto-schema.json");
        if (!File.Exists(path))
            return GlobalState.Default;

        try
        {
            using var schema = JsonDocument.Parse(File.ReadAllText(path));
            var databaseName = schema.RootElement.GetProperty("database").GetString();
            if (string.IsNullOrWhiteSpace(databaseName))
                throw new InvalidDataException("Schema database name is empty");

            var tables = new List<TableSymbol>();
            foreach (var table in schema.RootElement.GetProperty("tables").EnumerateObject())
            {
                var columns = new List<ColumnSymbol>();
                foreach (var column in table.Value.EnumerateObject())
                {
                    var typeName = column.Value.GetString();
                    var type = ScalarTypes.GetSymbol(typeName);
                    if (type is null)
                        throw new InvalidDataException($"Unknown type '{typeName}' for {table.Name}.{column.Name}");
                    columns.Add(new ColumnSymbol(column.Name, type));
                }
                tables.Add(new TableSymbol(table.Name, columns));
            }

            return GlobalState.Default.WithDatabase(new DatabaseSymbol(databaseName, tables));
        }
        catch (Exception exception)
        {
            Console.Error.WriteLine($"Could not load Kusto schema from {path}: {exception.Message}");
            return GlobalState.Default;
        }
    }

    private async Task SendAsync(object message)
    {
        var body = JsonSerializer.SerializeToUtf8Bytes(message);
        var header = Encoding.ASCII.GetBytes($"Content-Length: {body.Length}\r\n\r\n");
        await writeLock.WaitAsync();
        try
        {
            await output.WriteAsync(header);
            await output.WriteAsync(body);
            await output.FlushAsync();
        }
        finally
        {
            writeLock.Release();
        }
    }

    private static async Task<JsonDocument?> ReadMessageAsync(Stream stream)
    {
        var length = -1;
        while (true)
        {
            var line = await ReadLineAsync(stream);
            if (line is null)
                return null;
            if (line.Length == 0)
                break;
            if (line.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase))
                length = int.Parse(line["Content-Length:".Length..].Trim());
        }

        if (length < 0 || length > 16 * 1024 * 1024)
            throw new InvalidDataException("Invalid LSP message length");

        var body = new byte[length];
        await stream.ReadExactlyAsync(body);
        return JsonDocument.Parse(body);
    }

    private static async Task<string?> ReadLineAsync(Stream stream)
    {
        using var buffer = new MemoryStream();
        var currentByte = new byte[1];
        while (await stream.ReadAsync(currentByte) != 0)
        {
            if (currentByte[0] == '\n')
                return Encoding.ASCII.GetString(buffer.ToArray()).TrimEnd('\r');
            buffer.WriteByte(currentByte[0]);
            if (buffer.Length > 8192)
                throw new InvalidDataException("LSP header line too long");
        }
        return null;
    }
}

internal sealed class DocumentSnapshot
{
    private readonly List<int> lineStarts = [0];

    public DocumentSnapshot(string text)
    {
        Text = text;
        for (var index = 0; index < text.Length; index++)
        {
            if (text[index] == '\n')
                lineStarts.Add(index + 1);
        }
    }

    public string Text { get; }

    public int OffsetAt(int line, int character)
    {
        if (line < 0 || line >= lineStarts.Count)
            return Text.Length;

        var lineStart = lineStarts[line];
        var lineEnd = line + 1 < lineStarts.Count ? lineStarts[line + 1] - 1 : Text.Length;
        if (lineEnd > lineStart && Text[lineEnd - 1] == '\r')
            lineEnd--;
        return Math.Clamp(lineStart + Math.Max(character, 0), lineStart, lineEnd);
    }

    public object PositionAt(int offset)
    {
        var (line, character) = LineAndColumn(offset);
        return new { line, character };
    }

    /// <summary>The line and the UTF-16 column of an offset, as LSP counts them.</summary>
    public (int Line, int Character) LineAndColumn(int offset)
    {
        offset = Math.Clamp(offset, 0, Text.Length);
        var line = lineStarts.BinarySearch(offset);
        if (line < 0)
            line = ~line - 1;
        return (line, offset - lineStarts[line]);
    }
}
