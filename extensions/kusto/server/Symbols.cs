using System.Text.Json;
using Kusto.Language;
using Kusto.Language.Syntax;

/// <summary>
/// One entry of a document's outline: its name, and where it is in the text as offsets. The editor shows
/// the name and jumps to the name's range, and colours the name from the text it finds there, so the
/// name is the text of the range whenever it can be.
/// </summary>
internal sealed record OutlineSymbol(string Name, string Detail, int Kind, int Start, int End, int NameStart, int NameEnd);

/// <summary>
/// The outline of a file of queries: each query, labelled the way the history labels it, and each
/// definition in a `// :functions` block, in the order written. A command that defines a function is
/// that function. Notes and directives on their own are not entries.
/// </summary>
internal static class Outline
{
    public const int FunctionKind = 12;
    public const int VariableKind = 13;
    public const int QueryKind = 24;

    /// <summary>A long first line makes an outline nobody can read, so a label is cut here.</summary>
    private const int LabelLength = 100;

    public static List<OutlineSymbol> Of(string text)
    {
        var symbols = new List<OutlineSymbol>();
        foreach (var block in QueryBlocks.Find(text))
        {
            if (block.IsFunctions)
            {
                foreach (var located in InlineFunctions.Locate(block.Text))
                {
                    symbols.Add(new OutlineSymbol(
                        located.Function.Name,
                        located.Function.IsValue ? "" : located.Function.Parameters,
                        located.Function.IsValue ? VariableKind : FunctionKind,
                        block.Start + located.Start,
                        block.Start + located.End,
                        block.Start + located.NameStart,
                        block.Start + located.NameEnd));
                }
            }
            else if (block.IsQuery && Describe(block) is { } symbol)
            {
                symbols.Add(symbol);
            }
        }
        return symbols;
    }

    private static OutlineSymbol? Describe(QueryBlock block)
    {
        if (QueryBlocks.IsControlCommand(block.Text) && DefinedFunction(block.Text) is { } defined)
        {
            return new OutlineSymbol(
                defined.Name, defined.Parameters, FunctionKind,
                block.Start, block.End, block.Start + defined.NameStart, block.Start + defined.NameEnd);
        }
        return Label(block.Text) is { } label
            ? new OutlineSymbol(label.Text, "", QueryKind, block.Start, block.End, block.Start + label.Start, block.Start + label.End)
            : null;
    }

    /// <summary>The function a `.create-or-alter function` command (or `.create`, or `.alter`) defines, with where its name is.</summary>
    public static (string Name, string Parameters, int NameStart, int NameEnd)? DefinedFunction(string command)
    {
        var syntax = KustoCode.Parse(command).Syntax;
        foreach (var name in syntax.GetDescendants<NameDeclaration>())
        {
            if (name.Parent is not { } parent)
                continue;
            for (var index = 0; index < parent.ChildCount; index++)
            {
                if (parent.GetChild(index) is FunctionDeclaration declaration)
                    return (name.SimpleName, declaration.Parameters.ToString().Trim(), name.TextStart, name.End);
            }
        }
        return null;
    }

    /// <summary>
    /// What a query is called: the first comment above its code that says something (a directive, or a line
    /// of dashes, does not), and otherwise its first line of code. A comment after the code starts is not a
    /// label, it is a line commented out of the query. The range is where that text is in the block.
    /// </summary>
    public static (string Text, int Start, int End)? Label(string block)
    {
        var lineStart = 0;
        foreach (var raw in block.Split('\n'))
        {
            var line = raw.TrimEnd('\r');
            var indent = line.Length - line.TrimStart().Length;
            var trimmed = line.Trim();
            if (trimmed.StartsWith("//", StringComparison.Ordinal))
            {
                var body = trimmed[2..].Trim();
                if (body.Any(char.IsLetterOrDigit) && !body.StartsWith(':'))
                {
                    var start = lineStart + indent + 2 + (trimmed[2..].Length - trimmed[2..].TrimStart().Length);
                    return (Truncate(body), start, start + Truncate(body).Length);
                }
            }
            else if (trimmed.Length > 0)
            {
                return (Truncate(trimmed), lineStart + indent, lineStart + indent + Truncate(trimmed).Length);
            }
            lineStart += raw.Length + 1;
        }
        return null;
    }

    private static string Truncate(string text) =>
        text.Length <= LabelLength ? text : text[..LabelLength].TrimEnd();
}

internal sealed partial class KustoLanguageServer
{
    private object[] GetDocumentSymbols(JsonElement parameters)
    {
        var uri = parameters.GetProperty("textDocument").GetProperty("uri").GetString();
        if (uri is null || !documents.TryGetValue(uri, out var document))
            return [];
        return Outline.Of(document.Text)
            .Select(symbol => (object)new
            {
                name = symbol.Name,
                detail = symbol.Detail,
                kind = symbol.Kind,
                range = new { start = document.PositionAt(symbol.Start), end = document.PositionAt(symbol.End) },
                selectionRange = new { start = document.PositionAt(symbol.NameStart), end = document.PositionAt(symbol.NameEnd) }
            })
            .ToArray();
    }

    /// <summary>
    /// The functions that have a file in a snapshot (see <see cref="FunctionSnapshots"/>) whose name matches the
    /// query, best matches first. Only functions with a file can be listed, because a symbol is a place in a
    /// file, and nothing is fetched: going to a definition is what makes the files.
    /// </summary>
    private object[] GetWorkspaceSymbols(JsonElement parameters)
    {
        var query = parameters.TryGetProperty("query", out var typed) ? typed.GetString() ?? "" : "";
        var matches = SnapshotRoots()
            .SelectMany(FunctionSnapshots.Files)
            .Select(file => (File: file, Rank: FunctionSnapshots.Rank(file.Name, query)))
            .Where(match => match.Rank >= 0)
            .OrderBy(match => match.Rank)
            .ThenBy(match => match.File.Name, StringComparer.OrdinalIgnoreCase)
            .ThenBy(match => match.File.Path, StringComparer.Ordinal)
            .Take(WorkspaceSymbolLimit)
            .ToList();
        return matches
            .Select(match =>
            {
                var file = match.File;
                var line = FunctionSnapshots.NameLine(File.ReadAllText(file.Path), file.Name);
                return (object)new
                {
                    name = file.Name,
                    kind = Outline.FunctionKind,
                    containerName = file.Folder is null ? file.Database : $"{file.Database} / {file.Folder}",
                    location = new
                    {
                        uri = new Uri(file.Path).AbsoluteUri,
                        range = new
                        {
                            start = new { line, character = 0 },
                            end = new { line, character = file.Name.Length }
                        }
                    }
                };
            })
            .ToArray();
    }

    private const int WorkspaceSymbolLimit = 200;

    /// <summary>The folders snapshots are kept in for the open projects: each project's own, or the one full path.</summary>
    private IEnumerable<string> SnapshotRoots()
    {
        var projects = workspaceFolders.Count == 0
            ? [(IReadOnlyList<string>)[]]
            : workspaceFolders.Select(folder => (IReadOnlyList<string>)[folder]);
        return projects
            .Select(project => FunctionSnapshots.Root(snapshotDirectory, null, project).Root)
            .OfType<string>()
            .Distinct(StringComparer.Ordinal);
    }
}
