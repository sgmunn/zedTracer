using System.Text;
using System.Text.Json;
using Kusto.Language;

/// <summary>
/// What the editor recorded about the queries it ran, read from `runs.jsonl` in Zed's Kusto
/// history folder. The editor appends a line when a run starts and when it ends; the language
/// server only reads, so the two never have to talk to each other directly.
/// </summary>
internal sealed class RunLog : IDisposable
{
    /// <summary>A run that never reported an end, such as one whose editor was closed, stops counting as running.</summary>
    private static readonly TimeSpan StaleRunningAfter = TimeSpan.FromMinutes(15);

    private const int MaxBytesRead = 512 * 1024;

    private readonly string? path;
    private FileSystemWatcher? watcher;
    private Timer? debounce;

    public RunLog(string? dataDirectory)
    {
        path = string.IsNullOrWhiteSpace(dataDirectory)
            ? null
            : Path.Combine(dataDirectory, "kusto", "history", "runs.jsonl");
    }

    /// <summary>Raised, after a short quiet period, when the log changes.</summary>
    public event Action? Changed;

    public void Watch()
    {
        if (path is null || watcher is not null)
            return;
        try
        {
            var directory = Path.GetDirectoryName(path)!;
            Directory.CreateDirectory(directory);
            watcher = new FileSystemWatcher(directory, Path.GetFileName(path))
            {
                NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.Size | NotifyFilters.FileName,
                EnableRaisingEvents = true
            };
            void OnChange(object? sender, FileSystemEventArgs e) =>
                (debounce ??= new Timer(_ => Changed?.Invoke())).Change(150, Timeout.Infinite);
            watcher.Changed += OnChange;
            watcher.Created += OnChange;
            watcher.Renamed += (sender, e) => OnChange(sender, e);
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
        {
            Console.Error.WriteLine($"Could not watch {path}: {exception.Message}");
        }
    }

    public void Dispose()
    {
        watcher?.Dispose();
        debounce?.Dispose();
    }

    /// <summary>
    /// What names a query for the log: where it runs and its text without comments and layout,
    /// so the same text on another cluster or database is another query.
    /// </summary>
    public static string Key(string query, string? cluster, string? database) =>
        string.Join("|", cluster is null ? "" : KustoRestClient.ClusterHost(cluster), database ?? "", Normalize(query));

    /// <summary>The latest news about each query, keyed by [Key].</summary>
    public Dictionary<string, QueryRuns> Read()
    {
        var result = new Dictionary<string, QueryRuns>(StringComparer.Ordinal);
        var queryOfRun = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var line in ReadTailLines())
        {
            JsonDocument document;
            try
            {
                document = JsonDocument.Parse(line);
            }
            catch (JsonException)
            {
                continue;
            }

            using (document)
            {
                var root = document.RootElement;
                var runId = Text(root, "cid");
                var query = Text(root, "query");
                if (runId is null || query is null)
                    continue;
                var key = Key(query, Text(root, "cluster"), Text(root, "database"));
                queryOfRun[runId] = key;
                if (!result.TryGetValue(key, out var runs))
                    result[key] = runs = new QueryRuns();

                var startedAt = DateTimeOffset.TryParse(Text(root, "at"), out var parsed) ? parsed : (DateTimeOffset?)null;
                switch (Text(root, "event"))
                {
                    case "started":
                        runs.Running = new RunningRun(runId, startedAt);
                        break;
                    case "finished":
                        runs.ClearRunning(runId);
                        runs.Last = new FinishedRun(
                            runId,
                            startedAt,
                            root.TryGetProperty("durationMs", out var duration) && duration.TryGetInt64(out var milliseconds) ? milliseconds : null,
                            root.TryGetProperty("rows", out var rows) && rows.TryGetInt64(out var rowCount) ? rowCount : null,
                            Text(root, "path"),
                            null);
                        break;
                    case "failed":
                        runs.ClearRunning(runId);
                        runs.Last = new FinishedRun(runId, startedAt, null, null, null, Text(root, "message") ?? "The query failed.");
                        break;
                    case "cancelled":
                        runs.ClearRunning(runId);
                        break;
                }
            }
        }

        foreach (var runs in result.Values)
        {
            if (runs.Running is { StartedAt: { } startedAt } && DateTimeOffset.UtcNow - startedAt > StaleRunningAfter)
                runs.Running = null;
        }
        return result;
    }

    private IEnumerable<string> ReadTailLines()
    {
        if (path is null || !File.Exists(path))
            return [];
        try
        {
            using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            var skip = Math.Max(0, stream.Length - MaxBytesRead);
            stream.Seek(skip, SeekOrigin.Begin);
            using var reader = new StreamReader(stream, Encoding.UTF8);
            var text = reader.ReadToEnd();
            var lines = text.Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            // After a seek into the middle of the file the first line is probably cut off.
            return skip > 0 ? lines.Skip(1) : lines;
        }
        catch (IOException exception)
        {
            Console.Error.WriteLine($"Could not read {path}: {exception.Message}");
            return [];
        }
    }

    private static string? Text(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    /// <summary>
    /// The text of a query without its comments and layout, so a query that was only reformatted
    /// or annotated still matches its earlier run.
    /// </summary>
    public static string Normalize(string query) =>
        string.Join(" ", KustoCode.Parse(query).GetLexicalTokens().Select(token => token.Text).Where(text => text.Length > 0));
}

internal sealed class QueryRuns
{
    public RunningRun? Running { get; set; }
    public FinishedRun? Last { get; set; }

    public void ClearRunning(string runId)
    {
        if (Running?.RunId == runId)
            Running = null;
    }
}

internal sealed record RunningRun(string RunId, DateTimeOffset? StartedAt);

internal sealed record FinishedRun(
    string RunId,
    DateTimeOffset? StartedAt,
    long? DurationMilliseconds,
    long? Rows,
    string? ResultPath,
    string? Failure);
