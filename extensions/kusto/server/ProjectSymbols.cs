/// <summary>One symbol of a file of queries, placed by the line and column of its name.</summary>
internal sealed record FileSymbol(string Name, int Kind, int Line, int Character, int EndLine, int EndCharacter);

/// <summary>
/// The symbols of every `.kql` file in the open projects (see <see cref="Outline"/> for what a file's symbols
/// are), so that the editor's project symbols can find a query by its label in a file that is not open. A
/// file is read again only when its time of modification or its size is not what it was, and a file that is
/// open is read from the editor, so what has not been saved is found too. The folders of snapshots are left
/// out, since their functions are listed from the snapshot (<see cref="FunctionSnapshots.Files"/>), and so are
/// the folders a project keeps what it builds or what tools write in: those that start with a dot, and a few
/// that are well known. Folders and files past a limit are not looked at, so a project folder that is a
/// whole disk does not make every search wait. The list of files of a folder is kept for a few seconds,
/// because a search is asked again with every letter typed, so a file made a moment ago may not be found yet.
/// </summary>
internal sealed class ProjectSymbols
{
    private const long MaxFileBytes = 1_000_000;
    private const int MaxFiles = 5000;
    private const int MaxFolders = 20000;

    private static readonly HashSet<string> SkippedFolders = new(StringComparer.Ordinal)
    {
        "bin", "obj", "node_modules", "target"
    };

    private static readonly TimeSpan ListingLifetime = TimeSpan.FromSeconds(5);

    private readonly Dictionary<string, Entry> entries = new(StringComparer.Ordinal);
    private readonly Dictionary<string, (DateTime At, List<string> Files)> listings = new(StringComparer.Ordinal);

    private sealed record Entry(DateTime Modified, long Length, IReadOnlyList<FileSymbol> Symbols);

    /// <summary>
    /// Every symbol of every file, with the file and where it is called from: the file's path from its project
    /// folder, or its name when it is in none. <paramref name="openDocuments"/> are the open files by path.
    /// </summary>
    public List<(string Path, string Container, FileSymbol Symbol)> All(
        IReadOnlyList<string> workspaceFolders,
        IReadOnlyList<string> snapshotRoots,
        IReadOnlyDictionary<string, DocumentSnapshot> openDocuments)
    {
        var found = new List<(string, string, FileSymbol)>();
        var seen = new HashSet<string>(StringComparer.Ordinal);
        foreach (var folder in workspaceFolders)
        {
            foreach (var path in ListingOf(folder, snapshotRoots))
            {
                if (!seen.Add(path))
                    continue;
                var container = Path.GetRelativePath(folder, path).Replace(Path.DirectorySeparatorChar, '/');
                foreach (var symbol in SymbolsOf(path, openDocuments))
                    found.Add((path, container, symbol));
            }
        }

        // A file that is open but is in none of the folders, such as a query thread's.
        foreach (var (path, document) in openDocuments)
        {
            if (!path.EndsWith(".kql", StringComparison.OrdinalIgnoreCase) || IsUnder(path, snapshotRoots) || !seen.Add(path))
                continue;
            foreach (var symbol in Symbols(document.Text))
                found.Add((path, Path.GetFileName(path), symbol));
        }

        foreach (var path in entries.Keys.Where(path => !seen.Contains(path)).ToList())
            entries.Remove(path);
        return found;
    }

    private List<string> ListingOf(string folder, IReadOnlyList<string> snapshotRoots)
    {
        var key = folder + "\n" + string.Join('\n', snapshotRoots);
        if (listings.TryGetValue(key, out var listing) && DateTime.UtcNow - listing.At < ListingLifetime)
            return listing.Files;
        var files = FilesIn(folder, snapshotRoots).Where(path => !IsUnder(path, snapshotRoots)).ToList();
        listings[key] = (DateTime.UtcNow, files);
        return files;
    }

    private IReadOnlyList<FileSymbol> SymbolsOf(string path, IReadOnlyDictionary<string, DocumentSnapshot> openDocuments)
    {
        if (openDocuments.TryGetValue(path, out var open))
            return Symbols(open.Text);
        try
        {
            var info = new FileInfo(path);
            if (info.Length > MaxFileBytes)
                return [];
            if (entries.TryGetValue(path, out var entry) && entry.Modified == info.LastWriteTimeUtc && entry.Length == info.Length)
                return entry.Symbols;
            var symbols = Symbols(File.ReadAllText(path));
            entries[path] = new Entry(info.LastWriteTimeUtc, info.Length, symbols);
            return symbols;
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
        {
            entries.Remove(path);
            return [];
        }
    }

    private static List<FileSymbol> Symbols(string text)
    {
        var document = new DocumentSnapshot(text);
        return Outline.Of(text)
            .Select(symbol =>
            {
                var (line, character) = document.LineAndColumn(symbol.NameStart);
                var (endLine, endCharacter) = document.LineAndColumn(symbol.NameEnd);
                return new FileSymbol(symbol.Name, symbol.Kind, line, character, endLine, endCharacter);
            })
            .ToList();
    }

    private static IEnumerable<string> FilesIn(string root, IReadOnlyList<string> snapshotRoots)
    {
        var folders = new Stack<string>();
        folders.Push(root);
        var visited = 0;
        var files = 0;
        while (folders.Count > 0)
        {
            var folder = folders.Pop();
            if (++visited > MaxFolders)
            {
                Console.Error.WriteLine($"Stopped looking for .kql files in {root} after {MaxFolders} folders.");
                yield break;
            }
            string[] subfolders;
            string[] kqlFiles;
            try
            {
                subfolders = Directory.GetDirectories(folder);
                kqlFiles = Directory.GetFiles(folder, "*.kql");
            }
            catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
            {
                continue;
            }
            foreach (var file in kqlFiles.Order(StringComparer.Ordinal))
            {
                if (++files > MaxFiles)
                {
                    Console.Error.WriteLine($"Stopped looking for .kql files in {root} after {MaxFiles} files.");
                    yield break;
                }
                yield return file;
            }
            foreach (var subfolder in subfolders.OrderDescending(StringComparer.Ordinal))
            {
                var name = Path.GetFileName(subfolder);
                if (!name.StartsWith('.') && !SkippedFolders.Contains(name) && !IsUnder(subfolder, snapshotRoots))
                    folders.Push(subfolder);
            }
        }
    }

    private static bool IsUnder(string path, IReadOnlyList<string> roots) =>
        roots.Any(root => path == root || path.StartsWith(root.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar, StringComparison.Ordinal));
}
