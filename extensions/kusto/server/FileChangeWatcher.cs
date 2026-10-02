/// <summary>
/// Tells when one file the editor writes changes, once the changes have gone quiet. The editor
/// and the server share files instead of talking to each other.
/// </summary>
internal sealed class FileChangeWatcher(string? path) : IDisposable
{
    private FileSystemWatcher? watcher;
    private Timer? debounce;

    public event Action? Changed;

    /// <summary>
    /// Starts watching. A missing folder is made when the server owns it, and otherwise the file is
    /// not watched, which a later call can put right once the folder exists.
    /// </summary>
    public bool Start(bool createDirectory = true)
    {
        if (path is null)
            return false;
        if (watcher is not null)
            return true;
        try
        {
            var directory = Path.GetDirectoryName(path)!;
            if (createDirectory)
                Directory.CreateDirectory(directory);
            else if (!Directory.Exists(directory))
                return false;
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
            return true;
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
        {
            Console.Error.WriteLine($"Could not watch {path}: {exception.Message}");
            return false;
        }
    }

    public void Dispose()
    {
        watcher?.Dispose();
        debounce?.Dispose();
    }
}
