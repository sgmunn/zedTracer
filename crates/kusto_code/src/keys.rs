//! What one trace event says about the code that wrote it: its marker, a `Class.Method` at the
//! start of its message, and the application frames of any call stack in its text.

use std::sync::LazyLock;

use regex::Regex;

/// Only the top of a call stack is looked up; the frames below it are callers.
const MAX_FRAMES: usize = 5;

const FRAMEWORK_PREFIXES: &[&str] = &[
    "System.",
    "Microsoft.AspNetCore.",
    "Microsoft.Extensions.",
    "Microsoft.Win32.",
    "Newtonsoft.",
];

fn regex(pattern: &str) -> Regex {
    // The patterns are constants. A typo would surface in the first test that uses one.
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

const CHAIN: &str = r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+";

static LEADING_CHAIN: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"^\s*(?:\d+\s*/\s*\d+\s*:\s*)?({CHAIN})")));
static BRACKETED_CHAIN: LazyLock<Regex> = LazyLock::new(|| regex(&format!(r"\[\s*({CHAIN})\s*\]")));
// A frame can follow a real line break or a JSON-escaped one, which has no word boundary.
static STACK_FRAME: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?:^|\s|\\[rn])at\s+([A-Za-z_<][^\s(]*)\("));

/// The text of one event that can name code.
#[derive(Debug, Default, Clone, Copy)]
pub struct EventText<'a> {
    pub marker: Option<&'a str>,
    pub message: Option<&'a str>,
    /// Other text of the event that may hold a call stack, such as its custom data.
    pub details: Option<&'a str>,
    pub process: Option<&'a str>,
}

/// A namespace, a type and a method: `Microsoft.Dms.MetadataManager.GetThing`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolPath {
    pub namespace: Vec<String>,
    pub class_name: String,
    pub method_name: String,
}

impl SymbolPath {
    /// A dotted name read as `[namespace.]Class.Method`. Names that cannot be C# code are
    /// refused: both of the last two segments must start with an upper-case letter, which keeps
    /// host names and abbreviations such as "e.g." out.
    pub fn parse(text: &str) -> Option<Self> {
        let mut segments = normalized_segments(text);
        let method_name = segments.pop()?;
        let class_name = segments.pop()?;
        if !starts_upper_case(&method_name) || !starts_upper_case(&class_name) {
            return None;
        }
        Some(Self {
            namespace: segments,
            class_name,
            method_name,
        })
    }

    pub fn display(&self) -> String {
        format!("{}.{}", self.class_name, self.method_name)
    }

    /// The full dotted name, as it was written.
    pub fn full_name(&self) -> String {
        let mut parts: Vec<&str> = self.namespace.iter().map(String::as_str).collect();
        parts.push(&self.class_name);
        parts.push(&self.method_name);
        parts.join(".")
    }
}

fn starts_upper_case(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|first| first.is_ascii_uppercase())
}

/// Splits a type or frame name into plain segments. Generic arity (`List`1`) is dropped, `+`
/// separates nested types, an async state machine (`<Run>d__5.MoveNext`) reads as its method,
/// and so does a lambda or local function (`<Run>b__0`); the closure class around it is dropped.
fn normalized_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut skip_move_next = false;
    for part in text.split(['.', '+']) {
        let part = part.split_once('`').map_or(part, |(name, _)| name);
        if part.is_empty() {
            continue;
        }
        if let Some(generated) = part.strip_prefix('<') {
            let Some((name, tail)) = generated.split_once('>') else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            skip_move_next = tail.starts_with("d__");
            segments.push(name.to_string());
            continue;
        }
        if part == "MoveNext" && skip_move_next {
            skip_move_next = false;
            continue;
        }
        segments.push(part.to_string());
    }
    segments
}

/// The `Class.Method` an event's message starts with, or the first one in square brackets.
fn message_prefix(message: &str) -> Option<SymbolPath> {
    let chain = LEADING_CHAIN
        .captures(message)
        .or_else(|| BRACKETED_CHAIN.captures(message))?
        .get(1)?
        .as_str();
    // A long message is split into parts, and a later part can begin in the middle of a name:
    // `reading.Tasks.Task.Run` is what is left of `System.Threading.Tasks.Task.Run`.
    if !starts_upper_case(chain) {
        return None;
    }
    SymbolPath::parse(chain)
}

