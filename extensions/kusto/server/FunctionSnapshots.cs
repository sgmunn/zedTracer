using System.Text;
using Kusto.Language;

/// <summary>One function of a database as `.show functions` describes it.</summary>
internal sealed record StoredFunction(string Name, string Parameters, string Body, string? Folder, string? DocString);

/// <summary>What writing a snapshot did. `Stale` are the files that no function of the database has any more.</summary>
internal sealed record SnapshotResult(string Directory, int Created, int Changed, int Unchanged, IReadOnlyList<string> Stale);

/// <summary>
/// The functions of a database kept as files, one `.create-or-alter function` command each, so that a
/// project can keep them in git and see what changed on the cluster, and so that go to definition has a
/// file to go to. A file is laid out as
/// `&lt;root&gt;/&lt;cluster&gt;/&lt;database&gt;/functions/&lt;folder of the function&gt;/&lt;name&gt;.kql` and is written the same way every
/// time, with nothing in it that changes between runs, so that a difference is a difference in the function.
/// </summary>
internal static class FunctionSnapshots
{
    /// <summary>Where snapshots go in the project when the `snapshotDirectory` option does not say.</summary>
    public const string DefaultFolder = "kusto-schema";

    public const string NoProjectMessage =
        "There is no project open to put the snapshot of the functions in. Set `snapshotDirectory` in "
        + "`lsp.kusto-lsp.initialization_options` to a full path.";

    /// <summary>
    /// The folder snapshots are kept in. A full path in the option is used as it is, project or no project.
    /// A relative one, or the default when the option is empty, is relative to the project that holds the
    /// document, which is why there must be one. A leading `~` is the home folder.
    /// </summary>
    public static (string? Root, string? Problem) Root(
        string? configured, string? documentPath, IReadOnlyList<string> workspaceFolders)
    {
        var setting = string.IsNullOrWhiteSpace(configured) ? null : ExpandHome(configured.Trim());
        if (setting is not null && Path.IsPathRooted(setting))
            return (Path.GetFullPath(setting), null);
        return QueryParameters.FolderOf(documentPath, workspaceFolders) is { } project
            ? (Path.GetFullPath(Path.Combine(project, setting ?? DefaultFolder)), null)
            : (null, NoProjectMessage);
    }

    private static string ExpandHome(string path)
    {
        if (path != "~" && !path.StartsWith("~/", StringComparison.Ordinal))
            return path;
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        return path.Length == 1 ? home : Path.Combine(home, path[2..]);
    }

    public static string DirectoryOf(string root, string host, string database) =>
        Path.Combine(root, Escape(host), Escape(database), "functions");

    public static string PathOf(string root, string host, string database, StoredFunction function)
    {
        var parts = new List<string> { DirectoryOf(root, host, database) };
        // A folder is a free string and people write it with either slash.
        parts.AddRange((function.Folder ?? "")
            .Split('/', '\\')
            .Select(part => part.Trim())
            .Where(part => part.Length > 0 && part is not "." and not "..")
            .Select(Escape));
        parts.Add(Escape(function.Name) + ".kql");
        return Path.Combine(parts.ToArray());
    }

    /// <summary>
    /// A name that can be one segment of a path whatever is in it, written the way the schema cache writes
    /// names. A name that would be a way up the tree is not left as it is.
    /// </summary>
    private static string Escape(string name)
    {
        var escaped = Uri.EscapeDataString(name);
        return escaped is "" or "." or ".." ? "_" + escaped.Replace(".", "%2E") : escaped;
    }

