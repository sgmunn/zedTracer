/// <summary>
/// A file holds several queries, each a run of non-blank lines, with blank lines between them.
/// That is how Kusto Explorer reads a file and how the editor chooses the query to run. A blank
/// line inside parentheses or braces that are still open, such as a gap in the body of a function,
/// does not end the query; <see cref="QueryBlocks.Find"/> says when it does. Each
/// query is analysed on its own, so a query never continues into the one after it and a `let`
/// in one is not visible in the next. A block of only comments, such as a directive or a note, is
/// not a query, and neither is a block of function definitions (see <see cref="InlineFunctions"/>).
/// The editor finds the same blocks (`query_text.rs`), which the cases in
/// `fork-docs/samples/query-blocks.json` keep true.
/// </summary>
internal sealed record QueryBlock(int FirstLine, int LastLine, int Start, string Text, bool IsQuery, bool IsFunctions = false)
{
    public int End => Start + Text.Length;
}

internal static class QueryBlocks
{
    /// <summary>
    /// The blocks of a text. A blank line ends a block unless a bracket the block opened is still open
    /// (a gap in a function body or a call), or a multi-line string is. Brackets that are never closed
    /// are no gap: they would join every query below to this one, so a block whose brackets are open at
    /// the end of the text ends at its first blank line instead.
    /// </summary>
    public static List<QueryBlock> Find(string text)
    {
        var blocks = new List<QueryBlock>();
        var lines = text.Split('\n');
        var starts = new int[lines.Length];
        var offset = 0;
        for (var line = 0; line < lines.Length; line++)
        {
            starts[line] = offset;
            offset += lines[line].Length + 1;
        }

        var first = 0;
        while (first < lines.Length)
        {
            if (string.IsNullOrWhiteSpace(lines[first]))
            {
                first++;
                continue;
            }
            var last = LastLineOfBlock(lines, first);
            var contents = lines[first..(last + 1)].Where(line => !string.IsNullOrWhiteSpace(line)).ToList();
            var isFunctions = contents.Any(ConnectionDirectives.IsFunctionsMarker);
            var isQuery = !isFunctions && contents.Any(line => !line.TrimStart().StartsWith("//", StringComparison.Ordinal));
            var queryText = text[starts[first]..(starts[last] + lines[last].Length)].TrimEnd('\r', '\n');
            blocks.Add(new QueryBlock(first, last, starts[first], queryText, isQuery, isFunctions));
            first = last + 1;
        }
        return blocks;
    }

    private static int LastLineOfBlock(string[] lines, int first)
    {
        var brackets = new Brackets();
        var last = first;
        for (var line = first; line < lines.Length; line++)
        {
            if (!string.IsNullOrWhiteSpace(lines[line]))
            {
                brackets.Follow(lines[line]);
                last = line;
            }
            else if (!brackets.IsOpen)
            {
                return last;
            }
        }
        if (!brackets.IsOpen)
            return last;
        var blank = Array.FindIndex(lines, first, line => string.IsNullOrWhiteSpace(line));
        return blank < 0 ? lines.Length - 1 : blank - 1;
    }

    /// <summary>
    /// Follows the brackets of the lines it is given, to tell whether a blank line comes inside parentheses
    /// or braces or between two queries. Brackets in comments and strings do not count. A string of one
    /// quote ends with its line, so a quote left open while typing does not hold the rest of the file.
    /// </summary>
    private sealed class Brackets
    {
        private int depth;
        private string? multilineString;

        public bool IsOpen => depth > 0 || multilineString is not null;

        public void Follow(string line)
        {
            var index = 0;
            while (index < line.Length)
            {
                if (multilineString is { } delimiter)
                {
                    var end = line.IndexOf(delimiter, index, StringComparison.Ordinal);
                    if (end < 0)
                        return;
                    index = end + delimiter.Length;
                    multilineString = null;
                    continue;
                }
                switch (line[index])
                {
                    case '/' when index + 1 < line.Length && line[index + 1] == '/':
                        return;
                    case '`' or '~' when index + 2 < line.Length && line[index + 1] == line[index] && line[index + 2] == line[index]:
                        multilineString = new string(line[index], 3);
                        index += 3;
                        break;
                    case '\'' or '"':
                        index = EndOfString(line, index);
                        break;
                    case '(' or '{':
                        depth++;
                        index++;
                        break;
                    case ')' or '}':
                        depth = Math.Max(0, depth - 1);
                        index++;
                        break;
                    default:
                        index++;
                        break;
                }
            }
        }

        /// <summary>
        /// Where the string of one quote that starts at <paramref name="start"/> ends, or the end of the line.
        /// A verbatim string (`@'…'`) has no escapes and doubles its quote; any other takes a backslash as one.
        /// </summary>
        private static int EndOfString(string line, int start)
        {
            var quote = line[start];
            var verbatim = start > 0 && line[start - 1] == '@';
            var index = start + 1;
            while (index < line.Length)
            {
                if (line[index] == quote)
                {
                    if (verbatim && index + 1 < line.Length && line[index + 1] == quote)
                        index += 2;
                    else
                        return index + 1;
                }
                else
                {
                    index += line[index] == '\\' && !verbatim ? 2 : 1;
                }
            }
            return line.Length;
        }
    }

    /// <summary>
    /// Whether a query is a control command such as `.show tables`: its first line that is not blank
    /// or a comment starts with a dot. The editor decides this the same way before it runs one.
    /// </summary>
    public static bool IsControlCommand(string query) =>
        query.Split('\n')
            .Select(line => line.Trim())
            .FirstOrDefault(line => line.Length > 0 && !line.StartsWith("//", StringComparison.Ordinal))
            ?.StartsWith('.') == true;

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
