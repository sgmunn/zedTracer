use std::ops::Range;

/// A run of non-blank lines.
struct Block {
    range: Range<usize>,
    /// Whether anything in it is more than comments. A block of only comments, such as a
    /// directive or a note, is not a query.
    is_query: bool,
}

fn blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Option<(usize, bool)> = None;
    let mut line_start = 0;
    let mut previous_end = 0;
    for line in text.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some((start, is_query)) = current.take() {
                blocks.push(Block {
                    range: start..previous_end,
                    is_query,
                });
            }
        } else {
            let content = !trimmed.starts_with("//");
            current = Some(match current {
                Some((start, is_query)) => (start, is_query || content),
                None => (line_start, content),
            });
            previous_end = line_start + line.trim_end_matches(['\r', '\n']).len();
        }
        line_start = line_end;
    }
    if let Some((start, is_query)) = current {
        blocks.push(Block {
            range: start..previous_end,
            is_query,
        });
    }
    blocks
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
}
