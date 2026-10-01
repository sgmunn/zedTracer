using System.Text.RegularExpressions;

/// <summary>The cluster and database a query runs on, as written: not yet normalized.</summary>
internal readonly record struct Connection(string? Cluster, string? Database)
{
    public string? Host => Cluster is null ? null : KustoRestClient.ClusterHost(Cluster);
}

/// <summary>
/// Where a query runs, as the file says. A line that is a comment of the form
/// `// :setDefaultCluster("https://…")` or `// :setDefaultDb("…")` sets the cluster or the database for
/// every query below it, until a later line sets it again. Setting the cluster clears the
/// database, because a database name from another cluster is more likely to fail confusingly than
/// to be the right one. The space after the slashes is optional: Zed adds `// ` when a comment line is
/// continued, so `// :` is what gets typed, and `//:` is what other Kusto extensions write. Before the first directive the defaults apply. The editor resolves the
/// same way when it runs a query, and both are tested against fork-docs/samples/connection-directives.json.
/// </summary>
internal static partial class ConnectionDirectives
{
    public const string SetCluster = "setDefaultCluster";
    public const string SetDatabase = "setDefaultDb";

    [GeneratedRegex(@"^\s*//\s*:\s*([A-Za-z]+)\s*\(\s*(?:""([^""]*)""|'([^']*)')\s*\)\s*$")]
    private static partial Regex DirectiveLine();

    [GeneratedRegex(@"^\s*//\s*:")]
    private static partial Regex DirectiveStart();

    /// <summary>The connection after every directive on a line that starts at or before `offset`.</summary>
    public static Connection Before(string text, int offset, Connection defaults)
    {
        var connection = defaults;
        var lineStart = 0;
        foreach (var line in text.Split('\n'))
        {
            if (lineStart > offset)
                break;
            var match = DirectiveLine().Match(line);
            if (match.Success)
            {
                var value = match.Groups[2].Success ? match.Groups[2].Value : match.Groups[3].Value;
                switch (match.Groups[1].Value)
                {
                    case SetCluster:
                        connection = new Connection(value, null);
                        break;
                    case SetDatabase:
                        connection = connection with { Database = value };
                        break;
                }
            }
            lineStart += line.Length + 1;
        }
        return connection;
    }

    /// <summary>
    /// The connection at a position: that of the query it is in, or where a new query would
    /// start when it is on a blank line.
    /// </summary>
    public static Connection At(string text, int offset, Connection defaults)
    {
        var block = QueryBlocks.At(QueryBlocks.Find(text), offset);
        return Before(text, block?.End ?? offset, defaults);
    }

    /// <summary>The connection of each query, in order.</summary>
    public static List<Connection> OfQueries(string text, Connection defaults) =>
        QueryBlocks.Find(text)
            .Where(block => block.IsQuery)
            .Select(block => Before(text, block.End, defaults))
            .ToList();

    /// <summary>Lines that look like directives but are not ones the editor knows.</summary>
    public static IEnumerable<(int Line, int Length)> Problems(string text)
    {
        var lines = text.Split('\n');
        for (var line = 0; line < lines.Length; line++)
        {
            if (!DirectiveStart().IsMatch(lines[line]))
                continue;
            var match = DirectiveLine().Match(lines[line]);
            if (!match.Success || match.Groups[1].Value is not (SetCluster or SetDatabase))
                yield return (line, lines[line].TrimEnd('\r').Length);
        }
    }
}
