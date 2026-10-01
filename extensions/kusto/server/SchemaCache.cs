using System.Text.Json;

/// <summary>
/// The schema of clusters and databases kept on disk, so completion works as soon as the server
/// starts and does not wait for, or depend on, the network. Entries are replaced by the schema
/// fetched from the cluster, which happens in the background when an entry is old.
/// </summary>
internal sealed class SchemaCache
{
    /// <summary>Entries written by another version of the format are not read.</summary>
    public const int FormatVersion = 1;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase
    };

    private readonly string? root;

    /// <summary>Without a data directory there is no cache, and everything is fetched.</summary>
    public SchemaCache(string? dataDirectory)
    {
        root = string.IsNullOrWhiteSpace(dataDirectory)
            ? null
            : Path.Combine(dataDirectory, "kusto", "schema");
    }

    public CachedDatabase? ReadDatabase(string host, string database) =>
        Read<CachedDatabase>(DatabasePath(host, database), entry => entry.Version);

    public void WriteDatabase(string host, string database, IReadOnlyList<CachedEntity> entities) =>
        Write(DatabasePath(host, database), new CachedDatabase(FormatVersion, DateTimeOffset.UtcNow, entities.ToList()));

    public CachedCluster? ReadCluster(string host) =>
        Read<CachedCluster>(ClusterPath(host), entry => entry.Version);

    public void WriteCluster(string host, IReadOnlyList<CachedDatabaseName> databases) =>
        Write(ClusterPath(host), new CachedCluster(FormatVersion, DateTimeOffset.UtcNow, databases.ToList()));

    // Names are escaped so that nothing in a cluster or database name can leave the folder. A
    // database file always ends in an escaped name, and `@` is always escaped, so the cluster
    // file's name cannot be a database's.
    private string? ClusterPath(string host) =>
        root is null ? null : Path.Combine(root, Uri.EscapeDataString(host), "@databases.json");

    private string? DatabasePath(string host, string database) =>
        root is null ? null : Path.Combine(root, Uri.EscapeDataString(host), Uri.EscapeDataString(database) + ".json");

    private static T? Read<T>(string? path, Func<T, int> version) where T : class
    {
        if (path is null || !File.Exists(path))
            return null;
        try
        {
            var entry = JsonSerializer.Deserialize<T>(File.ReadAllText(path), JsonOptions);
            return entry is not null && version(entry) == FormatVersion ? entry : null;
        }
        catch (Exception exception) when (exception is JsonException or IOException or UnauthorizedAccessException)
        {
            Console.Error.WriteLine($"Ignoring the cached schema {path}: {exception.Message}");
            return null;
        }
    }

    /// <summary>
    /// Written to a new file that then replaces the old one, so another server reading the same
    /// entry, such as one in another window, never sees half of it.
    /// </summary>
    private static void Write<T>(string? path, T entry)
    {
        if (path is null)
            return;
        var temporary = $"{path}.{Guid.NewGuid():N}.tmp";
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(temporary, JsonSerializer.Serialize(entry, JsonOptions));
            File.Move(temporary, path, overwrite: true);
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
        {
            Console.Error.WriteLine($"Could not cache the schema in {path}: {exception.Message}");
            try
            {
                File.Delete(temporary);
            }
            catch (IOException)
            {
            }
        }
    }
}

/// <summary>One table, function or view as the cluster described it.</summary>
internal sealed record CachedEntity(
    string Kind,
    string Name,
    string Schema,
    string Parameters,
    string Body,
    string? Description);

internal sealed record CachedDatabase(int Version, DateTimeOffset FetchedAt, List<CachedEntity> Entities);

internal sealed record CachedDatabaseName(string Name, string Alternate);

internal sealed record CachedCluster(int Version, DateTimeOffset FetchedAt, List<CachedDatabaseName> Databases);
