using Kusto.Language;
using Kusto.Language.Symbols;

/// <summary>
/// A function or value defined in the file: its name, its parameters as written (`(x:long)`) and its body
/// (`{ … }`). A value, such as `let limit = 5;` or `let recent = Events | take 3;`, is a function without
/// parameters whose body is the expression, which is how the library takes it.
/// </summary>
internal sealed record InlineFunction(string Name, string Parameters, string Body, bool IsValue = false);

/// <summary>A definition with where it is in the block that holds it: its whole statement, and its name, as offsets into the block.</summary>
internal sealed record LocatedFunction(InlineFunction Function, int Start, int End, int NameStart, int NameEnd);

/// <summary>
/// Functions and values defined in the query file itself. A block with the line `// :functions` holds the
/// definitions, as `let name = …;` statements, and every query below it can use them (see
/// <see cref="ConnectionDirectives"/> for how long they last). Here they are symbols of
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

    private static List<InlineFunction> Parse(string block) =>
        Locate(block).Select(located => located.Function).ToList();

    /// <summary>The functions a functions block defines, in the order written, with where each is in the block.</summary>
    public static List<LocatedFunction> Locate(string block)
    {
        var functions = new List<LocatedFunction>();
        if (KustoCode.Parse(block).Syntax is not Kusto.Language.Syntax.QueryBlock query)
            return functions;
        foreach (var element in query.Statements)
        {
            if (element.Element is not Kusto.Language.Syntax.LetStatement statement)
                continue;
            var name = statement.Name.SimpleName;
            InlineFunction? function = null;
            if (statement.Expression is Kusto.Language.Syntax.FunctionDeclaration declaration)
            {
                // `let f = (x:long);` parses as a function with nothing in its body, which the block's own diagnostics report.
                if (declaration.Body.ToString().Trim().StartsWith('{'))
                    function = new InlineFunction(name, declaration.Parameters.ToString().Trim(), declaration.Body.ToString().Trim());
            }
            else if (statement.Expression.ToString().Trim() is { Length: > 0 } expression)
            {
                function = new InlineFunction(name, "()", "{ " + expression + " }", IsValue: true);
            }
            if (function is not null)
                functions.Add(new LocatedFunction(
                    function, statement.TextStart, statement.End, statement.Name.TextStart, statement.Name.End));
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

    /// <summary>
    /// The calls, written with parentheses, of a name the file defines as a value. The library takes a
    /// function without parameters either way, but the service does not take `limit()` for `let limit = 5;`.
    /// </summary>
    public static List<(int Start, int Length, string Name)> CallsOfValues(KustoCode code, GlobalState globals, IReadOnlyList<InlineFunction> functions)
    {
        var calls = new List<(int, int, string)>();
        var values = functions.Where(function => function.IsValue).Select(function => function.Name).ToHashSet();
        if (values.Count == 0)
            return calls;
        var members = globals.Database.Members;
        foreach (var name in code.Syntax.GetDescendants<Kusto.Language.Syntax.NameReference>())
        {
            if (name.Parent is not Kusto.Language.Syntax.FunctionCallExpression call || call.Name != name)
                continue;
            var symbol = name.ReferencedSymbol ?? call.ReferencedSymbol;
            if (symbol is FunctionSymbol && values.Contains(symbol.Name) && members.Contains(symbol))
                calls.Add((call.TextStart, call.End - call.TextStart, symbol.Name));
        }
        return calls;
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
