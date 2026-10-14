using System.Text.Json;
using Kusto.Language;
using Kusto.Language.Symbols;
using NameReference = Kusto.Language.Syntax.NameReference;

/// <summary>
/// Go to definition for the functions of the database: the name of a function the cluster stores opens
/// its file in the snapshot of the database's functions (see <see cref="FunctionSnapshots"/>). When the
/// snapshot does not have the function yet, the functions of the database are written first, which is
/// the only time going to a definition reaches the cluster. Functions the file defines, and the
/// language's own, have no definition to go to here.
/// </summary>
internal sealed partial class KustoLanguageServer
{
    /// <summary>The `snapshotDirectory` option: a full path, or a folder in the project.</summary>
    private string? snapshotDirectory;

    private readonly Dictionary<string, Task<SnapshotResult>> snapshotsInFlight = new(StringComparer.Ordinal);

    /// <summary>
    /// Answers a request for a definition when it has the answer, so that the others are not held up
    /// while the functions of a database are fetched. The parameters outlive the message they came in.
    /// </summary>
    private async Task AnswerDefinitionAsync(JsonElement id, JsonElement parameters)
    {
        try
        {
            var result = await DefinitionAsync(parameters);
            await SendAsync(new { jsonrpc = "2.0", id, result });
        }
        catch (Exception exception)
        {
            Console.Error.WriteLine($"textDocument/definition: {exception}");
            await SendAsync(new { jsonrpc = "2.0", id, error = new { code = -32603, message = exception.Message } });
        }
    }

    private async Task<object?> DefinitionAsync(JsonElement parameters)
    {
        if (!TryGetDocumentAndPosition(parameters, out var document, out var offset))
            return null;
        var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString();
        var state = ConnectionDirectives.StateAt(document.Text, offset, schema.Defaults);
        if (StoredFunctionAt(document.Text, offset, state, GlobalsOf(state)) is not { } stored)
            return null;
        var (name, host, database) = stored;

        var (root, problem) = FunctionSnapshots.Root(snapshotDirectory, DocumentPath(uri), workspaceFolders);
        if (root is null)
        {
            await ShowMessageAsync(2, problem!);
            return null;
        }

        var file = FunctionSnapshots.Find(root, host, database, name);
        if (file is null)
        {
            SnapshotResult snapshot;
            try
            {
                snapshot = await SnapshotAsync(root, host, database, overwrite: false);
            }
            catch (Exception exception) when (IsSnapshotFailure(exception))
            {
                await ShowMessageAsync(1, $"Could not write the snapshot of the functions of {host} / {database}: {exception.Message}");
                return null;
            }
            await ShowMessageAsync(3, Describe(snapshot, host, database));
            file = FunctionSnapshots.Find(root, host, database, name);
            if (file is null)
            {
                await ShowMessageAsync(2, $"{host} / {database} has no function {name} any more.");
                return null;
            }
        }

        var line = FunctionSnapshots.NameLine(await File.ReadAllTextAsync(file), name);
        return new[]
        {
            new
            {
                uri = new Uri(file).AbsoluteUri,
                range = new
                {
                    start = new { line, character = 0 },
                    end = new { line, character = 0 }
                }
            }
        };
    }

    /// <summary>
    /// Writes the functions of a database again, whatever the files hold, and says in a message in the
    /// editor what that did. Unlike going to a definition this is meant to take the files to what the
    /// cluster has, which is what makes a difference visible to git. Nulls mean the default cluster and database.
    /// </summary>
    private async Task SnapshotFunctionsAsync(string? cluster, string? database, string? documentUri)
    {
        var defaults = schema.Defaults;
        var connection = new Connection(
            string.IsNullOrWhiteSpace(cluster) ? defaults.Cluster : cluster,
            string.IsNullOrWhiteSpace(database) ? defaults.Database : database.Trim());
        if (connection.Host is not { } host || connection.Database is not { } name)
        {
            await ShowMessageAsync(2, "This query has no cluster and database to write the functions of.");
            return;
        }

        var (root, problem) = FunctionSnapshots.Root(snapshotDirectory, DocumentPath(documentUri), workspaceFolders);
        if (root is null)
        {
            await ShowMessageAsync(2, problem!);
            return;
        }

        try
        {
            var snapshot = await SnapshotAsync(root, host, name, overwrite: true);
            await ShowMessageAsync(3, Describe(snapshot, host, name));
        }
        catch (Exception exception) when (IsSnapshotFailure(exception))
        {
            Console.Error.WriteLine($"workspace/executeCommand {CodeLenses.SnapshotFunctionsCommand}: {exception}");
            await ShowMessageAsync(1, $"Could not write the functions of {host} / {name}: {exception.Message}");
        }
    }

