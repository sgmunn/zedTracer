using System.Text;
using System.Text.RegularExpressions;
using Kusto.Language;
using Kusto.Language.Symbols;
using Kusto.Language.Syntax;

/// <summary>
/// Signature help for the function call the cursor is inside. The call is found from the tokens
/// before the cursor rather than from the syntax tree, because the call being typed rarely parses
/// as a call yet: `Add(1, ` has no closing parenthesis and no second argument.
/// </summary>
internal static partial class SignatureHelp
{
    public static object? Get(string text, GlobalState globals, int offset)
    {
        var code = KustoCode.Parse(text, globals);
        var call = FindCall(code.GetLexicalTokens(), offset);
        while (call is not null)
        {
            if (FindFunction(call.Name, globals) is { Signatures.Count: > 0 } function)
            {
                var signatures = function.Signatures.Where(signature => !signature.IsHidden).ToList();
                if (signatures.Count > 0)
                {
                    var argumentCount = call.Commas + 1;
                    return new
                    {
                        signatures = signatures.Select(signature => Describe(function, signature)).ToArray(),
                        activeSignature = Math.Max(
                            signatures.FindIndex(signature => signature.IsValidArgumentCount(argumentCount)), 0),
                        activeParameter = call.Commas
                    };
                }
            }
            call = call.Outer;
        }
        return null;
    }

    private sealed record Call(string Name, int Commas, Call? Outer);

    private sealed class Frame(string? name)
    {
        public string? Name { get; } = name;
        public int Commas { get; set; }
    }

    /// <summary>The innermost call that contains the offset, with the calls around it.</summary>
    private static Call? FindCall(IReadOnlyList<Kusto.Language.Parsing.LexicalToken> tokens, int offset)
    {
        var stack = new List<Frame>();
        string? previousText = null;
        var position = 0;
        foreach (var token in tokens)
        {
            var textStart = position + token.Trivia.Length;
            position += token.Length;
            if (textStart >= offset)
                break;

            switch (token.Text)
            {
                case "(":
                    stack.Add(new Frame(previousText is not null && IdentifierPattern().IsMatch(previousText) ? previousText : null));
                    break;
                case "[" or "{":
                    stack.Add(new Frame(null));
                    break;
                case ")" or "]" or "}":
                    if (stack.Count > 0)
                        stack.RemoveAt(stack.Count - 1);
                    break;
                case ",":
                    if (stack.Count > 0)
                        stack[^1].Commas++;
                    break;
            }
            previousText = token.Text;
        }

        Call? call = null;
        foreach (var frame in stack.Where(frame => frame.Name is not null))
            call = new Call(frame.Name!, frame.Commas, call);
        return call;
    }

    private static FunctionSymbol? FindFunction(string name, GlobalState globals) =>
        globals.Database?.GetFunction(name) ?? globals.GetFunction(name);

    private static object Describe(FunctionSymbol function, Signature signature)
    {
        var label = new StringBuilder(function.Name).Append('(');
        var parameters = new List<object>();
        foreach (var parameter in signature.Parameters)
        {
            if (parameters.Count > 0)
                label.Append(", ");
            var start = label.Length;
            label.Append(Parameter.GetDeclaration(parameter));
            parameters.Add(new
            {
                label = new[] { start, label.Length },
                documentation = string.IsNullOrEmpty(parameter.Description) ? null : parameter.Description
            });
        }
        label.Append(')');

        return new
        {
            label = label.ToString(),
            documentation = string.IsNullOrWhiteSpace(function.Description) ? null : function.Description,
            parameters
        };
    }

    [GeneratedRegex(@"^[A-Za-z_][A-Za-z_0-9]*$")]
    private static partial Regex IdentifierPattern();
}
