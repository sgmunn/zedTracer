//! Reading C# source text without parsing it: declarations on a line, and the method or type
//! around a line.
//!
//! This relies on the layout the code is formatted in (a block is indented more than its
//! header). It is a label for a search hit, not an analysis; a syntax tree replaces it where
//! one is available.

use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context as _, Result};
use regex::Regex;

fn regex(pattern: &str) -> Regex {
    // The patterns are constants. A typo would surface in the first test that uses one.
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

static TYPE_DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"^\s*(?:\[[^\]]*\]\s*)*(?:[a-z]+\s+)*(?:class|struct|record|interface)\s+([A-Za-z_]\w*)")
});
static STRING_CONSTANT: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?:const|static\s+readonly)\s+string\s+([A-Za-z_]\w*)\s*=\s*""#));
static NAMESPACE: LazyLock<Regex> = LazyLock::new(|| regex(r"^\s*namespace\s+([A-Za-z_][\w.]*)"));
static MARKER_ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^\s*\[[^\]]*\bCodeMarkerName\w*\s*\("));
static GENERIC_ARGUMENTS: LazyLock<Regex> = LazyLock::new(|| regex(r"<[^<>]*>"));
static LEADING_ATTRIBUTES: LazyLock<Regex> = LazyLock::new(|| regex(r"^(?:\[[^\]]*\]\s*)+"));
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| regex(r"^[A-Za-z_]\w*$"));

/// Words a statement can start with, which tell it from a declaration that also has parentheses.
const STATEMENT_WORDS: &[&str] = &[
    "return", "await", "throw", "new", "else", "yield", "using", "if", "while", "for", "foreach",
    "switch", "catch", "lock", "case", "goto", "var", "base", "this", "where", "when", "fixed",
];

/// Words that end a line that is not a declaration, or that are modifiers, never a method's name:
/// `async` is what `action: async (x) =>` ends in before its parenthesis.
const NOT_A_NAME: &[&str] = &[
    "async",
    "await",
    "static",
    "public",
    "private",
    "protected",
    "internal",
    "override",
    "virtual",
    "abstract",
    "sealed",
    "extern",
    "unsafe",
    "partial",
    "readonly",
    "const",
    "ref",
    "out",
    "in",
    "params",
    "delegate",
    "operator",
    "var",
    "new",
    "return",
];

pub fn read_lines(path: &Path) -> Result<Vec<String>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_string)
        .collect())
}

pub fn is_comment(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
}

/// Whether the string on `line` that holds `piece` of a message has text that the message does
/// not, in the order the string has it, so that the line did not log the message. What a
/// program fills into a string (an interpolation, a format item, an escape) may be anything, and
/// a line that is not understood, as when a string goes on past it, contradicts nothing.
pub fn contradicts_message(line: &str, piece: &str, message: &str) -> bool {
    let Some(strings) = string_fragments(line) else {
        return false;
    };
    let Some(fragments) = strings
        .iter()
        .find(|fragments| fragments.iter().any(|fragment| fragment.contains(piece)))
    else {
        return false;
    };
    // Taking the first place each fragment is at leaves the most of the message for the rest.
    let mut rest = message;
    for fragment in fragments {
        match rest.find(fragment.as_str()) {
            Some(start) => rest = rest.get(start + fragment.len()..).unwrap_or_default(),
            None => return true,
        }
    }
    false
}

