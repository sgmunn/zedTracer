using System.Text.RegularExpressions;

/// <summary>The cluster and database a query runs on, as written: not yet normalized.</summary>
internal readonly record struct Connection(string? Cluster, string? Database)
{
    public string? Host => Cluster is null ? null : KustoRestClient.ClusterHost(Cluster);
}

/// <summary>What the directives above a point set: where a query runs, the values the file gives to parameters, and the functions it defines.</summary>
internal sealed record DirectiveState(
    Connection Connection,
    IReadOnlyDictionary<string, string> Parameters,
    IReadOnlyList<InlineFunction> Functions);

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
/// A block with the line `// :functions` defines functions (see <see cref="InlineFunctions"/>) that every query
/// below it can use. A later definition of the same name replaces it, `// :clearFunction("name")` takes one
/// away and `// :clearFunctions()` every one.
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
    public const string ClearFunction = "clearFunction";
    public const string ClearFunctions = "clearFunctions";

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

    [GeneratedRegex(@"^\s*//\s*:\s*functions\s*$")]
    private static partial Regex FunctionsMarker();

    /// <summary>Whether a line marks its block as one of function definitions.</summary>
    public static bool IsFunctionsMarker(string line) => FunctionsMarker().IsMatch(line);

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
            ClearFunction => arguments.Length == 1 && QueryParameters.IsParameterName(arguments[0]),
            ClearFunctions => arguments.Length == 0,
            _ => false
        };
        return fits ? new Directive(name, arguments) : null;
    }

    /// <summary>What every directive on a line that starts at or before `offset` has set.</summary>
    public static DirectiveState StateBefore(string text, int offset, Connection defaults)
    {
        var connection = defaults;
        var parameters = new Dictionary<string, string>();
        var functions = new List<InlineFunction>();
        var definitions = new Queue<QueryBlock>(QueryBlocks.Find(text).Where(block => block.IsFunctions));
        var lineStart = 0;
        foreach (var line in text.Split('\n'))
        {
            if (lineStart > offset)
                break;
            while (definitions.Count > 0 && definitions.Peek().Start <= lineStart)
            {
                foreach (var definition in InlineFunctions.Read(definitions.Dequeue().Text))
                    InlineFunctions.Define(functions, definition);
            }
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
                    case ClearFunction:
                        functions.RemoveAll(function => function.Name == directive.Arguments[0]);
                        break;
                    case ClearFunctions:
                        functions.Clear();
                        break;
                }
            }
            lineStart += line.Length + 1;
        }
        return new DirectiveState(connection, parameters, functions);
    }

    /// <summary>The connection after every directive on a line that starts at or before `offset`.</summary>
    public static Connection Before(string text, int offset, Connection defaults) =>
        StateBefore(text, offset, defaults).Connection;

    /// <summary>
    /// The connection at a position: that of the query it is in, or where a new query would
    /// start when it is on a blank line.
    /// </summary>
    public static Connection At(string text, int offset, Connection defaults) =>
        StateAt(text, offset, defaults).Connection;

    /// <summary>What applies at a position: to the query it is in, or where a new query would start when it is on a blank line.</summary>
    public static DirectiveState StateAt(string text, int offset, Connection defaults)
    {
        var block = QueryBlocks.At(QueryBlocks.Find(text), offset);
        return StateBefore(text, block?.End ?? offset, defaults);
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
            if (DirectiveStart().IsMatch(lines[line]) && !IsFunctionsMarker(lines[line]) && Read(lines[line]) is null)
                yield return (line, lines[line].TrimEnd('\r').Length);
        }
    }
}
