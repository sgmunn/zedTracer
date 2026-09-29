using System.Text;
using System.Text.Json;
using Kusto.Language;
using Kusto.Language.Editor;
using Kusto.Language.Symbols;

await new KustoLanguageServer(Console.OpenStandardInput(), Console.OpenStandardOutput()).RunAsync();

internal sealed class KustoLanguageServer(Stream input, Stream output)
{
    private readonly Dictionary<string, DocumentSnapshot> documents = new(StringComparer.Ordinal);
    private GlobalState globals = GlobalState.Default;

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
                globals = LoadGlobals(parameters);
                return new
                {
                    capabilities = new
                    {
                        textDocumentSync = 1,
                        completionProvider = new { triggerCharacters = new[] { "|", ".", "(" } },
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
            default:
                return null;
        }
    }

    private object[] Complete(JsonElement parameters)
    {
        if (!TryGetDocumentAndPosition(parameters, out var document, out var offset))
            return [];

        var completions = new KustoCodeService(document.Text, globals)
            .GetCompletionItems(offset);
        var start = Math.Clamp(completions.EditStart, 0, document.Text.Length);
        var end = Math.Clamp(start + completions.EditLength, start, document.Text.Length);
        var range = new { start = document.PositionAt(start), end = document.PositionAt(end) };

        return completions.Items
            .Select(item =>
            {
                var insertedText = string.Concat(item.ApplyTexts.Select(part => part.Text));
                return (object)new
                {
                    label = item.DisplayText,
                    kind = CompletionKind(item.Kind.ToString()),
                    sortText = item.OrderText,
                    filterText = item.MatchText,
                    textEdit = new { range, newText = insertedText }
                };
            })
            .ToArray();
    }

    private object? Hover(JsonElement parameters)
    {
        if (!TryGetDocumentAndPosition(parameters, out var document, out var offset))
            return null;

        var info = new KustoCodeService(document.Text, globals).GetQuickInfo(offset);
        return string.IsNullOrWhiteSpace(info.Text)
            ? null
            : new { contents = new { kind = "plaintext", value = info.Text } };
    }

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
        var diagnostics = KustoCode.Parse(document.Text)
            .GetSyntaxDiagnostics()
            .Where(diagnostic => diagnostic.HasLocation)
            .Select(diagnostic => new
            {
                range = new
                {
                    start = document.PositionAt(diagnostic.Start),
                    end = document.PositionAt(diagnostic.Start + diagnostic.Length)
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
            .ToArray();

        await SendAsync(new
        {
            jsonrpc = "2.0",
            method = "textDocument/publishDiagnostics",
            @params = new { uri, diagnostics }
        });
    }

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
        await output.WriteAsync(header);
        await output.WriteAsync(body);
        await output.FlushAsync();
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