/// The strings on a line of code, each as its runs of text between what a program fills in.
/// `None` when the line has a string that it does not close, or one this does not read.
fn string_fragments(line: &str) -> Option<Vec<Vec<String>>> {
    // A raw string, or a quote that is a character, would be read as the bounds of a string.
    if line.contains(r#"""""#) || line.contains(r#"'"'"#) || line.contains(r#"'\"'"#) {
        return None;
    }
    let characters: Vec<char> = line.chars().collect();
    let at = |index: usize| characters.get(index).copied();
    let mut strings = Vec::new();
    let mut index = 0;
    while let Some(character) = at(index) {
        index += 1;
        if character != '"' {
            continue;
        }
        let before = |back: usize| index.checked_sub(back + 1).and_then(at);
        let verbatim = before(1) == Some('@') || (before(1) == Some('$') && before(2) == Some('@'));
        let interpolated =
            before(1) == Some('$') || (before(1) == Some('@') && before(2) == Some('$'));
        let mut fragments = Vec::new();
        let mut current = String::new();
        let mut end_fragment = |current: &mut String| {
            let fragment = current.trim();
            if !fragment.is_empty() {
                fragments.push(fragment.to_string());
            }
            current.clear();
        };
        loop {
            let character = at(index)?;
            index += 1;
            match character {
                '"' if verbatim && at(index) == Some('"') => {
                    index += 1;
                    current.push('"');
                }
                '"' => break,
                '\\' if !verbatim => {
                    index += 1;
                    end_fragment(&mut current);
                }
                '{' if at(index) != Some('{') => {
                    end_fragment(&mut current);
                    index = if interpolated {
                        end_of_interpolation(&characters, index)?
                    } else {
                        // A format item; a quote in it ends the string all the same.
                        let mut end = index;
                        while let Some(character) = at(end).filter(|character| *character != '"') {
                            end += 1;
                            if character == '}' {
                                break;
                            }
                        }
                        end
                    };
                }
                '{' | '}' => {
                    index += usize::from(at(index) == Some(character));
                    end_fragment(&mut current);
                }
                '%' => {
                    end_fragment(&mut current);
                    while at(index).is_some_and(|character| "-+ #.0123456789".contains(character)) {
                        index += 1;
                    }
                    if at(index).is_some_and(|character| character.is_ascii_alphabetic()) {
                        index += 1;
                    }
                }
                _ => current.push(character),
            }
        }
        end_fragment(&mut current);
        strings.push(fragments);
    }
    Some(strings)
}

/// Where an interpolation ends, from just after its `{`: past the `}` that closes it, over any
/// braces and strings in its expression.
fn end_of_interpolation(characters: &[char], mut index: usize) -> Option<usize> {
    let mut depth = 1;
    while depth > 0 {
        match characters.get(index)? {
            '{' => depth += 1,
            '}' => depth -= 1,
            '"' => loop {
                index += 1;
                match characters.get(index)? {
                    '\\' => index += 1,
                    '"' => break,
                    _ => {}
                }
            },
            _ => {}
        }
        index += 1;
    }
    Some(index)
}

/// The name a `class`, `struct`, `record` or `interface` declaration on this line introduces.
pub fn type_declaration_name(line: &str) -> Option<String> {
    TYPE_DECLARATION
        .captures(line)
        .and_then(|capture| capture.get(1))
        .map(|name| name.as_str().to_string())
}

/// Whether the line is a `[CodeMarkerName(...)]` attribute, and not just a use of the property
/// of the same name, as in `CodeMarkers.Name.Instance.CodeMarkerName`.
pub fn is_marker_attribute(line: &str) -> bool {
    MARKER_ATTRIBUTE.is_match(line)
}

/// The name of `const string Name = "..."` or `static readonly string Name = "..."`.
pub fn string_constant_name(line: &str) -> Option<String> {
    STRING_CONSTANT
        .captures(line)
        .and_then(|capture| capture.get(1))
        .map(|name| name.as_str().to_string())
}

/// The name a method (or constructor) declaration on this line introduces. A call, a
/// statement and a lambda are refused.
pub fn method_declaration_name(line: &str) -> Option<String> {
    if is_comment(line) {
        return None;
    }
    let trimmed =
        line.trim_start_matches(|character: char| character == '}' || character.is_whitespace());
    let without_attributes = LEADING_ATTRIBUTES.replace(trimmed, "");
    // Generic arguments go first: a tuple in a return type, `Task<(int A, int B)>`, has parentheses.
    let text = without_generic_arguments(&without_attributes);
    let head = text.get(..text.find('(')?)?;
    // A colon is a named argument (`action: async (x) =>`), a label or a base call; `::` is not.
    if head.contains('=') || head.replace("::", "").contains(':') {
        return None;
    }

    let words: Vec<&str> = head.split_whitespace().collect();
    let (first, name) = (words.first()?, words.last()?);
    if words.len() < 2
        || STATEMENT_WORDS.contains(first)
        || NOT_A_NAME.contains(name)
        || !IDENTIFIER.is_match(name)
    {
        return None;
    }
    Some((*name).to_string())
}

