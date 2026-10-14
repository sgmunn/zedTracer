using System.Text.RegularExpressions;
using YamlDotNet.Core;
using YamlDotNet.RepresentationModel;

/// <summary>A named set of values for the parameters queries declare.</summary>
internal sealed record ParameterProfile(string Name, IReadOnlyDictionary<string, string> Values);

/// <summary>The profiles of one file. `Active` is a name from `Profiles`, or null.</summary>
internal sealed record ParameterProfiles(string? Active, IReadOnlyList<ParameterProfile> Profiles)
{
    public static readonly ParameterProfiles Empty = new(null, []);

    public ParameterProfile? ActiveProfile =>
        Active is null ? null : Profiles.FirstOrDefault(profile => profile.Name == Active);
}

internal sealed record DeclaredParameter(string Name, bool HasDefault);

/// <summary>A parameter a profile or the file has a value for, that a declaration may name. `Order` sorts the nearer sources first.</summary>
internal sealed record KnownParameter(string Name, string Detail, string Order);

/// <summary>
/// The query parameter profiles that apply to a query file, which the editor reads the same way
/// when it runs the query: the `.parameters.yaml` beside the file when there is one, and otherwise
/// `.kusto/parameters.yaml` in the project.
/// </summary>
internal static partial class QueryParameters
{
    [GeneratedRegex(@"^[A-Za-z_][A-Za-z0-9_]*$")]
    private static partial Regex ParameterName();

    [GeneratedRegex(@"\bdeclare\s+query_parameters\s*\(", RegexOptions.IgnoreCase)]
    private static partial Regex Declaration();

    /// <summary>Whether a text can be the name of a parameter.</summary>
    public static bool IsParameterName(string name) => ParameterName().IsMatch(name);