    /// <summary>
    /// The file for a function. It starts with the directives that say where the command runs, so that
    /// running the file deploys the function to the database it was read from and not to whatever the
    /// window's defaults are. The body is as the cluster has it, with the line ends of a file.
    /// </summary>
    public static string Script(string host, string database, StoredFunction function)
    {
        var properties = new List<string>();
        if (!string.IsNullOrWhiteSpace(function.DocString))
            properties.Add("docstring = " + KustoFacts.GetStringLiteral(function.DocString));
        if (!string.IsNullOrWhiteSpace(function.Folder))
            properties.Add("folder = " + KustoFacts.GetStringLiteral(function.Folder));

        var parameters = string.IsNullOrWhiteSpace(function.Parameters) ? "()" : function.Parameters.Trim();
        var body = function.Body.Replace("\r\n", "\n").Replace('\r', '\n').Trim();
        var script = new StringBuilder();
        script.Append("// :setDefaultCluster(").Append(DirectiveArgument(host)).Append(")\n");
        script.Append("// :setDefaultDb(").Append(DirectiveArgument(database)).Append(")\n");
        script.Append(".create-or-alter function");
        if (properties.Count > 0)
            script.Append(" with (").Append(string.Join(", ", properties)).Append(')');
        script.Append('\n').Append(KustoFacts.BracketNameIfNecessary(function.Name)).Append(parameters).Append('\n');
        script.Append(body).Append('\n');
        return script.ToString();
    }

    /// <summary>A directive's argument has no escapes, so a value with double quotes in it is written in single quotes.</summary>
    private static string DirectiveArgument(string value) =>
        value.Contains('"') ? $"'{value}'" : $"\"{value}\"";

    /// <summary>The line of a file where the function's name is, or the first when the file has been changed past recognition.</summary>
    public static int NameLine(string text, string name)
    {
        var lines = text.Split('\n');
        var bracketed = KustoFacts.BracketNameIfNecessary(name);
        for (var line = 0; line < lines.Length; line++)
        {
            if (lines[line].StartsWith(bracketed + "(", StringComparison.Ordinal))
                return line;
        }
        return 0;
    }

    /// <summary>
    /// Writes the files of a database's functions. A file that is already there is replaced when
    /// <paramref name="overwrite"/> is set and otherwise left as it is, so that going to a definition
    /// never takes away what someone changed in a file. A file is written only when its text differs, so
    /// the files of functions that did not change keep their time.
    /// </summary>
    public static SnapshotResult Write(
        string root, string host, string database, IReadOnlyList<StoredFunction> functions, bool overwrite)
    {
        var created = 0;
        var changed = 0;
        var unchanged = 0;
        var wanted = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var function in functions.OrderBy(function => function.Folder, StringComparer.Ordinal)
                     .ThenBy(function => function.Name, StringComparer.Ordinal))
        {
            var path = PathOf(root, host, database, function);
            wanted.Add(path);
            var text = Script(host, database, function);
            var exists = File.Exists(path);
            if (exists && (!overwrite || File.ReadAllText(path) == text))
            {
                unchanged++;
                continue;
            }
            WriteFile(path, text);
            if (exists)
                changed++;
            else
                created++;
        }

        var directory = DirectoryOf(root, host, database);
        var stale = Directory.Exists(directory)
            ? Directory.EnumerateFiles(directory, "*.kql", SearchOption.AllDirectories)
                .Where(path => !wanted.Contains(path))
                .Select(path => Path.GetRelativePath(directory, path))
                .Order(StringComparer.Ordinal)
                .ToList()
            : [];
        return new SnapshotResult(directory, created, changed, unchanged, stale);
    }

    /// <summary>The file of a function, wherever its folder put it, or null when there is none.</summary>
    public static string? Find(string root, string host, string database, string name)
    {
        var directory = DirectoryOf(root, host, database);
        return Directory.Exists(directory)
            ? Directory.EnumerateFiles(directory, Escape(name) + ".kql", SearchOption.AllDirectories).FirstOrDefault()
            : null;
    }

    /// <summary>Written to a new file that then replaces the old one, so that an editor watching the file never reads half of it.</summary>
    private static void WriteFile(string path, string text)
    {
        var temporary = $"{path}.{Guid.NewGuid():N}.tmp";
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(temporary, text, new UTF8Encoding(false));
            File.Move(temporary, path, overwrite: true);
        }
        catch
        {
            if (File.Exists(temporary))
                File.Delete(temporary);
            throw;
        }
    }
}