/// `text` without anything in angle brackets, nested ones included.
fn without_generic_arguments(text: &str) -> String {
    let mut reduced = text.to_string();
    loop {
        let next = GENERIC_ARGUMENTS.replace_all(&reduced, "").into_owned();
        if next == reduced {
            return reduced;
        }
        reduced = next;
    }
}

/// The base types and interfaces in the declaration of the type on the line at `index`
/// (0-based), which may continue on the next line. Interface names by the `IName` convention
/// are left out, since a method is looked for in base classes.
pub fn base_types(lines: &[String], index: usize) -> Vec<String> {
    let Some(line) = lines.get(index) else {
        return Vec::new();
    };
    let Some(name) = type_declaration_name(line) else {
        return Vec::new();
    };
    let Some((_, after_name)) = line.split_once(name.as_str()) else {
        return Vec::new();
    };
    let declaration = without_generic_arguments(after_name);
    let declaration = declaration
        .split(" where ")
        .next()
        .unwrap_or_default()
        .to_string();
    let list = match declaration.split_once(':') {
        Some((_, list)) => list.to_string(),
        None => match lines.get(index + 1).map(|next| next.trim()) {
            Some(next) if next.starts_with(':') => without_generic_arguments(next)
                .split(" where ")
                .next()
                .unwrap_or_default()
                .trim_start_matches(':')
                .to_string(),
            _ => return Vec::new(),
        },
    };
    list.split('{')
        .next()
        .unwrap_or_default()
        .split(',')
        .filter_map(|base| {
            let base = base.split('(').next().unwrap_or_default().trim();
            let base = base.rsplit('.').next().unwrap_or_default();
            let is_interface = base.starts_with('I')
                && base
                    .chars()
                    .nth(1)
                    .is_some_and(|second| second.is_ascii_uppercase());
            (IDENTIFIER.is_match(base) && !is_interface).then(|| base.to_string())
        })
        .collect()
}

fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|character| character.is_whitespace())
        .map(|character| if character == '\t' { 4 } else { 1 })
        .sum()
}

/// Walks up from `index` through the headers of the blocks it is in. A header is the nearest line
/// above with less indentation; an opening brace on a line of its own belongs to the header just
/// above it. `inspect` sees each header and says whether it is one wanted, and the walk goes on
/// outwards until a type or a namespace. Preprocessor lines (`#if`, `#endif`) start in the first
/// column whatever they are inside, so they say nothing about the nesting.
fn find_enclosing<T>(
    lines: &[String],
    index: usize,
    mut inspect: impl FnMut(&str) -> Option<T>,
) -> Vec<T> {
    let mut found = Vec::new();
    let Some(line_at_index) = lines.get(index) else {
        return found;
    };
    // A closing brace is in the block it closes, whose header has the same indentation.
    let closes_a_block = line_at_index.trim_start().starts_with('}');
    let mut limit = indent(line_at_index) + usize::from(closes_a_block);
    for earlier in (0..index).rev() {
        let Some(line) = lines.get(earlier) else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || is_comment(line) || trimmed.starts_with('#') {
            continue;
        }
        let line_indent = indent(line);
        if line_indent >= limit {
            continue;
        }
        if trimmed == "{" {
            limit = line_indent + 1;
            continue;
        }
        if let Some(wanted) = inspect(line) {
            found.push(wanted);
        } else if type_declaration_name(line).is_some() || trimmed.starts_with("namespace ") {
            break;
        }
        limit = line_indent;
    }
    found
}

/// The method that contains the line at `index` (0-based): the innermost one, which is a local
/// function when the line is in one.
pub fn enclosing_method(lines: &[String], index: usize) -> Option<String> {
    enclosing_methods(lines, index).into_iter().next()
}

/// Every method that contains the line at `index` (0-based), the innermost first: a local
/// function, then the method it is declared in.
pub fn enclosing_methods(lines: &[String], index: usize) -> Vec<String> {
    find_enclosing(lines, index, method_declaration_name)
}

/// The type that contains the line at `index` (0-based).
pub fn enclosing_type(lines: &[String], index: usize) -> Option<String> {
    find_enclosing(lines, index, type_declaration_name)
        .into_iter()
        .next()
}

