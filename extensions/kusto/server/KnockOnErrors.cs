using Kusto.Language;
using Kusto.Language.Syntax;

/// <summary>
/// When the table or function a query starts from is not known, the analysis cannot say what its
/// columns are, so it reports every column the rest of the query uses, and every argument whose type
/// depends on one. Those say nothing the first error does not, and they bury it. Only the errors that
/// follow from an unresolved source are left out: a wrong name elsewhere in the query, or in another
/// statement, is still reported.
/// </summary>
internal static class KnockOnErrors
{
    /// <summary>The source of a statement is a name the schema does not have.</summary>
    private static readonly HashSet<string> UnresolvedSource = new(StringComparer.Ordinal)
    {
        "KS204", // table, tabular variable or function
        "KS211", // function
        "KS143", // function not defined
        "KS208", // database
        "KS209", // external table
        "KS210", // materialized view
        "KS247", // entity group
        "KS248"  // stored query result
    };

    /// <summary>What an unknown source makes the analysis report downstream.</summary>
    private static readonly HashSet<string> Consequence = new(StringComparer.Ordinal)
    {
        "KS142", // the name does not refer to any known column, table, variable or function
        "KS107", // a value of a type is expected
        "KS108", // scalar value expected
        "KS109", // column name expected
        "KS111", // tabular value expected
        "KS112", // tabular or scalar value expected
        "KS113"  // a tabular value with only one column expected
    };

    public static List<Diagnostic> Without(KustoCode code, List<Diagnostic> diagnostics)
    {
        if (!diagnostics.Any(diagnostic => UnresolvedSource.Contains(diagnostic.Code)))
            return diagnostics;

        var hidden = new List<(int From, int To)>();
        foreach (var statement in code.Syntax.GetDescendants<Statement>())
        {
            var source = SourceOf(statement);
            if (source is null)
                continue;
            var unresolved = diagnostics.Any(diagnostic =>
                UnresolvedSource.Contains(diagnostic.Code)
                && diagnostic.Start >= source.TextStart
                && diagnostic.Start < source.End);
            if (unresolved)
                hidden.Add((source.End, statement.End));
        }

        return diagnostics
            .Where(diagnostic => !Consequence.Contains(diagnostic.Code)
                || !hidden.Any(range => diagnostic.Start >= range.From && diagnostic.Start < range.To))
            .ToList();
    }

    /// <summary>The expression a statement's pipeline starts from: `T` in `T | where A > 1 | take 5`.</summary>
    private static SyntaxNode? SourceOf(Statement statement)
    {
        var expression = statement switch
        {
            ExpressionStatement expressionStatement => expressionStatement.Expression,
            LetStatement letStatement => letStatement.Expression,
            _ => null
        };
        while (expression is PipeExpression pipe)
            expression = pipe.Expression;
        return expression;
    }
}
