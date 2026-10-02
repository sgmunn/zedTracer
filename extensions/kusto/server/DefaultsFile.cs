using System.Text.Json;

/// <summary>
/// The cluster and database the editor runs queries on when a file does not say, which it writes
/// from its settings to `defaults.json` in Zed's Kusto folder. Following the file means the
/// settings are the only place they are written, and a change applies without a restart.
/// </summary>
internal sealed class DefaultsFile : IDisposable
{
    private readonly string? path;
    private readonly FileChangeWatcher watcher;

    public DefaultsFile(string? dataDirectory)
    {
        path = string.IsNullOrWhiteSpace(dataDirectory)
            ? null
            : Path.Combine(dataDirectory, "kusto", "defaults.json");
        watcher = new FileChangeWatcher(path);
    }

    public event Action Changed
    {
        add => watcher.Changed += value;
        remove => watcher.Changed -= value;
    }

    public void Watch() => watcher.Start();

    public void Dispose() => watcher.Dispose();

    /// <summary>What the file says, or null when there is no readable file, so that whatever else configured the defaults stands.</summary>
    public Connection? Read()
    {
        if (path is null)
            return null;
        try
        {
            using var document = JsonDocument.Parse(File.ReadAllText(path));
            string? Text(string name) =>
                document.RootElement.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
                    ? value.GetString()
                    : null;
            return new Connection(Text("cluster"), Text("database"));
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException
                                              or JsonException or InvalidOperationException)
        {
            return null;
        }
    }
}