/// The application frames of the call stacks in `text`, top first.
fn stack_frames(text: &str) -> Vec<SymbolPath> {
    STACK_FRAME
        .captures_iter(text)
        .filter_map(|capture| capture.get(1))
        .map(|name| name.as_str())
        .filter(|name| {
            !FRAMEWORK_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .filter_map(SymbolPath::parse)
        .collect()
}

/// Everything an event offers to look its code up with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeKeys {
    pub marker: Option<String>,
    pub prefix: Option<SymbolPath>,
    pub frames: Vec<SymbolPath>,
    pub process: Option<String>,
}

impl CodeKeys {
    pub fn from_event(event: &EventText) -> Self {
        let mut frames: Vec<SymbolPath> = Vec::new();
        for text in [event.details, event.message].into_iter().flatten() {
            for frame in stack_frames(text) {
                if !frames.contains(&frame) {
                    frames.push(frame);
                }
            }
        }
        frames.truncate(MAX_FRAMES);

        Self {
            marker: non_empty(event.marker),
            prefix: event.message.and_then(message_prefix),
            frames,
            process: non_empty(event.process),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.marker.is_none() && self.prefix.is_none() && self.frames.is_empty()
    }
}

fn non_empty(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(marker: Option<&str>, message: Option<&str>, details: Option<&str>) -> CodeKeys {
        CodeKeys::from_event(&EventText {
            marker,
            message,
            details,
            process: None,
        })
    }

    fn symbol(namespace: &[&str], class_name: &str, method_name: &str) -> SymbolPath {
        SymbolPath {
            namespace: namespace.iter().map(|part| part.to_string()).collect(),
            class_name: class_name.to_string(),
            method_name: method_name.to_string(),
        }
    }

    #[test]
    fn reads_a_class_and_method_at_the_start_of_a_message() {
        let found = keys(None, Some("  SyncMetadataManager.Sync: starting"), None);
        assert_eq!(
            found.prefix,
            Some(symbol(&[], "SyncMetadataManager", "Sync"))
        );
    }

    #[test]
    fn keeps_the_namespace_of_a_longer_chain() {
        let found = keys(
            None,
            Some("Microsoft.Dms.MetadataManager.Refresh done"),
            None,
        );
        assert_eq!(
            found.prefix,
            Some(symbol(&["Microsoft", "Dms"], "MetadataManager", "Refresh"))
        );
    }

    #[test]
    fn reads_a_bracketed_chain_anywhere_in_the_message() {
        let found = keys(None, Some("Failed after 3 tries [Foo.Bar] giving up"), None);
        assert_eq!(found.prefix, Some(symbol(&[], "Foo", "Bar")));
    }

    #[test]
    fn skips_the_part_number_of_a_split_message() {
        let found = keys(None, Some("1/3: Foo.Bar started"), None);
        assert_eq!(found.prefix, Some(symbol(&[], "Foo", "Bar")));
    }

    #[test]
    fn ignores_text_that_cannot_be_code() {
        for message in [
            "api.contoso.com returned 500",
            "e.g. something happened",
            "version 1.5 released",
            "reading.Tasks.Task.FinishContinuations()",
            "Monitored scope start",
            "",
        ] {
            assert_eq!(keys(None, Some(message), None).prefix, None, "{message}");
        }
    }

    #[test]
    fn trims_the_marker_and_drops_an_empty_one() {
        assert_eq!(
            keys(Some(" A-B "), None, None).marker.as_deref(),
            Some("A-B")
        );
        assert_eq!(keys(Some("  "), None, None).marker, None);
        assert!(keys(Some(""), None, None).is_empty());
    }

    #[test]
    fn reads_frames_of_a_call_stack_and_drops_framework_frames() {
        let stack = "   at System.Threading.Tasks.Task.Wait()\n   at Contoso.Orders.OrderStore.Save(Order order) in C:\\__w\\1\\s\\src\\OrderStore.cs:line 42\n   at Contoso.Orders.Checkout.Run(Cart cart)";
        let found = keys(None, None, Some(stack));
        assert_eq!(
            found.frames,
            vec![
                symbol(&["Contoso", "Orders"], "OrderStore", "Save"),
                symbol(&["Contoso", "Orders"], "Checkout", "Run"),
            ]
        );
    }

    #[test]
    fn reads_frames_whose_line_breaks_are_json_escapes() {
        let stack = r"{\r\n   at Contoso.Store.Save()\r\nat Contoso.Store.Load()}";
        let found = keys(None, None, Some(stack));
        let names: Vec<String> = found.frames.iter().map(SymbolPath::display).collect();
        assert_eq!(names, ["Store.Save", "Store.Load"]);
    }

    #[test]
    fn reads_async_methods_and_lambdas_as_the_method() {
        let stack = "at Contoso.Orders.Checkout.<RunAsync>d__12.MoveNext()\nat Contoso.Orders.Checkout.<>c__DisplayClass3_0.<Wire>b__0(Object sender)\nat Contoso.Cache`1.Get(String key)";
        let found = keys(None, None, Some(stack));
        let names: Vec<String> = found.frames.iter().map(SymbolPath::display).collect();
        assert_eq!(names, ["Checkout.RunAsync", "Checkout.Wire", "Cache.Get"]);
    }

    #[test]
    fn treats_nested_types_as_the_innermost_class() {
        let found = keys(None, None, Some("at Contoso.Outer+Inner.Work()"));
        assert_eq!(
            found.frames,
            vec![symbol(&["Contoso", "Outer"], "Inner", "Work")]
        );
    }

    #[test]
    fn keeps_only_the_top_frames_without_repeats() {
        let mut stack = String::new();
        for index in 0..10 {
            stack.push_str(&format!("   at Contoso.Layer{index}.Step.Run()\n"));
        }
        stack.push_str("   at Contoso.Layer0.Step.Run()\n");
        let found = keys(None, None, Some(&stack));
        assert_eq!(found.frames.len(), MAX_FRAMES);
        assert_eq!(
            found.frames.first().map(SymbolPath::display).as_deref(),
            Some("Step.Run")
        );
    }

    #[test]
    fn full_name_gives_the_dotted_name_back() {
        let path = SymbolPath::parse("Contoso.Orders.Checkout.Run").expect("a symbol");
        assert_eq!(path.full_name(), "Contoso.Orders.Checkout.Run");
        assert_eq!(SymbolPath::parse("lower.case"), None);
        assert_eq!(SymbolPath::parse("Single"), None);
    }
}
