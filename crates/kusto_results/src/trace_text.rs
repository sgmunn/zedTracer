//! The text of a trace as people read it: the gist of a message, a marker without its namespace,
//! and text cut to fit. Every view that shows messages uses these, so they agree.

/// A row whose text says nothing about what went wrong: a later part of a split message, or a
/// notice that a message was split.
pub fn is_filler(text: &str) -> bool {
    let text = text.trim_start();
    if text.starts_with("The message is splitted")
        || text.starts_with("Message size is too large")
        || text.starts_with("Monitored scope")
    {
        return true;
    }
    split_part_prefix(text).is_some_and(|(part, _)| part != 1)
}

/// `(part, rest)` for text that starts `k/N: `.
pub fn split_part_prefix(text: &str) -> Option<(usize, &str)> {
    let (part, rest) = text.split_once('/')?;
    let (total, rest) = rest.split_once(':')?;
    if total.is_empty() || !total.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let part: usize = part.parse().ok()?;
    Some((part, rest.trim_start()))
}

/// What a message says in a few lines: the `message` of a JSON error when there is one, else the text itself,
/// wrapped at word boundaries to `width` characters and cut after `lines` lines. The lines are
/// separated by `\n`.
pub fn summarize_message(message: &str, width: usize, lines: usize) -> String {
    let text = match split_part_prefix(message.trim_start()) {
        Some((_, rest)) => rest,
        None => message.trim_start(),
    };
    let text = json_message(text).unwrap_or_else(|| text.to_string());
    wrap(&text, width, lines)
}

fn wrap(text: &str, width: usize, max_lines: usize) -> String {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cut = false;
    'words: for word in text.split_whitespace() {
        let mut word = word;
        loop {
            let current = lines.last().map_or(0, |line| line.chars().count());
            let word_length = word.chars().count();
            if !lines.is_empty() && current + 1 + word_length <= width {
                if let Some(line) = lines.last_mut() {
                    line.push(' ');
                    line.push_str(word);
                }
                continue 'words;
            }
            if lines.len() == max_lines {
                cut = true;
                break 'words;
            }
            if word_length <= width {
                lines.push(word.to_string());
                continue 'words;
            }
            let split = word
                .char_indices()
                .nth(width)
                .map_or(word.len(), |(index, _)| index);
            lines.push(word[..split].to_string());
            word = &word[split..];
        }
    }
    if cut {
        if let Some(line) = lines.last_mut() {
            *line = format!("{}…", line.trim_end());
        }
    }
    lines.join("\n")
}

/// Cuts text to `limit` characters, ending in an ellipsis when it was cut.
pub fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// The value of the first `"message"` key, read without parsing the document, because a split
/// message's first part is JSON that stops in the middle.
fn json_message(text: &str) -> Option<String> {
    let after_key = &text[text.find("\"message\"")? + "\"message\"".len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let body = after_colon.strip_prefix('"')?;
    let mut raw = String::new();
    let mut escaped = false;
    for character in body.chars() {
        match (escaped, character) {
            (true, other) => {
                raw.push('\\');
                raw.push(other);
                escaped = false;
            }
            (false, '\\') => escaped = true,
            (false, '"') => {
                return serde_json::from_str::<String>(&format!("\"{raw}\"")).ok();
            }
            (false, other) => raw.push(other),
        }
    }
    None
}

/// The last two dotted segments of a marker, so a namespace does not fill the label.
pub fn short_marker(marker: &str) -> String {
    let segments: Vec<&str> = marker.split('.').collect();
    match segments.len() {
        0..=2 => marker.to_string(),
        count => segments[count - 2..].join("."),
    }
}

/// Short names for the actors: generic trailing segments dropped, then the fewest trailing
/// segments that tell every actor apart.
pub fn display_names(names: &[String], generic_suffixes: &[String]) -> Vec<String> {
    let trimmed: Vec<Vec<&str>> = names
        .iter()
        .map(|name| {
            let mut segments: Vec<&str> = name.split('.').collect();
            while segments.len() > 1
                && segments.last().is_some_and(|last| {
                    generic_suffixes
                        .iter()
                        .any(|suffix| suffix.eq_ignore_ascii_case(last))
                })
            {
                segments.pop();
            }
            segments
        })
        .collect();
    let trailing = |segments: &[&str], count: usize| -> String {
        segments[segments.len().saturating_sub(count)..].join(".")
    };
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let own = &trimmed[index];
            (1..=own.len())
                .find(|count| {
                    let candidate = trailing(own, *count);
                    trimmed.iter().enumerate().all(|(other, segments)| {
                        other == index || trailing(segments, *count) != candidate
                    })
                })
                .map(|count| trailing(own, count))
                .unwrap_or_else(|| name.clone())
        })
        .collect()
}