    /// <summary>
    /// The function the cluster stores that a name at a position refers to, with the cluster and database
    /// that have it. The name may be the end of a word the cursor is just after.
    /// </summary>
    private static (string Name, string Host, string Database)? StoredFunctionAt(
        string text, int offset, DirectiveState state, GlobalState globals)
    {
        var (queryText, queryStart) = QueryBlocks.Around(text, offset);
        var code = KustoCode.ParseAndAnalyze(queryText, globals);
        foreach (var position in new[] { offset - queryStart, offset - queryStart - 1 })
        {
            // The token of a name is wrapped in a node of its own, which the reference is the parent of.
            var wrapper = position < 0 ? null : code.Syntax.GetTokenAt(position)?.Parent;
            if ((wrapper as NameReference ?? wrapper?.Parent as NameReference)?.ReferencedSymbol is not FunctionSymbol function)
                continue;
            // A definition in the file hides the stored function of the same name.
            if (state.Functions.Any(definition => definition.Name == function.Name))
                return null;
            foreach (var cluster in globals.Clusters)
            {
                foreach (var database in cluster.Databases)
                {
                    if (database.Members.Contains(function))
                        return (function.Name, cluster.Name, database.Name);
                }
            }
            return null;
        }
        return null;
    }

    /// <summary>
    /// Writes the functions of a database, once at a time for each place: a second request for the same
    /// snapshot while one is being written waits for it and gets its result.
    /// </summary>
    private async Task<SnapshotResult> SnapshotAsync(string root, string host, string database, bool overwrite)
    {
        var key = $"{root}|{host}|{database}|{overwrite}";
        Task<SnapshotResult> task;
        lock (snapshotsInFlight)
        {
            if (!snapshotsInFlight.TryGetValue(key, out task!))
                snapshotsInFlight[key] = task = WriteSnapshotAsync(root, host, database, overwrite);
        }
        try
        {
            return await task;
        }
        finally
        {
            lock (snapshotsInFlight)
                snapshotsInFlight.Remove(key);
        }
    }

    private async Task<SnapshotResult> WriteSnapshotAsync(string root, string host, string database, bool overwrite)
    {
        var functions = await schema.ReadFunctionsAsync(host, database, CancellationToken.None);
        return FunctionSnapshots.Write(root, host, database, functions, overwrite);
    }

    private static bool IsSnapshotFailure(Exception exception) =>
        exception is InvalidOperationException or HttpRequestException or TaskCanceledException
            or IOException or UnauthorizedAccessException;

    private static string Describe(SnapshotResult snapshot, string host, string database)
    {
        var message = $"Wrote the functions of {host} / {database} to {snapshot.Directory}: "
            + $"{snapshot.Created} new, {snapshot.Changed} changed, {snapshot.Unchanged} as they were.";
        return snapshot.Stale.Count == 0
            ? message
            : message + $" {snapshot.Stale.Count} file{(snapshot.Stale.Count == 1 ? " has" : "s have")} no function on the cluster.";
    }

    private static string? DocumentPath(string? uri) =>
        uri is not null && Uri.TryCreate(uri, UriKind.Absolute, out var parsed) && parsed.IsFile ? parsed.LocalPath : null;

    private Task ShowMessageAsync(int type, string message) =>
        SendAsync(new { jsonrpc = "2.0", method = "window/showMessage", @params = new { type, message } });
}
