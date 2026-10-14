use std::ops::Range;

use crate::functions::is_functions_marker;

/// A control command as the service wants it: from its first line that is not blank or a comment,
/// because the service rejects a command with a comment before the dot. The text of the query, which
/// people label with such comments, is kept as it is everywhere else.
pub fn command_text(query: &str) -> &str {
    let mut start = 0;
    for line in query.split_inclusive('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            return &query[start..];
        }
        start += line.len();
    }
    query
}

/// Whether a query is a control command such as `.show tables`: its first line that is not blank
/// or a comment starts with a dot. Commands go to a different endpoint than queries.
pub fn is_control_command(query: &str) -> bool {
    query
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .is_some_and(|line| line.starts_with('.'))
}

/// A run of non-blank lines, or more where a bracket is still open at a blank line.
struct Block {
    range: Range<usize>,
    /// Whether anything in it is more than comments. A block of only comments, such as a
    /// directive or a note, is not a query, and neither is a block of function definitions.
    is_query: bool,
    /// Whether it has the `// :functions` line.
    is_functions: bool,
}

struct Line<'a> {
    start: usize,
    text: &'a str,
}

impl Line<'_> {
    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    fn end(&self) -> usize {
        self.start + self.text.trim_end_matches(['\r', '\n']).len()
    }
}

/// Follows the brackets of the lines it is given, to tell whether a blank line comes inside
/// parentheses or braces (a function body with a gap in it) or between two queries. Brackets in
/// comments and strings do not count. A string of one quote ends with its line, so a quote left
/// open while typing does not hold the rest of the file.
#[derive(Default)]
struct Brackets {
    depth: usize,
    /// The delimiter that ends the multi-line string the lines are in.
    multiline_string: Option<[u8; 3]>,
}

impl Brackets {
    fn is_open(&self) -> bool {
        self.depth > 0 || self.multiline_string.is_some()
    }

    fn follow(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if let Some(delimiter) = self.multiline_string {
                match bytes[index..]
                    .windows(3)
                    .position(|window| window == delimiter)
                {
                    Some(offset) => {
                        index += offset + delimiter.len();
                        self.multiline_string = None;
                        continue;
                    }
                    None => return,
                }
            }
            match bytes[index] {
                b'/' if bytes.get(index + 1) == Some(&b'/') => return,
                byte @ (b'`' | b'~') if bytes[index..].starts_with(&[byte; 3]) => {
                    self.multiline_string = Some([byte; 3]);
                    index += 3;
                }
                quote @ (b'\'' | b'"') => {
                    index = end_of_string(bytes, index, quote);
                }
                b'(' | b'{' => {
                    self.depth += 1;
                    index += 1;
                }
                b')' | b'}' => {
                    self.depth = self.depth.saturating_sub(1);
                    index += 1;
                }
                _ => index += 1,
            }
        }
    }
}

/// Where the string of one quote that starts at `start` ends, or the end of the line. A verbatim
/// string (`@'…'`) has no escapes and doubles its quote; any other takes a backslash as an escape.
fn end_of_string(bytes: &[u8], start: usize, quote: u8) -> usize {
    let verbatim = start > 0 && bytes[start - 1] == b'@';
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte == quote => {
                if verbatim && bytes.get(index + 1) == Some(&quote) {
                    index += 2;
                } else {
                    return index + 1;
                }
            }
            b'\\' if !verbatim => index += 2,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// The last line of the block that starts at `first`. A blank line ends a block unless a bracket
/// the block opened is still open, which is a gap inside a function body or a call. Brackets
/// that are never closed are no gap: they would join every query below to this one, so a block
/// whose brackets are open at the end of the text ends at its first blank line instead.
fn last_line_of_block(lines: &[Line], first: usize) -> usize {
    let mut brackets = Brackets::default();
    let mut last = first;
    for (index, line) in lines.iter().enumerate().skip(first) {
        if !line.is_blank() {
            brackets.follow(line.text);
            last = index;
        } else if !brackets.is_open() {
            return last;
        }
    }
    if brackets.is_open() {
        lines[first..]
            .iter()
            .position(Line::is_blank)
            .map_or(lines.len() - 1, |blank| first + blank - 1)
    } else {
        last
    }
}

fn blocks(text: &str) -> Vec<Block> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        lines.push(Line { start, text: line });
        start += line.len();
    }

    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].is_blank() {
            index += 1;
            continue;
        }
        let last = last_line_of_block(&lines, index);
        let in_block = || lines[index..=last].iter().filter(|line| !line.is_blank());
        let has_code = in_block().any(|line| !line.text.trim().starts_with("//"));
        let is_functions = in_block().any(|line| is_functions_marker(line.text));
        blocks.push(Block {
            range: lines[index].start..lines[last].end(),
            is_query: has_code && !is_functions,
            is_functions,
        });
        index = last + 1;
    }
    blocks
}

