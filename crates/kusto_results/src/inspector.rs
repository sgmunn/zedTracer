//! What the Row Details inspector shows for a selection.
//!
//! Everything here produces display text. Nothing is written back to the result, and copy
//! from the grid still copies the source values.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::result::{Cell, Table};

/// A frame containing any of these is runtime plumbing rather than application code.
const FRAMEWORK_FRAME_MARKERS: &[&str] = &[
    "System.Threading.Tasks.",
    "System.Threading._IOCompletionCallback.",
    "System.Threading.ExecutionContext",
    "System.Threading.ThreadPool",
    "System.Runtime.CompilerServices.",
    "System.Runtime.",
    "System.Net.",
    "System.IO.",
    "System.Text.Json.",
    "System.Diagnostics.",
    "System.Collections.",
    "Polly.",
];

fn regex(pattern: &str) -> Regex {
    // The patterns are constants. A typo would surface in the first test that formats a stack.
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

static SOURCE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?:[A-Za-z]:\\|\\\\)[^:\r\n]*\\([^\\/:]+\.[A-Za-z0-9]+)\s*:\s*line\s*([0-9]+)")
});
static ESCAPED_BREAK_BEFORE_FRAME: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?:\\[rn])+(\s*at\s)"));
static ESCAPED_BREAK_AT_END: LazyLock<Regex> = LazyLock::new(|| regex(r"(?:\\[rn])+\s*$"));
static REAL_BREAK: LazyLock<Regex> = LazyLock::new(|| regex(r"[\r\n]+"));
static FRAME_START: LazyLock<Regex> = LazyLock::new(|| regex(r"\bat\s+[\w<]"));
static ASYNC_STATE_MACHINE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\.<([^>]+)>d__[0-9]+\.MoveNext\(\)"));
static GENERATED_LAMBDA: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"[.+]<>c(?:__DisplayClass[0-9]+(?:_[0-9]+)?)?\.<([^>]+)>b__[0-9]+(?:_[0-9]+)?\([^)]*\)")
});
static MULTIPART_MARKER: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?s)^([0-9]+)\s*/\s*([0-9]+)\s*:\s?(.*)$"));

