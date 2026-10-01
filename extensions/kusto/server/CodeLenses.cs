using System.Globalization;

/// <summary>
/// The lenses above each query: Run, or Running and Cancel, and what the last run of the same
/// query did. A lens that has to do something in the editor names one of Zed's actions through
/// `zed.dispatchAction`; the rest only show text.
/// </summary>
internal static class CodeLenses
{
    public const string DispatchActionCommand = "zed.dispatchAction";
    public const string NoopCommand = "kusto.noop";

    /// <summary>
    /// A query is a run of non-blank lines, the same rule the editor uses to pick the query to run.
    /// </summary>
    public static IEnumerable<QueryBlock> FindQueries(DocumentSnapshot document)
    {
        var lines = document.Text.Split('\n');
        var start = -1;
        for (var line = 0; line <= lines.Length; line++)
        {
            var blank = line == lines.Length || string.IsNullOrWhiteSpace(lines[line]);
            if (!blank && start < 0)
                start = line;
            if (blank && start >= 0)
            {
                var text = string.Join("\n", lines[start..line]).TrimEnd('\r');
                yield return new QueryBlock(start, line - 1, text);
                start = -1;
            }
        }
    }

    /// <summary>Braille spinner frames, one per refresh while a query runs.</summary>
    private const string SpinnerFrames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

    private const int FrameMilliseconds = 250;

    public static object[] For(DocumentSnapshot document, Dictionary<string, QueryRuns> runs, DateTimeOffset now)
    {
        var lenses = new List<object>();
        foreach (var block in FindQueries(document))
        {
            runs.TryGetValue(RunLog.Normalize(block.Text), out var state);
            var range = new
            {
                start = new { line = block.FirstLine, character = 0 },
                end = new { line = block.FirstLine, character = 0 }
            };
            void Add(string title, string command, params object[] arguments) =>
                lenses.Add(new { range, command = new { title, command, arguments } });

            if (state?.Running is { } running)
            {
                Add(Running(running, now), NoopCommand);
                Add("Cancel", DispatchActionCommand, "kusto::CancelQuery");
            }
            else
            {
                Add("▶ Run", DispatchActionCommand, "kusto::RunQuery");
            }

            if (state?.Last is { } last)
            {
                if (last.Failure is { } failure)
                {
                    Add($"Last run failed: {FirstLine(failure)}", NoopCommand);
                }
                else
                {
                    Add(Describe(last), NoopCommand);
                    if (last.ResultPath is { } path)
                        Add("Results", DispatchActionCommand, "kusto::ShowResult", new { path });
                }
                Add("Copy CID", DispatchActionCommand, "kusto::CopyClientRequestId", new { id = last.RunId });
            }
        }
        return lenses.ToArray();
    }

    /// <summary>`⠹ Running… 12 s`: the frame follows the clock, so each refresh moves the spinner.</summary>
    public static string Running(RunningRun run, DateTimeOffset now)
    {
        var frame = (int)(now.ToUnixTimeMilliseconds() / FrameMilliseconds % SpinnerFrames.Length);
        if (run.StartedAt is not { } started)
            return $"{SpinnerFrames[frame]} Running…";
        var elapsed = (int)Math.Max(0, (now - started).TotalSeconds);
        var time = elapsed < 60
            ? $"{elapsed} s"
            : $"{elapsed / 60} m {elapsed % 60:00} s";
        return $"{SpinnerFrames[frame]} Running… {time}";
    }

    private static string Describe(FinishedRun run)
    {
        var parts = new List<string>();
        if (run.StartedAt is { } started)
            parts.Add(started.ToLocalTime().ToString("HH:mm:ss", CultureInfo.InvariantCulture));
        if (run.DurationMilliseconds is { } milliseconds)
            parts.Add(milliseconds < 1000
                ? $"took {milliseconds} ms"
                : $"took {milliseconds / 1000.0:0.0} s");
        if (run.Rows is { } rows)
            parts.Add($"{rows.ToString("N0", CultureInfo.InvariantCulture)} {(rows == 1 ? "row" : "rows")}");
        return "Last run: " + string.Join(", ", parts);
    }

    private static string FirstLine(string text)
    {
        var line = text.Split('\n', 2)[0].Trim();
        return line.Length > 80 ? line[..79] + "…" : line;
    }
}

internal sealed record QueryBlock(int FirstLine, int LastLine, string Text);
