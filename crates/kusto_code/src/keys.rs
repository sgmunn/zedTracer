//! What one trace event says about the code that wrote it: its marker, a `Class.Method` at the
//! start of its message, and the application frames of any call stack in its text.

use std::sync::LazyLock;

use regex::Regex;

/// Only the top of a call stack is looked up; the frames below it are callers.
const MAX_FRAMES: usize = 5;

/// A run of a message shorter than this says too little to look for.
const MIN_LITERAL_LENGTH: usize = 12;
/// The longest runs of a message that are looked for in the source.
const MAX_LITERALS: usize = 2;
/// A run of a message is cut into this many words at most, and every stretch of them is a piece.
const MAX_WORDS_OF_A_LITERAL: usize = 24;
const MAX_MESSAGE_LENGTH: usize = 300;

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
// What a program fills into a message: a GUID, a long hex id, a number on its own (not the 2 of
// `S2S`), a quoted value, and the characters that source code escapes or interpolates. The two
// alternatives with `\B` are the quotes around a value, and not the apostrophe of "doesn't".
static FILLED_IN: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"[0-9a-fA-F]{8}-(?:[0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}|[0-9a-fA-F]{16,}|\b\d[\d.:/,\-]*[A-Za-z]{0,3}\b|["\\{}]|\B'|'\B"#,
    )
});
// A word of a message, cut after `:` and `=` because a value is often written straight after
// them (`metric:name`, `Reason=Allowed`).
static WORD: LazyLock<Regex> = LazyLock::new(|| regex(r"[^\s:=]*[:=]|[^\s:=]+"));
// Like `CHAIN`, and a segment may be a compiler-generated name, as in
// `FeatureFlightProvider.<IsEnabled>b__12_0`.
static ANY_CHAIN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b[A-Za-z_]\w*(?:\.(?:[A-Za-z_]\w*|<\w*>\w*))+"));
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

/// The `Class.Method` an event's message starts with, or else the first one in square brackets,
/// or else the first one anywhere in it ("Exiting from Store.Save. Took 3 ms").
fn message_prefix(message: &str) -> Option<SymbolPath> {
    let leading = LEADING_CHAIN
        .captures(message)
        .or_else(|| BRACKETED_CHAIN.captures(message))
        .and_then(|chain| chain.get(1))
        .map(|chain| chain.as_str());
    // A long message is split into parts, and a later part can begin in the middle of a name:
    // `reading.Tasks.Task.Run` is what is left of `System.Threading.Tasks.Task.Run`.
    if let Some(chain) = leading.filter(|chain| starts_upper_case(chain)) {
        return SymbolPath::parse(chain);
    }
    ANY_CHAIN
        .find_iter(message)
        .map(|chain| chain.as_str())
        .filter(|chain| starts_upper_case(chain))
        .filter(|chain| {
            !FRAMEWORK_PREFIXES
                .iter()
                .any(|prefix| chain.starts_with(prefix))
        })
        .find_map(SymbolPath::parse)
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

/// The runs of a message that may be text in the source, longest first, each as the pieces of
/// it that may be all the source has: every stretch of its words, longest first. Code that logs
/// `$"{path}, Resolved Lake Type: {type}"` has only `Resolved Lake Type:`, and code that logs
/// `$"Fetched details for {locator}"` only `Fetched details for`, though the message goes on with
/// what `locator` printed.
fn message_literals(message: &str) -> Vec<Vec<String>> {
    // The frames of a call stack are looked up as frames.
    if STACK_FRAME.is_match(message) {
        return Vec::new();
    }
    let Some(first_line) = message.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return Vec::new();
    };
    let first_line: String = first_line.chars().take(MAX_MESSAGE_LENGTH).collect();
    let mut runs: Vec<&str> = FILLED_IN
        .split(&first_line)
        .map(str::trim)
        .filter(|run| is_worth_looking_for(run))
        .collect();
    // Stable, so of two runs of one length the earlier stays first.
    runs.sort_by_key(|run| std::cmp::Reverse(run.chars().count()));
    runs.dedup();
    runs.truncate(MAX_LITERALS);
    runs.into_iter().map(pieces_of).collect()
}