/// Reduces an exception call stack to its useful frames, one per line.
///
/// Source paths shrink to `File.cs:line N`. Async state machines and compiler-generated
/// lambdas read as their method. Runtime frames are dropped, except that a stack made only of
/// them is kept whole.
///
/// Paths are shortened before literal `\r` and `\n` escape sequences are treated as line
/// breaks, and only escapes that precede a frame or end the text count, so a directory such as
/// `\repos\node\` inside a path is left intact.
pub fn format_call_stack(call_stack: &str) -> String {
    let shortened = SOURCE_PATH.replace_all(call_stack, "$1:line $2");
    let shortened = ESCAPED_BREAK_BEFORE_FRAME.replace_all(&shortened, " $1");
    let shortened = ESCAPED_BREAK_AT_END.replace_all(&shortened, "");
    let normalized = REAL_BREAK.replace_all(&shortened, " ");

    // Split into frames before filtering, so a line holding both an application frame and a
    // runtime frame never hides the application frame.
    let mut starts: Vec<usize> = FRAME_START
        .find_iter(&normalized)
        .map(|found| found.start())
        .collect();
    starts.insert(0, 0);
    starts.push(normalized.len());
    let frames: Vec<String> = starts
        .windows(2)
        .filter_map(|bounds| normalized.get(bounds[0]..bounds[1]))
        .map(str::trim)
        .filter(|frame| !frame.is_empty())
        .map(|frame| {
            let frame = ASYNC_STATE_MACHINE.replace_all(frame, ".$1()");
            GENERATED_LAMBDA.replace_all(&frame, ".$1()").into_owned()
        })
        .collect();

    let application_frames: Vec<&String> = frames
        .iter()
        .filter(|frame| {
            !FRAMEWORK_FRAME_MARKERS
                .iter()
                .any(|marker| frame.contains(marker))
        })
        .collect();
    if application_frames.is_empty() {
        frames.join("\n")
    } else {
        application_frames
            .iter()
            .map(|frame| frame.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Applies the call stack formatting to every string property named `callStack` (any case), at
/// any depth.
pub fn format_call_stacks_in_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(format_call_stacks_in_json).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| {
                    let formatted = match child {
                        Value::String(stack) if key.eq_ignore_ascii_case("callstack") => {
                            Value::String(format_call_stack(stack))
                        }
                        other => format_call_stacks_in_json(other),
                    };
                    (key.clone(), formatted)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The structured value in a cell: an object or array held in a dynamic column, or a string
/// that starts with `{` or `[` and parses. Scalars such as `123` or `"text"` are ordinary
/// values and return `None`.
pub fn structured_json(cell: &Cell) -> Option<Value> {
    match cell {
        Cell::Dynamic(value) => Some(value.as_ref().clone()),
        Cell::Text(text) => structured_json_text(text),
        _ => None,
    }
}

pub fn structured_json_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    serde_json::from_str::<Value>(trimmed).ok()
}

/// Pretty-printed JSON with two-space indentation, call stacks trimmed, and escaped newlines
/// inside strings shown as real line breaks.
pub fn format_json_for_display(value: &Value) -> String {
    let pretty = serde_json::to_string_pretty(&format_call_stacks_in_json(value))
        .unwrap_or_else(|_| value.to_string());
    show_escaped_newlines(&pretty)
}

/// Turns the `\n` escape inside JSON strings into a line break. A backslash that is itself
/// escaped (`\\n`) is left alone.
fn show_escaped_newlines(json: &str) -> String {
    let mut shown = String::with_capacity(json.len());
    let mut in_string = false;
    let mut chars = json.chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => {
                in_string = !in_string;
                shown.push(character);
            }
            '\\' if in_string => match chars.next() {
                Some('n') => shown.push('\n'),
                Some(escaped) => {
                    shown.push('\\');
                    shown.push(escaped);
                }
                None => shown.push('\\'),
            },
            other => shown.push(other),
        }
    }
    shown
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTokenKind {
    Key,
    String,
    Number,
    Boolean,
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonToken {
    pub range: Range<usize>,
    pub kind: JsonTokenKind,
}

/// Colour spans for JSON display text. Ranges are byte offsets and never change the text.
pub fn highlight_json(text: &str) -> Vec<JsonToken> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let mut end = index + 1;
            while end < bytes.len() {
                match bytes[end] {
                    b'\\' => end += 2,
                    b'"' => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
            let end = end.min(bytes.len());
            let mut next = end;
            while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                next += 1;
            }
            let kind = if bytes.get(next) == Some(&b':') {
                JsonTokenKind::Key
            } else {
                JsonTokenKind::String
            };
            tokens.push(JsonToken {
                range: index..end,
                kind,
            });
            index = end;
            continue;
        }
        if let Some(length) = number_length(&bytes[index..]) {
            if ends_token(bytes.get(index + length)) {
                tokens.push(JsonToken {
                    range: index..index + length,
                    kind: JsonTokenKind::Number,
                });
                index += length;
                continue;
            }
        }
        let keyword = [
            ("true", JsonTokenKind::Boolean),
            ("false", JsonTokenKind::Boolean),
            ("null", JsonTokenKind::Null),
        ]
        .into_iter()
        .find(|(word, _)| {
            bytes[index..].starts_with(word.as_bytes()) && ends_token(bytes.get(index + word.len()))
        });
        if let Some((word, kind)) = keyword {
            tokens.push(JsonToken {
                range: index..index + word.len(),
                kind,
            });
            index += word.len();
            continue;
        }
        index += 1;
    }
    tokens
}

fn ends_token(next: Option<&u8>) -> bool {
    next.is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b']' | b'}'))
}

fn number_length(bytes: &[u8]) -> Option<usize> {
    let mut index = 0;
    if bytes.first() == Some(&b'-') {
        index += 1;
    }
    match bytes.get(index)? {
        b'0' => index += 1,
        b'1'..=b'9' => {
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        _ => return None,
    }
    if bytes.get(index) == Some(&b'.') && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let mut exponent = index + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            while bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
                exponent += 1;
            }
            index = exponent;
        }
    }
    Some(index)
}

/// A message that was logged as several rows, each value starting `k/N:`, put back together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedField {
    pub column: usize,
    pub column_name: String,
    pub total: usize,
    pub text: String,
}

struct Part {
    number: usize,
    total: usize,
    text: String,
}

fn parse_part(cell: Option<&Cell>) -> Option<Part> {
    let Cell::Text(text) = cell? else {
        return None;
    };
    let captures = MULTIPART_MARKER.captures(text)?;
    let number: usize = captures[1].parse().ok()?;
    let total: usize = captures[2].parse().ok()?;
    (total >= 2 && (1..=total).contains(&number)).then(|| Part {
        number,
        total,
        text: captures[3].to_string(),
    })
}