/// Every block of function definitions of a text, in order.
pub(crate) fn function_blocks(text: &str) -> Vec<Range<usize>> {
    blocks(text)
        .into_iter()
        .filter(|block| block.is_functions)
        .map(|block| block.range)
        .collect()
}

/// Every query of a text, in order.
pub fn query_blocks(text: &str) -> Vec<Range<usize>> {
    blocks(text)
        .into_iter()
        .filter(|block| block.is_query)
        .map(|block| block.range)
        .collect()
}

/// The query a cursor is in: the run of non-blank lines around `offset`.
///
/// A cursor on a blank line, or in a block of only comments, belongs to the query above it, or
/// to the one below when nothing is above. A text with no query has none.
pub fn query_range_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let queries = query_blocks(text);
    queries
        .iter()
        .find(|range| range.start <= offset && offset <= range.end)
        .or_else(|| queries.iter().rev().find(|range| range.end < offset))
        .or_else(|| queries.iter().find(|range| range.start > offset))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_at<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
        let offset = text.find(marker)?;
        query_range_at(text, offset).map(|range| &text[range])
    }

    #[test]
    fn selects_the_lines_around_the_cursor() {
        let text = "T1\n| take 1\n\nT2\n| take 2\n| count\n\nT3";
        assert_eq!(query_at(text, "take 1"), Some("T1\n| take 1"));
        assert_eq!(query_at(text, "count"), Some("T2\n| take 2\n| count"));
        assert_eq!(query_at(text, "T3"), Some("T3"));
    }

    #[test]
    fn a_blank_line_belongs_to_the_query_above() {
        let text = "T1\n\n\nT2";
        assert_eq!(
            query_range_at(text, 3).map(|range| &text[range]),
            Some("T1")
        );
    }

    #[test]
    fn a_blank_line_before_any_query_belongs_to_the_one_below() {
        let text = "\n\nT1\n| take 1";
        assert_eq!(
            query_range_at(text, 0).map(|range| &text[range]),
            Some("T1\n| take 1")
        );
    }

    #[test]
    fn a_cursor_at_the_end_of_the_text_is_in_the_last_query() {
        let text = "T1\n\nT2\n";
        assert_eq!(
            query_range_at(text, text.len()).map(|range| &text[range]),
            Some("T2")
        );
    }

    #[test]
    fn a_line_with_only_spaces_is_blank() {
        let text = "T1\n   \nT2";
        assert_eq!(query_at(text, "T2"), Some("T2"));
    }

    #[test]
    fn carriage_returns_do_not_end_up_in_the_query() {
        let text = "T1\r\n| take 1\r\n\r\nT2";
        assert_eq!(query_at(text, "take"), Some("T1\r\n| take 1"));
    }

    #[test]
    fn a_blank_line_inside_brackets_does_not_end_the_query() {
        let text = "print (1\n\n+ 2)\n\nprint 3";
        assert_eq!(query_at(text, "+ 2"), Some("print (1\n\n+ 2)"));
        assert_eq!(query_at(text, "print 3"), Some("print 3"));
        assert_eq!(
            query_at(text, "\n\n+"),
            Some("print (1\n\n+ 2)"),
            "a cursor on the gap is in the query that holds it"
        );
    }

    #[test]
    fn a_cursor_on_a_blank_line_after_a_closed_bracket_belongs_to_the_query_above() {
        let text = "f(1)\n\n\ng(2)";
        assert_eq!(
            query_range_at(text, 6).map(|range| &text[range]),
            Some("f(1)")
        );
    }

    #[derive(serde::Deserialize)]
    struct BlockCase {
        name: String,
        text: String,
        queries: Vec<[usize; 2]>,
    }

    /// The same cases the language server is tested with, through the line each lens is above.
    #[test]
    fn the_shared_block_cases_find_the_queries_the_server_finds() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fork-docs/samples/query-blocks.json"
        );
        let cases: Vec<BlockCase> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("the cases file reads"))
                .expect("the cases file parses");
        assert!(!cases.is_empty());
        for case in cases {
            let line_of = |offset: usize| case.text[..offset].matches('\n').count();
            let found: Vec<[usize; 2]> = query_blocks(&case.text)
                .into_iter()
                .map(|range| [line_of(range.start), line_of(range.end)])
                .collect();
            assert_eq!(found, case.queries, "{}", case.name);
        }
    }

    #[test]
    fn a_block_of_only_comments_is_not_a_query() {
        let text = "// notes\n\n//:setDefaultDb(\"x\")\n\nT1\n\n// more notes";
        assert_eq!(query_blocks(text).len(), 1);
        assert_eq!(
            query_at(text, "notes"),
            Some("T1"),
            "a note belongs to the query below"
        );
        assert_eq!(
            query_at(text, "more notes"),
            Some("T1"),
            "a trailing note, to the query above"
        );
        assert_eq!(query_at(text, "T1"), Some("T1"));
    }

    #[test]
    fn a_block_of_function_definitions_is_not_a_query() {
        let text = "// :functions\nlet A = () { 1 };\n\nprint A()\n\nlet B = () { 2 };\n// :functions";
        assert_eq!(
            query_blocks(text).into_iter().map(|range| &text[range]).collect::<Vec<_>>(),
            ["print A()"],
            "the marker may come anywhere in its block"
        );
        assert_eq!(
            function_blocks(text).into_iter().map(|range| &text[range]).collect::<Vec<_>>(),
            ["// :functions\nlet A = () { 1 };", "let B = () { 2 };\n// :functions"]
        );
        assert_eq!(
            query_at(text, "let A"),
            Some("print A()"),
            "a cursor in the definitions belongs to the query below"
        );
    }

    #[test]
    fn comments_inside_a_query_belong_to_it() {
        let text = "// why\nT1\n// then\n| take 1";
        assert_eq!(query_at(text, "then"), Some(text));
    }

    #[test]
    fn text_without_a_query_has_none() {
        assert_eq!(query_range_at("", 0), None);
        assert_eq!(query_range_at("\n  \n", 1), None);
        assert_eq!(query_range_at("// only a note", 3), None);
    }

    #[test]
    fn a_command_is_sent_from_its_first_line_of_code() {
        assert_eq!(command_text(".show tables"), ".show tables");
        assert_eq!(
            command_text("// the tables\n\n  // more\n.show tables\n// after"),
            ".show tables\n// after"
        );
        assert_eq!(command_text("// only a comment"), "// only a comment");
        assert_eq!(command_text(""), "");
    }

    #[test]
    fn a_query_is_a_control_command_when_its_first_real_line_starts_with_a_dot() {
        assert!(is_control_command(".show tables"));
        assert!(is_control_command("  .show function ASAz  \n| take 1"));
        assert!(is_control_command(
            "// the functions\n\n// of the database\n.show functions"
        ));
        assert!(!is_control_command("T | where Value == 1.5"));
        assert!(!is_control_command("// .show tables\nT"));
        assert!(!is_control_command("let a = 1;\n.show tables"));
        assert!(!is_control_command(""));
        assert!(!is_control_command("// only a comment"));
    }
}