/// The first type declared within `within` lines below `index` (0-based), skipping attributes
/// and comments, with the 0-based line it is on.
pub fn type_declared_below(
    lines: &[String],
    index: usize,
    within: usize,
) -> Option<(String, usize)> {
    for later in index + 1..=index + within {
        let line = lines.get(later)?;
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('[')
            || trimmed.starts_with('#')
            || is_comment(line)
        {
            continue;
        }
        return type_declaration_name(line).map(|name| (name, later));
    }
    None
}

/// The namespace a file declares, from its first lines.
pub fn namespace_of(lines: &[String]) -> Option<String> {
    lines.iter().take(100).find_map(|line| {
        NAMESPACE
            .captures(line)
            .and_then(|capture| capture.get(1))
            .map(|name| name.as_str().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<String> {
        source.lines().map(str::to_string).collect()
    }

    fn line_of(source: &[String], needle: &str) -> usize {
        source
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line holds {needle}"))
    }

    #[test]
    fn a_string_with_text_the_message_lacks_contradicts_it() {
        let message = "DmsSqlWorkspaceClient.UpdateSqlWorkspaceCapacityUnitAsync => workspace ID: f6cc0055-1bc9-49d6-87c6-f30dcc1e4ec6 new CapacityID: 0a7334bd-8b42-4422-8fbd-0c459d30e189 sqlWorkspaceUpdateMutex acquired to update the sql workspace.";
        let piece = "=> workspace ID:";
        for line in [
            r#"CTracer.LogMessage($"{nameof(DmsSqlWorkspaceClient)}.{nameof(this.UpdateSqlWorkspaceCapacityUnitAsync)} => workspace ID: {workspaceId} cloned execution context, proceeding to CU update.");"#,
            r#"CTracer.LogMessage($"{nameof(this.UpdateSqlWorkspaceCapacityUnitAsync)} => workspace ID: {workspaceId} attempt to clone execution context started");"#,
            // In the message, but not in this order.
            r#"Log($"new CapacityID: {capacityId} => workspace ID: {workspaceId}");"#,
        ] {
            assert!(contradicts_message(line, piece, message), "{line}");
        }
        for line in [
            r#"CTracer.LogMessage($"{nameof(DmsSqlWorkspaceClient)}.{nameof(this.UpdateSqlWorkspaceCapacityUnitAsync)} => workspace ID: {workspaceId} new CapacityID: {capacityId}" +"#,
            r#"Log(string.Format("{0} => workspace ID: {1,-8} new CapacityID: {2}", a, b, c));"#,
            r#"Log("Other", $"{(ok ? "a" : "b")} => workspace ID: {id}");"#,
            r#"log.info("%s => workspace ID: %-8s new CapacityID: %s", a, b, c)"#,
            // A string this does not read, or no string that holds the piece.
            r#"Log($"=> workspace ID: {id} and more"#,
            r##"Log("""=> workspace ID: {id} and more""");"##,
            r#"var text = "x"; // => workspace ID: is in a comment"#,
        ] {
            assert!(!contradicts_message(line, piece, message), "{line}");
        }
    }

    #[test]
    fn escapes_and_escaped_braces_in_a_string_match_anything() {
        let message = "Retrying the call\n\tafter a \"transient\" failure {code=7}";
        for line in [
            r#"Log($"Retrying the call\n\tafter a \"transient\" failure {{code={code}}}");"#,
            r#"Log(@"Retrying the call" + "\n\tafter a ""transient"" failure");"#,
        ] {
            assert!(
                !contradicts_message(line, "Retrying the call", message),
                "{line}"
            );
        }
        assert!(contradicts_message(
            r#"Log($"Retrying the call\nafter a permanent failure");"#,
            "Retrying the call",
            message
        ));
    }

    #[test]
    fn reads_type_declarations() {
        for (line, name) in [
            (
                "    public sealed class BlockedWorkspace : Base<BlockedWorkspace> { }",
                "BlockedWorkspace",
            ),
            ("internal static partial class Helpers", "Helpers"),
            ("public readonly record struct Point(int X)", "Point"),
            ("    [Serializable] public interface IThing", "IThing"),
            ("class Plain<T>", "Plain"),
        ] {
            assert_eq!(type_declaration_name(line).as_deref(), Some(name), "{line}");
        }
        for line in [
            "// class Commented",
            "var x = new Foo();",
            "    return classify(x);",
        ] {
            assert_eq!(type_declaration_name(line), None, "{line}");
        }
    }

    #[test]
    fn tells_the_marker_attribute_from_a_use_of_its_property() {
        for line in [
            r#"        [CodeMarkerName("A-B")]"#,
            "        [CodeMarkerName(codeMarker: Name)]",
            r#"        [CodeMarkerName("A-B")] public sealed class AB { }"#,
            r#"[Obsolete, CodeMarkerName("A-B")]"#,
        ] {
            assert!(is_marker_attribute(line), "{line}");
        }
        for line in [
            "using (new MonitoredScope(CodeMarkers.AB.Instance.CodeMarkerName))",
            r#"// [CodeMarkerName("A-B")]"#,
            "var name = marker.CodeMarkerName;",
        ] {
            assert!(!is_marker_attribute(line), "{line}");
        }
    }

    #[test]
    fn reads_string_constants() {
        assert_eq!(
            string_constant_name(r#"        public const string WriteStates = "Service-Write";"#)
                .as_deref(),
            Some("WriteStates")
        );
        assert_eq!(
            string_constant_name(r#"static readonly string Marker = "x";"#).as_deref(),
            Some("Marker")
        );
        assert_eq!(string_constant_name(r#"var text = "x";"#), None);
    }

    #[test]
    fn reads_method_declarations_and_refuses_statements() {
        for (line, name) in [
            (
                "        public async Task<List<int>> LoadAsync(string key)",
                "LoadAsync",
            ),
            (
                "        private static void Run<T, U>(T first, U second)",
                "Run",
            ),
            ("        [Fact] public void Works()", "Works"),
            ("        public Store(IFoo foo)", "Store"),
            ("        public int Count() => items.Count;", "Count"),
            ("        Task Save();", "Save"),
            (
                "public static async Task<(string Id, DatamartState State)> GetInfo(string key)",
                "GetInfo",
            ),
        ] {
            assert_eq!(
                method_declaration_name(line).as_deref(),
                Some(name),
                "{line}"
            );
        }
        for line in [
            "            Save(order);",
            "            await Save(order);",
            "            return Build(order);",
            "            var order = Build(cart);",
            "            if (order.IsEmpty)",
            "            } else if (ready)",
            "            using (new MonitoredScope(CodeMarkers.Save))",
            "            foreach (var item in items)",
            "            throw new InvalidOperationException(message);",
            "            items.Select(item => Build(item));",
            "            : base(name)",
            "        // public void Old()",
        ] {
            assert_eq!(method_declaration_name(line), None, "{line}");
        }
    }

    const SOURCE: &str = "\
namespace Contoso.Orders
{
    public class Checkout
    {
        public Checkout(IStore store)
        {
            this.store = store;
        }

        public async Task RunAsync(
            Cart cart,
            CancellationToken token)
        {
            if (cart.IsEmpty)
            {
                return;
            }

            using (new MonitoredScope(CodeMarkers.Charge))
            {
                await this.store.SaveAsync(cart);
            }
        }

        private void Other() { Log(); }
    }
}
";

    #[test]
    fn finds_the_method_around_a_line() {
        let source = lines(SOURCE);
        for (needle, method) in [
            ("this.store = store", "Checkout"),
            ("return;", "RunAsync"),
            ("this.store.SaveAsync", "RunAsync"),
            ("new MonitoredScope", "RunAsync"),
        ] {
            assert_eq!(
                enclosing_method(&source, line_of(&source, needle)).as_deref(),
                Some(method),
                "{needle}"
            );
        }
    }

    #[test]
    fn a_line_outside_any_method_has_none() {
        let source = lines(SOURCE);
        assert_eq!(
            enclosing_method(&source, line_of(&source, "public class Checkout")),
            None
        );
        assert_eq!(enclosing_method(&source, 0), None);
    }

    #[test]
    fn finds_the_type_around_a_line() {
        let source = lines(SOURCE);
        assert_eq!(
            enclosing_type(&source, line_of(&source, "this.store.SaveAsync")).as_deref(),
            Some("Checkout")
        );
    }

    #[test]
    fn finds_the_type_declared_below_an_attribute() {
        let source = lines(
            "\
        /// <summary>Marker</summary>
        [CodeMarkerName(\"A-B\")]
        [Obsolete]
        public sealed class AB : Base<AB> { }
",
        );
        assert_eq!(
            type_declared_below(&source, 1, 6),
            Some(("AB".to_string(), 3))
        );
        assert_eq!(type_declared_below(&source, 3, 6), None);
    }

    #[test]
    fn reads_the_namespace() {
        let source = lines("// header\nnamespace Contoso.Orders\n{\n}");
        assert_eq!(namespace_of(&source).as_deref(), Some("Contoso.Orders"));
        assert_eq!(namespace_of(&lines("class A { }")), None);
        assert_eq!(
            namespace_of(&lines("namespace Contoso.Orders;\nclass A { }")).as_deref(),
            Some("Contoso.Orders")
        );
    }

    #[test]
    fn reads_the_base_types_of_a_declaration() {
        let source = lines(
            "\
public class Plain
public class Derived<T> : BaseThing<T>, IDisposable, Other.Namespaced where T : class
public record Point(int X) : Shape(X)
public sealed class Wrapped
    : Parent
{",
        );
        assert_eq!(base_types(&source, 0), Vec::<String>::new());
        assert_eq!(base_types(&source, 1), ["BaseThing", "Namespaced"]);
        assert_eq!(base_types(&source, 2), ["Shape"]);
        assert_eq!(base_types(&source, 3), ["Parent"]);
        assert_eq!(base_types(&source, 99), Vec::<String>::new());
    }

    const WITH_PREPROCESSOR_LINES: &str = "\
namespace Contoso.Orders
{
    public class Forwarder
    {
        public async Task<Response> ForwardAsync(
            Request request,
            Uri uri)
        {
#if NETFRAMEWORK
            request.Legacy = true;
#elif NETCOREAPP
            request.Legacy = false;
#endif
            var response = await this.client.SendAsync(
                request,
                async (result) =>
                {
                    return result.Succeeded(true);
                });
            return response;
        }
    }
}
";

    #[test]
    fn a_line_in_the_first_column_does_not_end_the_walk_up_to_the_method() {
        let source = lines(WITH_PREPROCESSOR_LINES);
        for needle in ["request.Legacy = false", "return response;", "request,"] {
            assert_eq!(
                enclosing_method(&source, line_of(&source, needle)).as_deref(),
                Some("ForwardAsync"),
                "{needle}"
            );
        }
    }

    #[test]
    fn a_lambda_given_as_a_named_argument_is_not_a_method() {
        for line in [
            "                action: async (certifiedEvent) =>",
            "                func: async ms =>",
            "                }, exceptionFilter: (Exception e) =>",
            "            await this.Run(async (token) =>",
        ] {
            assert_eq!(method_declaration_name(line), None, "{line}");
        }
        let source = lines(WITH_PREPROCESSOR_LINES);
        assert_eq!(
            enclosing_method(&source, line_of(&source, "return result.Succeeded")).as_deref(),
            Some("ForwardAsync")
        );
    }

    #[test]
    fn a_line_in_a_local_function_is_in_it_and_in_the_method_around_it() {
        let source = lines(
            "\
public class Cache
{
    public async Task<int> PerformAsync()
    {
        async Task<int> WithTimeout()
        {
            return await this.Work();
        }

        return await WithTimeout();
    }
}
",
        );
        let inside = line_of(&source, "return await this.Work");
        assert_eq!(
            enclosing_methods(&source, inside),
            ["WithTimeout", "PerformAsync"]
        );
        assert_eq!(
            enclosing_method(&source, inside).as_deref(),
            Some("WithTimeout")
        );
        assert_eq!(
            enclosing_methods(&source, line_of(&source, "return await WithTimeout")),
            ["PerformAsync"]
        );
    }

    #[test]
    fn a_closing_brace_is_in_the_method_it_closes() {
        let source = lines(WITH_PREPROCESSOR_LINES);
        let closing = source
            .iter()
            .rposition(|text| text.trim() == "}")
            .unwrap_or(0);
        // The last two closing braces close the class and the namespace.
        assert_eq!(enclosing_method(&source, closing), None);
        let method_end = source
            .iter()
            .position(|text| text.contains("return response;"))
            .map(|index| index + 1)
            .unwrap_or(0);
        assert_eq!(source.get(method_end).map(|text| text.trim()), Some("}"));
        assert_eq!(
            enclosing_method(&source, method_end).as_deref(),
            Some("ForwardAsync")
        );
    }
}
