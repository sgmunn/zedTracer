/// <summary>
/// Tells when one file the editor writes changes, once the changes have gone quiet. The editor
/// and the server share files instead of talking to each other.
/// </summary>
internal sealed class FileChangeWatcher(string? path) : IDisposable
{
    private FileSystemWatcher? watcher;
    private Timer? debounce;

    public event Action? Changed;

    public void Start()
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
}