/// Assembles the selected rows, column by column, when together they form one complete
/// multipart message: two or more rows, every one carrying a marker in that column, all
/// declaring the same total, as many rows as the total, and parts 1 to N each exactly once.
/// Anything less assembles nothing, so unrelated rows stay separate.
pub fn assemble_multipart(table: &Table, rows: &[usize]) -> Vec<MergedField> {
    if rows.len() < 2 {
        return Vec::new();
    }
    let mut merged = Vec::new();
    for (column_index, column) in table.columns.iter().enumerate() {
        let parts: Option<Vec<Part>> = rows
            .iter()
            .map(|row| parse_part(table.cell(*row, column_index)))
            .collect();
        let Some(mut parts) = parts else {
            continue;
        };
        let total = parts[0].total;
        if total != rows.len() || parts.iter().any(|part| part.total != total) {
            continue;
        }
        parts.sort_by_key(|part| part.number);
        if parts
            .iter()
            .enumerate()
            .any(|(position, part)| part.number != position + 1)
        {
            continue;
        }
        merged.push(MergedField {
            column: column_index,
            column_name: column.name.clone(),
            total,
            text: parts.into_iter().map(|part| part.text).collect(),
        });
    }
    merged
}

/// What the inspector shows for the current selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorSubject {
    /// `Select a result row to inspect its values here.`
    Empty,
    Row {
        row: usize,
    },
    Assembled {
        rows: Vec<usize>,
        fields: Vec<MergedField>,
    },
    /// Several rows that are not one multipart message: the first is shown with a count.
    FirstOfMany {
        first_row: usize,
        selected_count: usize,
    },
}

/// Resolves a selection, given as source rows in display order.
pub fn resolve_subject(table: &Table, selected_rows: &[usize]) -> InspectorSubject {
    let rows: Vec<usize> = selected_rows
        .iter()
        .copied()
        .filter(|row| *row < table.rows.len())
        .collect();
    match rows.as_slice() {
        [] => InspectorSubject::Empty,
        [row] => InspectorSubject::Row { row: *row },
        [first, ..] => {
            let fields = assemble_multipart(table, &rows);
            if fields.is_empty() {
                InspectorSubject::FirstOfMany {
                    first_row: *first,
                    selected_count: rows.len(),
                }
            } else {
                InspectorSubject::Assembled { rows, fields }
            }
        }
    }
}

/// How one value is presented in a field block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Null,
    /// Pretty-printed JSON; colour it with [`highlight_json`].
    Json(String),
    Text(String),
}

pub fn field_value(cell: &Cell) -> FieldValue {
    if cell.is_null() {
        return FieldValue::Null;
    }
    match structured_json(cell) {
        Some(value) => FieldValue::Json(format_json_for_display(&value)),
        None => FieldValue::Text(cell.display_text().into_owned()),
    }
}

/// How an assembled message is presented: as JSON when it parses as such, otherwise as text.
pub fn merged_field_value(text: &str) -> FieldValue {
    match structured_json_text(text) {
        Some(value) => FieldValue::Json(format_json_for_display(&value)),
        None => FieldValue::Text(text.to_string()),
    }
}

/// The whole inspector body as one text, with the spans that are styled differently.
///
/// Every range is a byte range into `text`, and none of the styling changes the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectorDocument {
    pub text: String,
    /// The line naming each field and its type.
    pub headers: Vec<Range<usize>>,
    /// Values that are null, shown as the word `null`.
    pub nulls: Vec<Range<usize>>,
    pub json_tokens: Vec<JsonToken>,
    /// The values shown as JSON, which are set apart from plain values as code (JSN-2).
    pub code_blocks: Vec<Range<usize>>,
}

impl InspectorDocument {
    fn push_field(&mut self, header: &str, value: &FieldValue) {
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        let start = self.text.len();
        self.text.push_str(header);
        self.headers.push(start..self.text.len());
        self.text.push('\n');
        let start = self.text.len();
        match value {
            FieldValue::Null => {
                self.text.push_str("null");
                self.nulls.push(start..self.text.len());
            }
            FieldValue::Text(text) => self.text.push_str(text),
            FieldValue::Json(text) => {
                self.text.push_str(text);
                self.code_blocks.push(start..self.text.len());
                self.json_tokens
                    .extend(highlight_json(text).into_iter().map(|token| JsonToken {
                        range: token.range.start + start..token.range.end + start,
                        kind: token.kind,
                    }));
            }
        }
    }
}

