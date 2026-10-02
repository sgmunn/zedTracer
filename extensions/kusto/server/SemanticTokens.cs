using Kusto.Language;
using Kusto.Language.Editor;

/// <summary>
/// The colours of a document, as LSP semantic tokens. Kusto's own analysis classifies every part of
/// a query, so this knows all of the language (`declare`, `print`, `.show` commands, every operator)
/// and, once a schema has loaded, tells tables from columns from functions, which no grammar that
/// only reads the text can.
/// </summary>
internal static class SemanticTokens
{
    /// <summary>The token types the server uses, in the order of the legend it sends.</summary>
    public static readonly string[] Legend =
    [
        "comment", "keyword", "string", "number", "type", "class", "namespace",
        "function", "property", "variable", "parameter", "operator"
    ];

    private static int TypeOf(ClassificationKind kind) => kind switch
    {
        ClassificationKind.Comment => 0,
        ClassificationKind.Keyword or ClassificationKind.QueryOperator or ClassificationKind.ScalarOperator
            or ClassificationKind.Command or ClassificationKind.Directive => 1,
        ClassificationKind.StringLiteral => 2,
        ClassificationKind.Literal => 3,
        ClassificationKind.Type => 4,
        ClassificationKind.Table or ClassificationKind.MaterializedView => 5,
        ClassificationKind.Database => 6,
        ClassificationKind.Function => 7,
        ClassificationKind.Column or ClassificationKind.SchemaMember or ClassificationKind.Option => 8,
        ClassificationKind.Variable => 9,
        ClassificationKind.Parameter or ClassificationKind.ClientParameter or ClassificationKind.QueryParameter
            or ClassificationKind.SignatureParameter => 10,
        ClassificationKind.MathOperator => 11,
        // Punctuation, plain text and a name the analysis could not place are left to the theme's default.
        _ => -1
    };

    /// <summary>
    /// The tokens of every query in the document, each analysed against the schema of the cluster and
    /// database it runs on, in the relative encoding LSP uses: five numbers per token.
    /// </summary>
    public static int[] Build(DocumentSnapshot document, Func<Connection, GlobalState> globalsFor, Connection defaults)
    {
        var data = new List<int>();
        var previousLine = 0;
        var previousColumn = 0;

        void Add(int offset, int length, int type)
        {
            var (line, column) = document.LineAndColumn(offset);
            data.Add(line - previousLine);
            data.Add(line == previousLine ? column - previousColumn : column);
            data.Add(length);
            data.Add(type);
            data.Add(0);
            previousLine = line;
            previousColumn = column;
        }

        foreach (var block in QueryBlocks.Find(document.Text))
        {
            var connection = ConnectionDirectives.Before(document.Text, block.End, defaults);
            var service = new KustoCodeService(block.Text, globalsFor(connection));
            foreach (var span in service.GetClassifications(0, block.Text.Length).Classifications)
            {
                var type = TypeOf(span.Kind);
                if (type < 0 || span.Length <= 0)
                    continue;
                // A token on one line at a time suits every client; a multi-line string or comment is split.
                var start = block.Start + span.Start;
                var end = start + span.Length;
                while (start < end)
                {
                    var lineEnd = document.Text.IndexOf('\n', start, end - start);
                    var segmentEnd = lineEnd < 0 ? end : lineEnd;
                    var length = segmentEnd - start;
                    if (length > 0 && document.Text[segmentEnd - 1] == '\r')
                        length--;
                    if (length > 0)
                        Add(start, length, type);
                    start = lineEnd < 0 ? end : lineEnd + 1;
                }
            }
        }
        return data.ToArray();
    }
}
