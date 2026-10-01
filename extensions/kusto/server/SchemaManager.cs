using System.Text.Json;
using Kusto.Language;
using Kusto.Language.Symbols;

/// <summary>
/// Owns the symbols the parser resolves names with: clusters, their databases and, for each
/// database that something refers to, its tables, functions and materialized views. Schema is
/// fetched in the background the first time a cluster or database is referred to, so a request
/// never waits for the network; the first completion after a load sees the new names.
/// </summary>
internal sealed class SchemaManager
{
    private static readonly TimeSpan RetryAfterFailure = TimeSpan.FromSeconds(60);

    private readonly KustoRestClient client;
    private readonly SchemaCache cache;
    private readonly object gate = new();
    private readonly Dictionary<string, Attempt> attempts = new();
    private GlobalState globals;
    private string? defaultCluster;
    private string? defaultHost;
    private string? defaultDatabase;

    public SchemaManager(KustoRestClient client, GlobalState offlineGlobals, SchemaCache cache)
    {
        this.client = client;
        this.cache = cache;
        globals = offlineGlobals.WithDomain("kusto.windows.net");
    }

    /// <summary>
    /// How old a cached schema may be before it is fetched again, in minutes. A cached schema is
    /// used straight away whatever its age; this only decides whether the cluster is asked too.
    /// Zero asks every time the server starts.
    /// </summary>
    public int CacheMinutes { get; set; } = 60;

    private bool IsFresh(DateTimeOffset fetchedAt) =>
        CacheMinutes > 0 && DateTimeOffset.UtcNow - fetchedAt < TimeSpan.FromMinutes(CacheMinutes);

    /// <summary>The cluster and database that unqualified table names refer to.</summary>
    public void SetDefaults(string? cluster, string? database)
    {
        lock (gate)
        {
            defaultCluster = string.IsNullOrWhiteSpace(cluster) ? null : cluster.Trim();
            defaultHost = string.IsNullOrWhiteSpace(cluster) ? null : KustoRestClient.ClusterHost(cluster);
            defaultDatabase = string.IsNullOrWhiteSpace(database) ? null : database.Trim();
        }
        EnsureDefaults();
    }

    /// <summary>The cluster and database a query has when the file does not say.</summary>
    public Connection Defaults
    {
        get
        {
            lock (gate)
                return new Connection(defaultCluster, defaultDatabase);
        }
    }

    /// <summary>The symbols to analyse a query with, with its cluster and database applied.</summary>
    public GlobalState GlobalsFor(Connection connection)
    {
        lock (gate)
        {
            var current = globals;
            if (connection.Host is not { } host || current.GetCluster(host) is not { } cluster)
                return current;
            current = current.WithCluster(cluster);
            if (connection.Database is { } name && cluster.GetDatabase(name) is { IsOpen: false } database)
                current = current.WithDatabase(database);
            return current;
        }
    }

    private void EnsureDefaults()
    {
        if (defaultHost is not null)
            EnsureReference(null, defaultDatabase);
    }

    /// <summary>
    /// Starts loading what a reference needs: the database names of the cluster and, when a
    /// database is named, that database's entities. A null cluster means the default one.
    /// </summary>
    public void EnsureReference(string? cluster, string? database)
    {
        string? host;
        lock (gate)
            host = cluster is null ? defaultHost : KustoRestClient.ClusterHost(cluster);
        if (host is null)
            return;

        Start($"cluster:{host}", token => LoadClusterAsync(host, token));
        if (!string.IsNullOrWhiteSpace(database))
            Start($"database:{host}/{database}", token => LoadDatabaseAsync(host, database, token));
    }

    private void Start(string key, Func<CancellationToken, Task> load)
    {
        lock (gate)
        {
            if (attempts.TryGetValue(key, out var existing)
                && !(existing.Failed && DateTime.UtcNow - existing.When > RetryAfterFailure))
                return;
            attempts[key] = new Attempt(false, DateTime.UtcNow);
        }

        _ = Task.Run(async () =>
        {
            try
            {
                await load(CancellationToken.None);
                lock (gate)
                    attempts[key] = new Attempt(false, DateTime.UtcNow);
                Console.Error.WriteLine($"Loaded {key}");
            }
            catch (Exception exception)
            {
                lock (gate)
                    attempts[key] = new Attempt(true, DateTime.UtcNow);
                Console.Error.WriteLine($"Could not load {key}: {exception.Message}");
            }
        });
    }

