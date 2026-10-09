using System.Globalization;

/// <summary>
/// The lenses above each query: Run, or Running and Cancel, then Results (with its row count) when
/// the last run left one, and what the last run of the same query did. A lens that has to do something in the editor names one of Zed's actions through
/// `zed.dispatchAction`; the rest only show text.
/// </summary>
internal static class CodeLenses
{
    public const string DispatchActionCommand = "zed.dispatchAction";
    public const string NoopCommand = "kusto.noop";
    public const string ConnectionCommand = "kusto.connection";
    public const string RefreshSchemaCommand = "kusto.refreshSchema";

    /// <summary>Braille spinner frames, one per refresh while a query runs.</summary>
    private const string SpinnerFrames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

    private const int FrameMilliseconds = 250;

    public static object[] For(
        DocumentSnapshot document,
        Dictionary<string, QueryRuns> runs,
        DateTimeOffset now,
        Connection defaults,
        Func<string, string?> parameters,
        Func<Connection, SchemaStatus?> schemaOf)
    {
        var lenses = new List<object>();
        foreach (var block in QueryBlocks.Find(document.Text).Where(block => block.IsQuery))
        {
            var connection = ConnectionDirectives.Before(document.Text, block.End, defaults);
            runs.TryGetValue(RunLog.Key(block.Text, connection.Cluster, connection.Database), out var state);
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
            // Next to Run, so that it stays within reach when the lenses after it, the connection
            // above all, are longer than the editor is wide.
            if (state?.Last is { Failure: null, ResultPath: { } path } shown)
                Add(ResultsTitle(shown), DispatchActionCommand, "kusto::ShowResult", new { path });
            Add(Describe(connection), ConnectionCommand);
            if (schemaOf(connection) is { } schema)
            {
                // Zed asks the server to run this one, since it is not an action of the editor.
                Add(SchemaTitle(schema, now), RefreshSchemaCommand, connection.Cluster ?? "", connection.Database ?? "");
            }
            if (parameters(block.Text) is { } parametersTitle)
                Add(parametersTitle, DispatchActionCommand, "kusto::SelectParameterProfile");

            if (state?.Last is { } last)
            {
                if (last.Failure is { } failure)
                {
                    Add($"Last run failed: {FirstLine(failure)}", NoopCommand);
                }
                else
                {
                    // The rows are on the Results lens when there is one.
                    Add(Describe(last, withRows: last.ResultPath is null), NoopCommand);
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

    /// <summary>
    /// The lenses of a profiles file: `✓ Active` above the active profile and `Make Active` above
    /// each of the others, which asks the editor to change the file's `active:` line.
    /// </summary>
    public static object[] ForProfilesFile(string text)
    {
        var active = QueryParameters.Parse(text)?.Active;
        var lenses = new List<object>();
        foreach (var (name, line, column, length) in QueryParameters.ProfileKeys(text))
        {
            var range = new
            {
                start = new { line, character = column },
                end = new { line, character = column + length }
            };
            lenses.Add(name == active
                ? new { range, command = new { title = "✓ Active", command = NoopCommand, arguments = Array.Empty<object>() } }
                : new
                {
                    range,
                    command = new
                    {
                        title = "Make Active",
                        command = DispatchActionCommand,
                        arguments = new object[] { "kusto::MakeParameterProfileActive", new { name } }
                    }
                });
        }
        return lenses.ToArray();
    }

    /// <summary>`↻ Schema: 3 h ago`: when the schema the query is checked against was fetched. Clicking fetches it again.</summary>
    public static string SchemaTitle(SchemaStatus schema, DateTimeOffset now)
    {
        if (schema.InFlight)
            return schema.LoadedAt is null ? "↻ Schema: loading…" : "↻ Schema: refreshing…";
        if (schema.LoadedAt is not { } loaded)
            return "↻ Schema: not loaded";
        var age = now - loaded;
        var text = age < TimeSpan.FromMinutes(1) ? "just now"
            : age < TimeSpan.FromHours(1) ? $"{(int)age.TotalMinutes} min ago"
            : age < TimeSpan.FromHours(48) ? $"{(int)age.TotalHours} h ago"
            : $"{(int)age.TotalDays} d ago";
        return $"↻ Schema: {text}";
    }

    /// <summary>Where the query runs: `help.kusto.windows.net / Samples`.</summary>
    public static string Describe(Connection connection) =>
        connection.Host is not { } host
            ? "no cluster"
            : connection.Database is { } database ? $"{host} / {database}" : $"{host} / no database";

    /// <summary>`Results (1,240 rows)`: what clicking shows, and how much of it there is.</summary>
    public static string ResultsTitle(FinishedRun run) =>
        run.Rows is { } rows ? $"Results ({RowCount(rows)})" : "Results";

    private static string RowCount(long rows) =>
        $"{rows.ToString("N0", CultureInfo.InvariantCulture)} {(rows == 1 ? "row" : "rows")}";

    private static string Describe(FinishedRun run, bool withRows)
    {
        var parts = new List<string>();
        if (run.StartedAt is { } started)
            parts.Add(started.ToLocalTime().ToString("HH:mm:ss", CultureInfo.InvariantCulture));
        if (run.DurationMilliseconds is { } milliseconds)
            parts.Add(milliseconds < 1000
                ? $"took {milliseconds} ms"
                : $"took {milliseconds / 1000.0:0.0} s");
        if (withRows && run.Rows is { } rows)
            parts.Add(RowCount(rows));
        return "Last run: " + string.Join(", ", parts);
    }

    private static string FirstLine(string text)
    {
        var line = text.Split('\n', 2)[0].Trim();
        return line.Length > 80 ? line[..79] + "…" : line;
    }
}