const TICKS_PER_MILLISECOND: i64 = 10_000;

/// A duration for a narrow column: `4.2 s` from a second up, `76 ms`, or `<1 ms`.
pub fn short_duration(ticks: i64) -> String {
    let milliseconds = ticks as f64 / TICKS_PER_MILLISECOND as f64;
    if milliseconds >= 1000.0 {
        format!("{:.1} s", milliseconds / 1000.0)
    } else if milliseconds >= 1.0 {
        format!("{} ms", milliseconds.round() as i64)
    } else {
        "<1 ms".to_string()
    }
}

/// A time from the start of a trace, as `+0.045 s`.
pub fn offset_from_start(ticks: i64) -> String {
    format!("+{:.3} s", ticks.max(0) as f64 / (TICKS_PER_MILLISECOND * 1000) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_wraps_at_words_and_is_cut_after_the_last_line() {
        assert_eq!(wrap("one two three four", 9, 3), "one two\nthree\nfour");
        assert_eq!(wrap("one two three four five six", 9, 2), "one two\nthree…");
        assert_eq!(wrap("abcdefghij", 4, 3), "abcd\nefgh\nij");
        assert_eq!(wrap("   ", 10, 3), "");
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("a very long label", 8), "a very…");
    }

    #[test]
    fn a_message_is_read_as_json_when_it_is_and_as_text_when_it_is_not() {
        let json = r#"1/3: {"code":"InternalError","message":"Row not found!","timeStamp":"x""#;
        assert_eq!(summarize_message(json, 60, 3), "Row not found!");
        assert_eq!(summarize_message("plain  text\nhere", 60, 3), "plain text here");
        assert_eq!(summarize_message(r#"{"message":"a \"b\" c"}"#, 60, 3), "a \"b\" c");
    }

    #[test]
    fn rows_that_say_nothing_are_filler() {
        assert!(is_filler("2/3: at Some.Frame()"));
        assert!(is_filler("The message is splitted into 3 parts."));
        assert!(is_filler("Message size is too large: 22392 bytes."));
        assert!(is_filler("  Monitored scope end."));
        assert!(!is_filler("1/3: {\"message\":\"x\""));
        assert!(!is_filler("Disk is full"));
    }

    #[test]
    fn a_marker_keeps_its_last_two_dotted_parts() {
        assert_eq!(short_marker("Microsoft.Dms.Client.GetToken"), "Client.GetToken");
        assert_eq!(short_marker("WebApi-IncomingRequest"), "WebApi-IncomingRequest");
        assert_eq!(short_marker("A.B"), "A.B");
    }

    #[test]
    fn actor_names_are_shortened_until_they_differ() {
        let names: Vec<String> = [
            "Microsoft.Dms.Service.EntryPoint",
            "Microsoft.MWC.Workload.OneLake.Service.EntryPoint",
            "Microsoft.ASPaaS.FrontEnd.Service",
            "(unknown)",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let suffixes = vec!["EntryPoint".to_string(), "Service".to_string()];
        assert_eq!(
            display_names(&names, &suffixes),
            vec!["Dms", "OneLake", "FrontEnd", "(unknown)"]
        );
        let clashing: Vec<String> = ["A.Core.Service", "B.Core.Service"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(display_names(&clashing, &suffixes), vec!["A.Core", "B.Core"]);
    }

    #[test]
    fn durations_and_offsets_fit_a_narrow_column() {
        assert_eq!(short_duration(4_202 * TICKS_PER_MILLISECOND), "4.2 s");
        assert_eq!(short_duration(76 * TICKS_PER_MILLISECOND), "76 ms");
        assert_eq!(short_duration(5_000), "<1 ms");
        assert_eq!(short_duration(0), "<1 ms");
        assert_eq!(offset_from_start(45 * TICKS_PER_MILLISECOND), "+0.045 s");
        assert_eq!(offset_from_start(4_539 * TICKS_PER_MILLISECOND), "+4.539 s");
        assert_eq!(offset_from_start(-5), "+0.000 s");
    }
}