    /// <summary>
    /// The databases of a cluster. A cached list is used at once; the cluster is asked too when the
    /// list is old, and a failure to reach it leaves the cached list in place.
    /// </summary>
    private async Task LoadClusterAsync(string host, CancellationToken cancellationToken)
    {
        if (cache.ReadCluster(host) is { } cached)
        {
            ApplyDatabaseNames(host, cached.Databases);
            Console.Error.WriteLine($"Using the cached database list of {host}");
            if (IsFresh(cached.FetchedAt))
                return;
        }

        var rows = await client.ExecuteManagementAsync(
            host, "", ".show databases | project DatabaseName, PrettyName", cancellationToken);
        var names = rows
            .Select(row => new CachedDatabaseName(Text(row, "DatabaseName"), Text(row, "PrettyName")))
            .Where(database => database.Name.Length > 0)
            .ToList();
        ApplyDatabaseNames(host, names);
        cache.WriteCluster(host, names);
    }

    private void ApplyDatabaseNames(string host, IReadOnlyList<CachedDatabaseName> names)
    {
        lock (gate)
        {
            var existing = globals.GetCluster(host);
            var databases = names.Select(database =>
                existing?.GetDatabase(database.Name) is { IsOpen: false } loaded
                    ? loaded
                    : new DatabaseSymbol(
                        database.Name,
                        database.Alternate.Length > 0 ? database.Alternate : null,
                        Array.Empty<Symbol>(),
                        isOpen: true))
                .ToList();
            globals = globals.AddOrReplaceCluster(new ClusterSymbol(host, databases, false));
        }
    }

    /// <summary>
    /// The tables, functions and views of a database. A cached schema is used at once, so completion
    /// works before the network answers and without it; the cluster is asked too when the cached
    /// schema is old, and a failure to reach it leaves the cached schema in place.
    /// </summary>
    private async Task LoadDatabaseAsync(string host, string database, CancellationToken cancellationToken)
    {
        if (cache.ReadDatabase(host, database) is { } cached)
        {
            ApplyDatabase(host, database, BuildMembers(cached.Entities));
            Console.Error.WriteLine($"Using the cached schema of {host}/{database}");
            if (IsFresh(cached.FetchedAt))
                return;
        }

        var command = ".show databases entities with (showObfuscatedStrings=false)"
            + $" | where DatabaseName == {KustoFacts.GetStringLiteral(database)}"
            + " | where EntityType in ('Table', 'ExternalTable', 'MaterializedView', 'Function')";
        var rows = await client.ExecuteManagementAsync(host, database, command, cancellationToken);

        var entities = rows
            .Select(row => new CachedEntity(
                Text(row, "EntityType"),
                Text(row, "EntityName"),
                Text(row, "CslOutputSchema"),
                Text(row, "CslInputSchema"),
                Text(row, "Content"),
                NullIfEmpty(Text(row, "DocString"))))
            .ToList();
        ApplyDatabase(host, database, BuildMembers(entities));
        cache.WriteDatabase(host, database, entities);
    }

    private static List<Symbol> BuildMembers(IEnumerable<CachedEntity> entities)
    {
        var members = new List<Symbol>();
        foreach (var entity in entities)
        {
            switch (entity.Kind)
            {
                case "Table":
                    members.Add(new TableSymbol(entity.Name, "(" + entity.Schema + ")", entity.Description));
                    break;
                case "ExternalTable":
                    members.Add(new ExternalTableSymbol(entity.Name, "(" + entity.Schema + ")", entity.Description));
                    break;
                case "MaterializedView":
                    members.Add(new MaterializedViewSymbol(entity.Name, "(" + entity.Schema + ")", "", entity.Description));
                    break;
                case "Function":
                    members.Add(new FunctionSymbol(entity.Name, entity.Parameters, entity.Body, entity.Description));
                    break;
            }
        }
        return members;
    }

    private void ApplyDatabase(string host, string database, List<Symbol> members)
    {
        lock (gate)
        {
            var cluster = globals.GetCluster(host) ?? new ClusterSymbol(host, Array.Empty<DatabaseSymbol>(), true);
            var existing = cluster.GetDatabase(database);
            var loaded = new DatabaseSymbol(
                existing?.Name ?? database, existing?.AlternateName, members, isOpen: false);
            globals = globals.AddOrReplaceCluster(cluster.AddOrUpdateDatabase(loaded));
        }
    }

    private static string Text(Dictionary<string, JsonElement> row, string column) =>
        row.TryGetValue(column, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString() ?? ""
            : "";

    private static string? NullIfEmpty(string text) => text.Length == 0 ? null : text;

    private readonly record struct Attempt(bool Failed, DateTime When);
}
