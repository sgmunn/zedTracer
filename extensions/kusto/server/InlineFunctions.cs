using Kusto.Language;
using Kusto.Language.Symbols;

/// <summary>A function defined in the file: its name, its parameters as written (`(x:long)`) and its body (`{ … }`).</summary>
internal sealed record InlineFunction(string Name, string Parameters, string Body);

/// <summary>
/// Functions defined in the query file itself. A block with the line `// :functions` holds the
/// definitions, as `let name = (parameters) { body };` statements, and every query below it can use
/// them (see <see cref="ConnectionDirectives"/> for how long they last). Here they are symbols of
/// the query's database, so completion, hover, signature help, colours and the check of names all
/// know them, and no text is added to the query; the editor adds the `let`s that a run needs. Which
/// statements are definitions is decided the same way by the editor, and both are tested against
/// fork-docs/samples/function-definitions.json.
/// </summary>
internal static class InlineFunctions
{
    private const int CacheLimit = 64;

    private static readonly Dictionary<string, IReadOnlyList<InlineFunction>> Cache = [];

    /// <summary>The functions a functions block defines, in the order written.</summary>
    public static IReadOnlyList<InlineFunction> Read(string block)
    {
        lock (Cache)
        {
            if (Cache.TryGetValue(block, out var cached))
                return cached;
        }
        var functions = Parse(block);
        lock (Cache)
        {
            if (Cache.Count >= CacheLimit)
                Cache.Clear();
            Cache[block] = functions;
        }
        return functions;
    }

    private static List<InlineFunction> Parse(string block)
    {
        var functions = new List<InlineFunction>();
        if (KustoCode.Parse(block).Syntax is not Kusto.Language.Syntax.QueryBlock query)
            return functions;
        foreach (var element in query.Statements)
        {
            // `let f = (x:long);` parses as a function with nothing in its body.
            if (element.Element is Kusto.Language.Syntax.LetStatement { Expression: Kusto.Language.Syntax.FunctionDeclaration declaration } statement
                && declaration.Body.ToString().Trim().StartsWith('{'))
            {
                functions.Add(new InlineFunction(
                    statement.Name.SimpleName,
                    declaration.Parameters.ToString().Trim(),
                    declaration.Body.ToString().Trim()));
            }
        }
        return functions;
    }

    /// <summary>The definitions after `definition` is made: it replaces a function of the same name where that was, and otherwise goes last.</summary>
    public static void Define(List<InlineFunction> functions, InlineFunction definition)
    {
        var index = functions.FindIndex(existing => existing.Name == definition.Name);
        if (index >= 0)
            functions[index] = definition;
        else
            functions.Add(definition);
    }

    /// <summary>The symbols with the functions added to the database the query runs on, which may be none that is known yet.</summary>
    public static GlobalState Apply(GlobalState globals, IReadOnlyList<InlineFunction> functions)
    {
        if (functions.Count == 0)
            return globals;
        var symbols = functions
            .Select(function => (Symbol)new FunctionSymbol(function.Name, function.Parameters, function.Body))
            .ToArray();
        return globals.WithDatabase(globals.Database.AddMembers(symbols));
    }
}