fn is_worth_looking_for(text: &str) -> bool {
    text.chars().count() >= MIN_LITERAL_LENGTH && text.split_whitespace().nth(1).is_some()
}

fn pieces_of(run: &str) -> Vec<String> {
    let words: Vec<(usize, usize)> = WORD
        .find_iter(run)
        .take(MAX_WORDS_OF_A_LITERAL)
        .map(|word| (word.start(), word.end()))
        .collect();
    let mut pieces: Vec<String> = Vec::new();
    for (first, (start, _)) in words.iter().enumerate() {
        for (_, end) in &words[first..] {
            let Some(piece) = run.get(*start..*end) else {
                continue;
            };
            if is_worth_looking_for(piece) && !pieces.iter().any(|known| known == piece) {
                pieces.push(piece.to_string());
            }
        }
    }
    pieces.sort_by_key(|piece| std::cmp::Reverse(piece.chars().count()));
    pieces
}

/// Everything an event offers to look its code up with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeKeys {
    pub marker: Option<String>,
    /// A `Class.Method` the message starts with or otherwise names.
    pub prefix: Option<SymbolPath>,
    pub frames: Vec<SymbolPath>,
    /// The text of the message that code may have logged, from `message_literals`.
    pub literals: Vec<Vec<String>>,
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
            literals: event.message.map(message_literals).unwrap_or_default(),
            process: non_empty(event.process),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.marker.is_none()
            && self.prefix.is_none()
            && self.frames.is_empty()
            && self.literals.is_empty()
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

    fn literals_of(message: &str) -> Vec<Vec<String>> {
        keys(None, Some(message), None).literals
    }

    fn longest(literals: &[Vec<String>]) -> Vec<&str> {
        literals
            .iter()
            .filter_map(|pieces| pieces.first().map(String::as_str))
            .collect()
    }

    #[test]
    fn a_message_with_nothing_filled_in_is_looked_for_whole() {
        let literals =
            literals_of("Memory backed cloud cache already initialized. Taking fast path.");
        assert_eq!(
            longest(&literals),
            ["Memory backed cloud cache already initialized. Taking fast path."]
        );
    }

    #[test]
    fn guids_and_numbers_are_where_a_message_is_split() {
        let literals = literals_of(
            "Successfully fetched cluster details for [MetadataLocator: [TenantId=0d9fca32-3db8-4ce8-a52e-1c9557f46ebf, RealmId=, WorkspaceId=84980b9c-4cb0-40c2-a152-b70d5fca1109, CapacityId=]]",
        );
        assert_eq!(
            longest(&literals),
            [
                "Successfully fetched cluster details for [MetadataLocator: [TenantId=",
                ", RealmId=, WorkspaceId="
            ]
        );
        // The code probably logs the start and prints the rest with a value of its own.
        assert!(literals[0].contains(&"Successfully fetched cluster details for".to_string()));
        assert!(
            literals[0]
                .contains(&"fetched cluster details for [MetadataLocator: [TenantId=".to_string())
        );
    }

    #[test]
    fn the_pieces_are_longest_first_and_at_least_two_words() {
        let literals = literals_of("Removed the user from the group 42 successfully today");
        let pieces = &literals[0];
        assert_eq!(
            pieces.first().map(String::as_str),
            Some("Removed the user from the group")
        );
        assert!(pieces.windows(2).all(|pair| pair[0].len() >= pair[1].len()));
        assert!(pieces.iter().all(
            |piece| piece.split_whitespace().count() >= 2 && piece.len() >= MIN_LITERAL_LENGTH
        ));
        assert!(pieces.contains(&"the user from the group".to_string()));
        assert!(
            !pieces.contains(&"Removed the".to_string()),
            "too short to say anything"
        );
    }

    #[test]
    fn digits_inside_a_word_do_not_split_it_but_quoted_values_do() {
        assert_eq!(
            longest(&literals_of("Getting an S2S token for the service")),
            ["Getting an S2S token for the service"]
        );
        // The path is a value that was filled in, and "for reading" is too short to look for.
        assert_eq!(
            longest(&literals_of(
                "Could not open file 'C:\\data\\a.txt' for reading"
            )),
            ["Could not open file"]
        );
        // An apostrophe inside a word is not a quote.
        assert_eq!(
            longest(&literals_of("The service doesn't answer the health probe")),
            ["The service doesn't answer the health probe"]
        );
    }

    #[test]
    fn only_the_two_longest_runs_are_kept_and_short_messages_are_not_looked_for() {
        let literals = literals_of(
            "alpha beta gamma delta 1 short run 2 a considerably longer run of text 3 medium sized run here",
        );
        assert_eq!(
            longest(&literals),
            [
                "a considerably longer run of text",
                "alpha beta gamma delta"
            ]
        );
        for message in ["Done.", "Started it", "", "   ", "12345"] {
            assert_eq!(literals_of(message), Vec::<Vec<String>>::new(), "{message}");
        }
    }

    #[test]
    fn only_the_first_line_of_a_message_is_looked_for() {
        let literals =
            literals_of("\n  Lock acquired for the resource\nand then a second line of detail");
        assert_eq!(longest(&literals), ["Lock acquired for the resource"]);
    }

    #[test]
    fn code_is_looked_for_in_the_middle_of_the_text_too() {
        let literals = literals_of(
            "/Tables/50cf30cdd9cf4bb88e165e9747e26053_Query_0020_00282_0029_002Eparquet, Resolved Lake Type: ADLS",
        );
        let pieces = literals.first().expect("a run to look for");
        // The id is a value that was filled in, and so is the file name before the comma.
        assert!(!pieces.iter().any(|piece| piece.contains("50cf30cd")));
        assert!(pieces.contains(&"Resolved Lake Type:".to_string()));
    }

    #[test]
    fn a_value_written_straight_after_a_colon_or_equals_sign_is_cut_off() {
        let literals = literals_of("Successfully updated metric:OR-Validate-SuccessRate, value:");
        let pieces = literals.first().expect("a run to look for");
        assert!(pieces.contains(&"Successfully updated metric:".to_string()));
        let literals = literals_of("RateLimiter forwarding; Reason=Allowed; Method=GET");
        assert!(
            literals.first().is_some_and(
                |pieces| pieces.contains(&"RateLimiter forwarding; Reason=".to_string())
            )
        );
    }

    #[test]
    fn the_text_of_a_call_stack_is_not_looked_for_as_text() {
        assert_eq!(
            literals_of("n   at System.Runtime.CompilerServices.TaskAwaiter.Handle(Task task)"),
            Vec::<Vec<String>>::new()
        );
    }

    #[test]
    fn reads_a_class_and_method_named_anywhere_in_the_message() {
        let found = keys(
            None,
            Some("Exiting from FeatureFlightProvider.IsEnabledForTips. Runtime: 12"),
            None,
        );
        assert_eq!(
            found.prefix,
            Some(symbol(&[], "FeatureFlightProvider", "IsEnabledForTips"))
        );
        // At the start still wins over one later on.
        let found = keys(None, Some("Store.Save failed after Store.Load"), None);
        assert_eq!(found.prefix, Some(symbol(&[], "Store", "Save")));
        // Framework names are not looked for.
        let found = keys(
            None,
            Some("Failed in System.Net.Http.HttpClient.Send today"),
            None,
        );
        assert_eq!(found.prefix, None);
    }

    #[test]
    fn a_lambda_named_in_a_message_is_read_as_its_method() {
        let found = keys(
            None,
            Some("] Starting FeatureFlightProvider.<IsEnabledForTips>b__12_0. ParameterList:"),
            None,
        );
        assert_eq!(
            found.prefix,
            Some(symbol(&[], "FeatureFlightProvider", "IsEnabledForTips"))
        );
    }
}
