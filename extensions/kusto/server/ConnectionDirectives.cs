using System.Text.RegularExpressions;

/// <summary>The cluster and database a query runs on, as written: not yet normalized.</summary>
internal readonly record struct Connection(string? Cluster, string? Database)
{
    public string? Host => Cluster is null ? null : KustoRestClient.ClusterHost(Cluster);
}

/// <summary>What the directives above a point set: where a query runs, and the values the file gives to parameters.</summary>
internal sealed record DirectiveState(Connection Connection, IReadOnlyDictionary<string, string> Parameters);

/// <summary>
/// What the file says about how its queries run. A line that is a comment of the form
/// `// :setDefaultCluster("https://…")` or `// :setDefaultDb("…")` sets the cluster or the database for
/// every query below it, until a later line sets it again. Setting the cluster clears the
/// database, because a database name from another cluster is more likely to fail confusingly than
/// to be the right one. Before the first directive the defaults apply.
///
/// `// :setParam("raid", "abc-123")` gives a parameter that queries declare a value for every query
/// below it, until a later `setParam` of the same name gives another, `// :clearParam("raid")` takes it
/// away, or `// :clearParams()` takes away every one. These values are the file's own, so they come
/// before those of the active parameter profile.
///
/// The space after the slashes is optional: Zed adds `// ` when a comment line is continued, so
/// `// :` is what gets typed, and `//:` is what other Kusto extensions write. An argument is a string in
/// double or single quotes, written as it is: there are no escapes, so a value with one kind of quote in
/// it is written with the other. The editor resolves the same way when it runs a query, and both are
/// tested against fork-docs/samples/connection-directives.json and parameter-directives.json.
/// </summary>
internal static partial class ConnectionDirectives
{
    public const string SetCluster = "setDefaultCluster";
    public const string SetDatabase = "setDefaultDb";
    public const string SetParameter = "setParam";
    public const string ClearParameter = "clearParam";
    public const string ClearParameters = "clearParams";

    private const string Quoted = @"(?:""[^""]*""|'[^']*')";

    private sealed record Directive(string Name, string[] Arguments);

    [GeneratedRegex(@"^\s*//\s*:\s*([A-Za-z]+)\s*\((.*)\)\s*$")]
    private static partial Regex DirectiveLine();

    [GeneratedRegex(@"^\s*(?:" + Quoted + @"(?:\s*,\s*" + Quoted + @")*)?\s*$")]
    private static partial Regex ArgumentList();

    [GeneratedRegex(@"""([^""]*)""|'([^']*)'")]
    private static partial Regex Argument();

    [GeneratedRegex(@"^\s*//\s*:")]
    private static partial Regex DirectiveStart();

    /// <summary>The directive a line is, or null when it is not one of the directives the editor knows with arguments that fit.</summary>
    private static Directive? Read(string line)
    {
        var match = DirectiveLine().Match(line);
        if (!match.Success || !ArgumentList().IsMatch(match.Groups[2].Value))
            return null;
        var arguments = Argument().Matches(match.Groups[2].Value)
            .Select(found => found.Groups[1].Success ? found.Groups[1].Value : found.Groups[2].Value)
            .ToArray();
        var name = match.Groups[1].Value;
        var fits = name switch
        {
            SetCluster or SetDatabase => arguments.Length == 1,
            SetParameter => arguments.Length == 2 && QueryParameters.IsParameterName(arguments[0]),
            ClearParameter => arguments.Length == 1 && QueryParameters.IsParameterName(arguments[0]),
            ClearParameters => arguments.Length == 0,
            _ => false
        };
        return fits ? new Directive(name, arguments) : null;
    }

    /// <summary>What every directive on a line that starts at or before `offset` has set.</summary>
    public static DirectiveState StateBefore(string text, int offset, Connection defaults)
    {
        var connection = defaults;
        var parameters = new Dictionary<string, string>();
        var lineStart = 0;
        foreach (var line in text.Split('\n'))
        {
            if (lineStart > offset)
                break;
            if (Read(line) is { } directive)
            {
                switch (directive.Name)
                {
                    case SetCluster:
                        connection = new Connection(directive.Arguments[0], null);
                        break;
                    case SetDatabase:
                        connection = connection with { Database = directive.Arguments[0] };
                        break;
                    case SetParameter:
                        parameters[directive.Arguments[0]] = directive.Arguments[1];
                        break;
                    case ClearParameter:
                        parameters.Remove(directive.Arguments[0]);
                        break;
                    case ClearParameters:
                        parameters.Clear();
                        break;
                }
            }
            lineStart += line.Length + 1;
        }
        return new DirectiveState(connection, parameters);
    }

    /// <summary>The connection after every directive on a line that starts at or before `offset`.</summary>
    public static Connection Before(string text, int offset, Connection defaults) =>
        StateBefore(text, offset, defaults).Connection;

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

    /// <summary>Lines that look like directives but are not ones the editor knows, or whose arguments do not fit.</summary>
    public static IEnumerable<(int Line, int Length)> Problems(string text)
    {
        var lines = text.Split('\n');
        for (var line = 0; line < lines.Length; line++)
        {
            if (DirectiveStart().IsMatch(lines[line]) && Read(lines[line]) is null)
                yield return (line, lines[line].TrimEnd('\r').Length);
        }
    }
}
