using System.Text;
using System.Text.Json;
using Kusto.Language;
using Kusto.Language.Editor;
using Kusto.Language.Symbols;

await new KustoLanguageServer(Console.OpenStandardInput(), Console.OpenStandardOutput()).RunAsync();

internal sealed partial class KustoLanguageServer(Stream input, Stream output)
{
    private readonly Dictionary<string, DocumentSnapshot> documents = new(StringComparer.Ordinal);
    private readonly SemaphoreSlim writeLock = new(1, 1);
    private SchemaManager schema = new(new KustoRestClient(), GlobalState.Default, new SchemaCache(null));
    private RunLog runLog = new(null);
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
                ApplyInitializationOptions(parameters);
                StartRunLog(dataDirectory);
                return new
                {
                    capabilities = new
                    {
                        textDocumentSync = 1,
                        completionProvider = new { triggerCharacters = new[] { "|", ".", "(", ":" } },
                        signatureHelpProvider = new { triggerCharacters = new[] { "(", "," } },
                        codeLensProvider = new { resolveProvider = false },
                        executeCommandProvider = new { commands = new[] { CodeLenses.NoopCommand, CodeLenses.ConnectionCommand } },
                        hoverProvider = true
                    },
                    serverInfo = new { name = "kusto-lsp", version = "0.1.0" }
                };
            case "initialized":
                return null;
            case "shutdown":
                return null;
            case "textDocument/didOpen":
            {
                var textDocument = parameters.GetProperty("textDocument");
                var uri = textDocument.GetProperty("uri").GetString()!;
                var document = new DocumentSnapshot(textDocument.GetProperty("text").GetString() ?? "");
                documents[uri] = document;
                await PublishDiagnosticsAsync(uri, document);
                LoadReferencedSchema(document);
                return null;
            }
            case "textDocument/didChange":
            {
                var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString()!;
                var changes = parameters.GetProperty("contentChanges");
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
                documents.Remove(uri);
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
            case "workspace/executeCommand":
                // The lenses that only show text name this command so that Zed treats them as clickable.
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

    private object[] GetCodeLenses(JsonElement parameters)
    {
        var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString();
        return uri is not null && documents.TryGetValue(uri, out var document)
            ? CodeLenses.For(document, runLog.Read(), DateTimeOffset.UtcNow, schema.Defaults)
            : [];
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

    private async Task RequestCodeLensRefreshAsync()
    {
        try
        {
            await SendAsync(new
            {
                jsonrpc = "2.0",
                id = Interlocked.Increment(ref nextRequestId),
                method = "workspace/codeLens/refresh"
            });
        }
        catch (Exception exception) when (exception is IOException or ObjectDisposedException)
        {
            Console.Error.WriteLine($"Could not ask for a code lens refresh: {exception.Message}");
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

    private async Task PublishDiagnosticsAsync(string uri, DocumentSnapshot document)
    {
        var diagnostics = QueryBlocks.Find(document.Text)
            .SelectMany(block => KustoCode.Parse(block.Text)
                .GetSyntaxDiagnostics()
                .Where(diagnostic => diagnostic.HasLocation)
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
                }))
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
        offset = Math.Clamp(offset, 0, Text.Length);
        var line = lineStarts.BinarySearch(offset);
        if (line < 0)
            line = ~line - 1;
        return new { line, character = offset - lineStarts[line] };
    }
}