    /// <summary>Whether a document is a profiles file: `.kusto/parameters.yaml`, or `<name>.parameters.yaml`.</summary>
    public static bool IsProfilesFile(string documentUri)
    {
        if (!Uri.TryCreate(documentUri, UriKind.Absolute, out var uri) || !uri.IsFile)
            return false;
        var path = uri.LocalPath.Replace('\\', '/');
        return path.EndsWith("/.kusto/parameters.yaml", StringComparison.OrdinalIgnoreCase)
            || path.EndsWith(".parameters.yaml", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Whether a document is YAML, of any kind, which is never a query.</summary>
    public static bool IsYaml(string documentUri) =>
        documentUri.EndsWith(".yaml", StringComparison.OrdinalIgnoreCase)
        || documentUri.EndsWith(".yml", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// The files to look in for a document, nearest first: the `.parameters.yaml` beside it, and the
    /// project's `.kusto/parameters.yaml`, from the workspace folder that holds it or else the first.
    /// </summary>
    public static IReadOnlyList<string> FilesFor(string documentUri, IReadOnlyList<string> workspaceFolders)
    {
        var files = new List<string>();
        if (!Uri.TryCreate(documentUri, UriKind.Absolute, out var uri) || !uri.IsFile)
            return files;
        var path = uri.LocalPath;
        if (path.EndsWith(".kql", StringComparison.Ordinal))
            files.Add(path[..^".kql".Length] + ".parameters.yaml");
        if (FolderOf(path, workspaceFolders) is { } folder)
            files.Add(Path.Combine(folder, ".kusto", "parameters.yaml"));
        return files;
    }

    /// <summary>
    /// The workspace folder that holds a document, or the first when none does: a file outside every
    /// folder, such as a query thread's, is still served for the project, and the editor takes the
    /// project's first folder for it too. Null when there is no project at all.
    /// </summary>
    public static string? FolderOf(string? documentPath, IReadOnlyList<string> workspaceFolders) =>
        workspaceFolders
            .Where(folder => documentPath is not null
                && documentPath.StartsWith(folder.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar,
                    StringComparison.Ordinal))
            .OrderByDescending(folder => folder.Length)
            .FirstOrDefault()
        ?? workspaceFolders.FirstOrDefault();

    /// <summary>
    /// The profiles that apply: from the first of `files` that exists, with that file's name. A
    /// file that cannot be read has no profiles and an error.
    /// </summary>
    public static (ParameterProfiles Profiles, string? Path, bool Unreadable) Load(IEnumerable<string> files)
    {
        foreach (var file in files)
        {
            if (!File.Exists(file))
                continue;
            try
            {
                return Parse(File.ReadAllText(file)) is { } profiles
                    ? (profiles, file, false)
                    : (ParameterProfiles.Empty, file, true);
            }
            catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
            {
                return (ParameterProfiles.Empty, file, true);
            }
        }
        return (ParameterProfiles.Empty, null, false);
    }

    /// <summary>
    /// What the lens above a query says about its parameters, or null when the query declares
    /// none: the active profile, the declared parameters the file gives a value with directives
    /// (`Incident A + file: raid`), and the declared parameters nothing has a value for.
    /// </summary>
    public static string? Describe(
        string query,
        (ParameterProfiles Profiles, string? Path, bool Unreadable) loaded,
        IReadOnlyDictionary<string, string> givenByFile)
    {
        var declared = Declared(query);
        if (declared.Count == 0)
            return null;
        if (loaded.Unreadable)
            return $"Params: cannot read {Path.GetFileName(loaded.Path)}";
        var active = loaded.Profiles.ActiveProfile;
        var fromFile = declared
            .Where(parameter => givenByFile.ContainsKey(parameter.Name))
            .Select(parameter => parameter.Name)
            .ToList();
        if (active is null && fromFile.Count == 0)
            return "Params: none";
        var missing = declared
            .Where(parameter => !parameter.HasDefault
                && !givenByFile.ContainsKey(parameter.Name)
                && active?.Values.ContainsKey(parameter.Name) != true)
            .Select(parameter => parameter.Name)
            .ToList();
        var shown = fromFile.Count == 0 ? "" : $"file: {string.Join(", ", fromFile)}";
        var title = active is null ? shown : fromFile.Count == 0 ? active.Name : $"{active.Name} + {shown}";
        return missing.Count == 0
            ? $"Params: {title}"
            : $"Params: {title} (no value for {string.Join(", ", missing)})";
    }

    /// <summary>Null when the text is not a profiles file.</summary>
    public static ParameterProfiles? Parse(string text)
    {
        var stream = new YamlStream();
        try
        {
            stream.Load(new StringReader(text));
        }
        catch (YamlException)
        {
            return null;
        }
        if (stream.Documents.Count == 0)
            return ParameterProfiles.Empty;
        switch (stream.Documents[0].RootNode)
        {
            case YamlScalarNode scalar when IsNull(scalar):
                return ParameterProfiles.Empty;
            case not YamlMappingNode:
                return null;
        }

        var root = (YamlMappingNode)stream.Documents[0].RootNode;
        var profiles = new List<ParameterProfile>();
        if (Find(root, "profiles") is { } found)
        {
            if (found is YamlMappingNode mapping)
            {
                foreach (var (name, values) in mapping.Children)
                {
                    if (Text(name) is not { } profileName || values is not YamlMappingNode valueMapping)
                        continue;
                    var pairs = new Dictionary<string, string>();
                    foreach (var (key, value) in valueMapping.Children)
                    {
                        if (Text(key) is { } parameter && ParameterName().IsMatch(parameter) && Text(value) is { } shown)
                            pairs[parameter] = shown;
                    }
                    profiles.Add(new ParameterProfile(profileName, pairs));
                }
            }
            else if (found is not YamlScalarNode nothing || !IsNull(nothing))
            {
                return null;
            }
        }

        var active = Find(root, "active") is { } activeNode ? Text(activeNode) : null;
        return new ParameterProfiles(
            profiles.Any(profile => profile.Name == active) ? active : null,
            profiles);
    }

    /// <summary>The profiles a file names, where each name is written (0-based line and column) and how long it is.</summary>
    public static List<(string Name, int Line, int Column, int Length)> ProfileKeys(string text)
    {
        var keys = new List<(string, int, int, int)>();
        var stream = new YamlStream();
        try
        {
            stream.Load(new StringReader(text));
        }
        catch (YamlException)
        {
            return keys;
        }
        if (stream.Documents.Count == 0
            || stream.Documents[0].RootNode is not YamlMappingNode root
            || Find(root, "profiles") is not YamlMappingNode profiles)
            return keys;
        foreach (var (key, _) in profiles.Children)
        {
            if (key is YamlScalarNode { Value: { } name } && key.Start.Line > 0)
                keys.Add((name, (int)key.Start.Line - 1, (int)key.Start.Column - 1, (int)(key.End.Index - key.Start.Index)));
        }
        return keys;
    }

    private static YamlNode? Find(YamlMappingNode mapping, string key) =>
        mapping.Children.FirstOrDefault(pair => pair.Key is YamlScalarNode { Value: var value } && value == key).Value;

    private static bool IsNull(YamlScalarNode node) =>
        node.Style == YamlDotNet.Core.ScalarStyle.Plain && node.Value is null or "" or "~" or "null";

    private static string? Text(YamlNode node) =>
        node is YamlScalarNode scalar && !IsNull(scalar) ? scalar.Value : null;

    /// <summary>The parameters a query declares with `declare query_parameters(...)`, and whether each has a default.</summary>
    public static List<DeclaredParameter> Declared(string query)
    {
        var text = WithoutComments(query);
        var declared = new List<DeclaredParameter>();
        foreach (Match found in Declaration().Matches(text))
        {
            var depth = 1;
            char? quote = null;
            var segmentStart = found.Index + found.Length;
            var segments = new List<string>();
            for (var at = segmentStart; at < text.Length; at++)
            {
                var character = text[at];
                if (quote is { } open)
                {
                    if (character == open)
                        quote = null;
                    continue;
                }
                switch (character)
                {
                    case '\'' or '"':
                        quote = character;
                        break;
                    case '(':
                        depth++;
                        break;
                    case ')':
                        depth--;
                        break;
                    case ',' when depth == 1:
                        segments.Add(text[segmentStart..at]);
                        segmentStart = at + 1;
                        break;
                }
                if (depth == 0)
                {
                    segments.Add(text[segmentStart..at]);
                    break;
                }
            }

            foreach (var segment in segments)
            {
                var trimmed = segment.TrimStart();
                var name = new string(trimmed.TakeWhile(character => char.IsLetterOrDigit(character) || character == '_').ToArray());
                if (ParameterName().IsMatch(name) && declared.All(existing => existing.Name != name))
                    declared.Add(new DeclaredParameter(name, HasDefault(trimmed)));
            }
        }
        return declared;
    }

    /// <summary>
    /// Where a new parameter name is being written in a `declare query_parameters(...)` at `offset`:
    /// the start and end of the name typed so far, or null when the cursor is elsewhere (after a
    /// colon, in a default value, outside the parentheses).
    /// </summary>
    public static (int Start, int End)? NameAt(string query, int offset)
    {
        var text = WithoutCommentsAndStrings(query);
        var limit = Math.Clamp(offset, 0, text.Length);
        var afterOpeningParenthesis = Declaration().Matches(text).Select(found => found.Index + found.Length).ToHashSet();
        var depth = 0;
        var segmentStart = 0;
        for (var at = 0; at < limit; at++)
        {
            if (depth == 0)
            {
                if (afterOpeningParenthesis.Contains(at + 1))
                {
                    depth = 1;
                    segmentStart = at + 1;
                }
                continue;
            }
            switch (text[at])
            {
                case '(':
                    depth++;
                    break;
                case ')':
                    depth--;
                    break;
                case ',' when depth == 1:
                    segmentStart = at + 1;
                    break;
            }
        }
        if (depth != 1 || PartialName().Match(text[segmentStart..limit]) is not { Success: true } typed)
            return null;
        var end = limit;
        while (end < text.Length && (char.IsLetterOrDigit(text[end]) || text[end] == '_'))
            end++;
        return (limit - typed.Groups[1].Length, end);
    }

    [GeneratedRegex(@"^\s*([A-Za-z_][A-Za-z0-9_]*)?$")]
    private static partial Regex PartialName();

    /// <summary>The text with the inside of comments and string literals blanked, so that positions stay the same.</summary>
    private static string WithoutCommentsAndStrings(string query)
    {
        var blanked = new System.Text.StringBuilder(query);
        char? quote = null;
        for (var at = 0; at < query.Length; at++)
        {
            var character = query[at];
            if (quote is { } open)
            {
                if (character == '\\' && at + 1 < query.Length)
                {
                    blanked[at] = ' ';
                    blanked[++at] = ' ';
                }
                else if (character == open)
                    quote = null;
                else if (character is not ('\n' or '\r'))
                    blanked[at] = ' ';
            }
            else if (character == '/' && at + 1 < query.Length && query[at + 1] == '/')
            {
                for (; at < query.Length && query[at] != '\n'; at++)
                {
                    if (query[at] != '\r')
                        blanked[at] = ' ';
                }
                at--;
            }
            else if (character is '\'' or '"')
            {
                quote = character;
            }
        }
        return blanked.ToString();
    }

    /// <summary>
    /// The parameters worth offering when one is declared: the ones the file gives a value to with
    /// `// :setParam`, then those the active profile has, then those only another profile has.
    /// </summary>
    public static List<KnownParameter> Known(ParameterProfiles profiles, IReadOnlyDictionary<string, string> givenByFile)
    {
        var known = new List<KnownParameter>();
        void Add(string name, string detail, string order)
        {
            if (known.All(existing => existing.Name != name))
                known.Add(new KnownParameter(name, detail, order));
        }
        foreach (var name in givenByFile.Keys)
            Add(name, "Given a value in this file with // :setParam", "0");
        if (profiles.ActiveProfile is { } active)
        {
            foreach (var name in active.Values.Keys)
                Add(name, $"A value in the profile {active.Name}", "1");
        }
        foreach (var profile in profiles.Profiles)
        {
            foreach (var name in profile.Values.Keys)
                Add(name, $"A value in the profile {profile.Name}", "2");
        }
        return known;
    }

    private static bool HasDefault(string segment)
    {
        var depth = 0;
        char? quote = null;
        foreach (var character in segment)
        {
            if (quote is { } open)
            {
                if (character == open)
                    quote = null;
                continue;
            }
            switch (character)
            {
                case '\'' or '"':
                    quote = character;
                    break;
                case '(':
                    depth++;
                    break;
                case ')':
                    depth--;
                    break;
                case '=' when depth == 0:
                    return true;
            }
        }
        return false;
    }

    private static string WithoutComments(string query) =>
        string.Join("\n", query.Split('\n').Select(line =>
        {
            char? quote = null;
            char? previous = null;
            for (var index = 0; index < line.Length; index++)
            {
                var character = line[index];
                if (quote is { } open)
                {
                    if (character == open)
                        quote = null;
                }
                else if (character == '/' && previous == '/')
                {
                    return line[..(index - 1)];
                }
                else if (character is '\'' or '"')
                {
                    quote = character;
                }
                previous = character;
            }
            return line;
        }));
}