/// The field blocks for a subject: one per column for a row, one per merged column for an
/// assembled message, nothing for an empty selection.
pub fn build_document(table: &Table, subject: &InspectorSubject) -> InspectorDocument {
    let mut document = InspectorDocument::default();
    match subject {
        InspectorSubject::Empty => {}
        InspectorSubject::Row { row: shown }
        | InspectorSubject::FirstOfMany {
            first_row: shown, ..
        } => {
            for (index, column) in table.columns.iter().enumerate() {
                let value = table
                    .cell(*shown, index)
                    .map_or(FieldValue::Null, field_value);
                document.push_field(&format!("{} · {}", column.name, column.type_name), &value);
            }
        }
        InspectorSubject::Assembled { fields, .. } => {
            for field in fields {
                document.push_field(
                    &format!(
                        "{} · merged {}-part message",
                        field.column_name, field.total
                    ),
                    &merged_field_value(&field.text),
                );
            }
        }
    }
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Column;
    use serde_json::json;

    #[test]
    fn drops_runtime_frames_and_simplifies_generated_names() {
        let stack = "at Contoso.Service.Handler.<HandleAsync>d__12.MoveNext() \
                     at System.Runtime.CompilerServices.AsyncTaskMethodBuilder.Start() \
                     at Contoso.Service.Program.Main() at System.Net.Http.HttpClient.SendAsync() \
                     at Polly.Retry.AsyncRetryEngine.ImplementationAsync()";
        assert_eq!(
            format_call_stack(stack),
            "at Contoso.Service.Handler.HandleAsync()\nat Contoso.Service.Program.Main()"
        );
    }

    #[test]
    fn simplifies_lambda_frames_and_treats_escaped_breaks_as_separators() {
        let stack = "t+<>c__DisplayClass21_0.<LoadIntoBufferAsync>b__0(Task copyTask) \\r\\n at System.Threading.Tasks.Task.Execute()";
        assert_eq!(format_call_stack(stack), "t.LoadIntoBufferAsync()");
    }

    #[test]
    fn never_erases_a_stack_made_only_of_runtime_frames() {
        let stack = "at System.Net.Http.HttpClient.SendAsync()";
        assert_eq!(format_call_stack(stack), stack);
    }

    #[test]
    fn puts_each_frame_on_its_own_line_with_short_source_locations() {
        let stack = r"at App.A() in D:\a\_work\1\s\Core\A.cs :line 10 at App.B() in D:\a\_work\1\s\Core\B.cs :line 20 at App.C() in D:\a\_work\1\s\Core\C.cs :line 30";
        assert_eq!(
            format_call_stack(stack),
            "at App.A() in A.cs:line 10\nat App.B() in B.cs:line 20\nat App.C() in C.cs:line 30"
        );
    }

    #[test]
    fn keeps_directories_that_look_like_escape_sequences() {
        let stack = r"at App.Run() in D:\repos\node\src\Foo.cs :line 12\nat App.Next()";
        assert_eq!(
            format_call_stack(stack),
            "at App.Run() in Foo.cs:line 12\nat App.Next()"
        );
        let unmatched = r"at App.Run() in C:\new\x.cs";
        assert_eq!(format_call_stack(unmatched), unmatched);
    }

    #[test]
    fn shortens_unc_paths_and_leaves_posix_paths() {
        assert_eq!(
            format_call_stack(r"at App.Share() in \\build01\share\src\Unc.cs :line 5"),
            "at App.Share() in Unc.cs:line 5"
        );
        let posix = "at App.Posix() in /mnt/build/src/Posix.cs:line 7";
        assert_eq!(format_call_stack(posix), posix);
    }

    #[test]
    fn real_newlines_between_frames_are_frame_separators() {
        assert_eq!(
            format_call_stack("at App.X()\nat System.Runtime.Foo()\nat App.Y()"),
            "at App.X()\nat App.Y()"
        );
    }

    #[test]
    fn formats_call_stacks_at_any_depth_by_key_name_only() {
        let value = json!({
            "CallStack": "at Upper.Case() at System.Net.Http.X()",
            "callStackHash": "at Keep.Me() at System.IO.Y()",
            "inner": [{"callstack": "at Lower.Case() at System.IO.Y()"}],
            "callStack2": 42,
        });
        let formatted = format_call_stacks_in_json(&value);
        assert_eq!(formatted["CallStack"], "at Upper.Case()");
        assert_eq!(formatted["callStackHash"], "at Keep.Me() at System.IO.Y()");
        assert_eq!(formatted["inner"][0]["callstack"], "at Lower.Case()");
        assert_eq!(formatted["callStack2"], 42);
    }

    #[test]
    fn json_is_recognised_only_when_structured() {
        assert!(structured_json(&Cell::Text(r#" {"a":1} "#.into())).is_some());
        assert!(structured_json(&Cell::Text("[1,2]".into())).is_some());
        assert!(structured_json(&Cell::Dynamic(Box::new(json!({"a": 1})))).is_some());
        for scalar in ["123", "true", "\"text\"", "{not json", "plain"] {
            assert!(
                structured_json(&Cell::Text(scalar.into())).is_none(),
                "{scalar}"
            );
        }
        assert!(structured_json(&Cell::Int(5)).is_none());
    }

    #[test]
    fn display_json_is_pretty_and_shows_escaped_newlines() {
        let value = json!({"message": "line1\nline2", "note": "back\\nslash", "callStack": "at A() at System.IO.B()"});
        assert_eq!(
            format_json_for_display(&value),
            "{\n  \"message\": \"line1\nline2\",\n  \"note\": \"back\\\\nslash\",\n  \"callStack\": \"at A()\"\n}"
        );
    }

    #[test]
    fn highlights_every_token_kind_without_changing_the_text() {
        let text = "{\n  \"name\": \"x\",\n  \"n\": -1.5e3,\n  \"ok\": true,\n  \"no\": false,\n  \"none\": null,\n  \"list\": [1, 2]\n}";
        let kinds: Vec<(&str, JsonTokenKind)> = highlight_json(text)
            .into_iter()
            .map(|token| (&text[token.range.clone()], token.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("\"name\"", JsonTokenKind::Key),
                ("\"x\"", JsonTokenKind::String),
                ("\"n\"", JsonTokenKind::Key),
                ("-1.5e3", JsonTokenKind::Number),
                ("\"ok\"", JsonTokenKind::Key),
                ("true", JsonTokenKind::Boolean),
                ("\"no\"", JsonTokenKind::Key),
                ("false", JsonTokenKind::Boolean),
                ("\"none\"", JsonTokenKind::Key),
                ("null", JsonTokenKind::Null),
                ("\"list\"", JsonTokenKind::Key),
                ("1", JsonTokenKind::Number),
                ("2", JsonTokenKind::Number),
            ]
        );
    }

    #[test]
    fn highlighting_keeps_escaped_quotes_inside_a_string() {
        let text = r#"{"a": "say \"hi\" 12"}"#;
        let tokens = highlight_json(text);
        assert_eq!(tokens.len(), 2);
        assert_eq!(&text[tokens[1].range.clone()], r#""say \"hi\" 12""#);
    }

    fn messages(values: &[&str]) -> Table {
        Table {
            name: "t".into(),
            columns: vec![Column::new("Message", "string")],
            rows: values
                .iter()
                .map(|value| vec![Cell::Text((*value).into())])
                .collect(),
        }
    }

    fn assembled(values: &[&str], selection: &[usize]) -> Option<String> {
        assemble_multipart(&messages(values), selection)
            .into_iter()
            .next()
            .map(|field| field.text)
    }

    #[test]
    fn assembles_complete_messages_in_part_order() {
        assert_eq!(
            assembled(&[r#"1/2:{"a":"#, "2/2:1}"], &[0, 1]).as_deref(),
            Some(r#"{"a":1}"#)
        );
        assert_eq!(
            assembled(&["2/2:y", "1/2:x"], &[0, 1]).as_deref(),
            Some("xy")
        );
        assert_eq!(
            assembled(&["1 / 2 : x", "2 / 2 : y"], &[0, 1]).as_deref(),
            Some("xy")
        );
        assert_eq!(
            assembled(&["1/2:  two", "2/2:end"], &[0, 1]).as_deref(),
            Some(" twoend")
        );
    }

    #[test]
    fn refuses_anything_that_is_not_one_complete_message() {
        assert_eq!(assembled(&["1/3:x", "2/3:y"], &[0, 1]), None);
        assert_eq!(assembled(&["1/2:x", "2/3:y"], &[0, 1]), None);
        assert_eq!(assembled(&["1/2:x", "1/2:y"], &[0, 1]), None);
        assert_eq!(assembled(&["1/2:x", "hello"], &[0, 1]), None);
        assert_eq!(assembled(&["1/1:x", "1/1:y"], &[0, 1]), None);
        assert_eq!(
            assembled(&["see 1/3: here", "2/3:y", "3/3:z"], &[0, 1, 2]),
            None
        );
        assert_eq!(assembled(&["1/2:x"], &[0]), None);
    }

    #[test]
    fn interleaved_sets_assemble_only_when_selected_separately() {
        let values = [
            "1/3:A1;", "1/3:B1;", "2/3:A2;", "2/3:B2;", "3/3:A3;", "3/3:B3;",
        ];
        assert_eq!(assembled(&values, &[0, 1, 2, 3, 4, 5]), None);
        assert_eq!(assembled(&values, &[0, 2, 4]).as_deref(), Some("A1;A2;A3;"));
        assert_eq!(assembled(&values, &[1, 3, 5]).as_deref(), Some("B1;B2;B3;"));
    }

    #[test]
    fn resolves_the_inspector_subject() {
        let table = messages(&["1/2:x", "2/2:y", "plain", "other"]);
        assert_eq!(resolve_subject(&table, &[]), InspectorSubject::Empty);
        assert_eq!(
            resolve_subject(&table, &[2]),
            InspectorSubject::Row { row: 2 }
        );
        assert!(matches!(
            resolve_subject(&table, &[1, 0]),
            InspectorSubject::Assembled { .. }
        ));
        assert_eq!(
            resolve_subject(&table, &[2, 3]),
            InspectorSubject::FirstOfMany {
                first_row: 2,
                selected_count: 2
            }
        );
        assert_eq!(resolve_subject(&table, &[99]), InspectorSubject::Empty);
    }

    #[test]
    fn presents_values_as_null_json_or_text() {
        assert_eq!(field_value(&Cell::Null), FieldValue::Null);
        assert_eq!(field_value(&Cell::Int(5)), FieldValue::Text("5".into()));
        assert!(matches!(
            field_value(&Cell::Text("{\"a\":1}".into())),
            FieldValue::Json(_)
        ));
        assert_eq!(
            merged_field_value("hello"),
            FieldValue::Text("hello".into())
        );
        assert!(matches!(merged_field_value("[1]"), FieldValue::Json(_)));
    }

    #[test]
    fn a_row_becomes_one_block_per_column_with_styled_spans() {
        let table = Table {
            name: "PrimaryResult".into(),
            columns: vec![
                Column::new("Message", "string"),
                Column::new("Exception", "dynamic"),
                Column::new("Trace", "string"),
            ],
            rows: vec![vec![
                Cell::Text("line one\nline two".into()),
                Cell::Dynamic(Box::new(json!({"type": "Oops"}))),
                Cell::Null,
            ]],
        };
        let document = build_document(&table, &InspectorSubject::Row { row: 0 });
        assert_eq!(
            document.text,
            "Message · string\nline one\nline two\n\nException · dynamic\n{\n  \"type\": \"Oops\"\n}\n\nTrace · string\nnull"
        );
        let spans = |ranges: &[Range<usize>]| -> Vec<&str> {
            ranges
                .iter()
                .map(|range| &document.text[range.clone()])
                .collect()
        };
        assert_eq!(
            spans(&document.headers),
            ["Message · string", "Exception · dynamic", "Trace · string"]
        );
        assert_eq!(spans(&document.nulls), ["null"]);
        assert_eq!(spans(&document.code_blocks), ["{\n  \"type\": \"Oops\"\n}"]);
        let tokens: Vec<(&str, JsonTokenKind)> = document
            .json_tokens
            .iter()
            .map(|token| (&document.text[token.range.clone()], token.kind))
            .collect();
        assert_eq!(
            tokens,
            [
                ("\"type\"", JsonTokenKind::Key),
                ("\"Oops\"", JsonTokenKind::String)
            ]
        );
    }

    #[test]
    fn an_assembled_message_shows_only_the_merged_fields() {
        let table = Table {
            name: "PrimaryResult".into(),
            columns: vec![
                Column::new("Message", "string"),
                Column::new("Level", "long"),
            ],
            rows: vec![
                vec![Cell::Text("1/2:{\"a\":".into()), Cell::Int(4)],
                vec![Cell::Text("2/2:1}".into()), Cell::Int(4)],
            ],
        };
        let subject = resolve_subject(&table, &[1, 0]);
        let document = build_document(&table, &subject);
        assert_eq!(
            document.text,
            "Message · merged 2-part message\n{\n  \"a\": 1\n}"
        );
        assert_eq!(
            build_document(&table, &InspectorSubject::Empty),
            InspectorDocument::default()
        );
    }
}
