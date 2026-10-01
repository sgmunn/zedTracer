/// <summary>
/// A file holds several queries, each a run of non-blank lines, with blank lines between them.
/// That is how Kusto Explorer reads a file and how the editor chooses the query to run. Each
/// query is analysed on its own, so a query never continues into the one after it and a `let`
/// in one is not visible in the next.
/// </summary>
internal sealed record QueryBlock(int FirstLine, int LastLine, int Start, string Text)
{
    public int End => Start + Text.Length;
}

internal static class QueryBlocks
{
    public static List<QueryBlock> Find(string text)
    {
        var blocks = new List<QueryBlock>();
        var lines = text.Split('\n');
        var lineStart = 0;
        var firstLine = -1;
        var firstStart = 0;
        for (var line = 0; line <= lines.Length; line++)
        {
            var blank = line == lines.Length || string.IsNullOrWhiteSpace(lines[line]);
            if (!blank && firstLine < 0)
            {
                firstLine = line;
                firstStart = lineStart;
            }
            if (blank && firstLine >= 0)
            {
                var queryText = text[firstStart..Math.Min(lineStart, text.Length)].TrimEnd('\r', '\n');
                blocks.Add(new QueryBlock(firstLine, line - 1, firstStart, queryText));
                firstLine = -1;
            }
            if (line < lines.Length)
                lineStart += lines[line].Length + 1;
        }
        return blocks;
    }

    /// <summary>The query the offset is in, or at the end of; null on a blank line between queries.</summary>
    public static QueryBlock? At(IReadOnlyList<QueryBlock> blocks, int offset) =>
        blocks.FirstOrDefault(block => offset >= block.Start && offset <= block.End);

    /// <summary>
    /// The text to analyse for a position and where that text starts in the file. A blank line
    /// is a query that has not been started yet, so it gets empty text.
    /// </summary>
    public static (string Text, int Start) Around(string documentText, int offset)
    {
        var block = At(Find(documentText), offset);
        return block is null ? ("", offset) : (block.Text, block.Start);
    }
}
