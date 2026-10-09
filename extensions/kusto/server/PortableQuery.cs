using Kusto.Language;
using Kusto.Language.Symbols;
using Kusto.Language.Syntax;

/// <summary>
/// A query that means the same wherever it is run. A name that only the query's own database knows
/// (a table, a function) gets the cluster and database in front of it, so that a copy of the query
/// pasted into another tool, or kept with a result, still finds it.
/// </summary>
internal static class PortableQuery
{
    public const string Command = "kusto.qualifyQuery";

    /// <summary>
    /// `Text` is the query with its names qualified. It is `Complete` when the query's database is
    /// known, so that every name that needed it got it; without the schema nothing can be told from
    /// a column, and the text is the query as it was.
    /// </summary>
    public sealed record Result(string Text, bool Complete);

    public static Result Qualify(string text, GlobalState globals, string host, string database, bool schemaIsLoaded)
    {
        if (QueryBlocks.IsControlCommand(text))
            return new Result(text, Complete: true);
        if (!schemaIsLoaded)
            return new Result(text, Complete: false);

        var code = KustoCode.ParseAndAnalyze(text, globals);
        var members = globals.Database.Members;
        var prefix = $"cluster('{Escape(host)}').database('{Escape(database)}').";
        var starts = new List<int>();
        foreach (var name in code.Syntax.GetDescendants<NameReference>())
        {
            var call = name.Parent is FunctionCallExpression function && function.Name == name ? function : null;
            var symbol = name.ReferencedSymbol ?? call?.ReferencedSymbol;
            if (symbol is null || !members.Contains(symbol))
                continue;
            SyntaxNode used = call is not null ? call : name;
            // After a dot the name belongs to what comes before it, which is already qualified.
            if (used.Parent is PathExpression path && path.Selector == used)
                continue;
            starts.Add(name.TextStart);
        }

        var qualified = new System.Text.StringBuilder(text);
        foreach (var start in starts.OrderByDescending(start => start))
            qualified.Insert(start, prefix);
        return new Result(qualified.ToString(), Complete: true);
    }

    private static string Escape(string text) => text.Replace("\\", "\\\\").Replace("'", "\\'");
}
