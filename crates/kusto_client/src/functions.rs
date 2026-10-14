//! Functions and values defined in the query file itself, and added to the queries that use them.
//!
//! A block (a run of non-blank lines) with the line `// :functions` in it holds definitions: each
//! `let name = …;` statement in it, whether it defines a function with `(parameters) { body }`, a
//! scalar such as `5` or `ago(7d)`, or a table, is one every query below it may use (see
//! [crate::directives] for how long they last). The block is not a query. When a query is run or
//! copied it gets a `let` for each definition it uses, and for those they use in turn, so the text
//! that reaches the service is self-contained. Nothing else of a functions block is used.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::query_text::is_control_command;

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*//\s*:\s*functions\s*$").expect("the marker pattern is valid")
});

/// Whether a line marks its block as one of function definitions.
pub(crate) fn is_functions_marker(line: &str) -> bool {
    MARKER.is_match(line)
}

/// A function or value defined in the file: its name, and the whole `let` statement, ending in `;`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionDefinition {
    pub name: String,
    pub text: String,
}

/// A run of text that is code, or something that is not read as code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    Comment,
    Text,
}

/// Each character of a text with what it is part of: code, a comment, or a string literal (the
/// quotes included). A backslash in a string escapes the next character.
fn classify(text: &str) -> Vec<(usize, char, Kind)> {
    let mut classified = Vec::with_capacity(text.len());
    let mut characters = text.char_indices().peekable();
    let mut quote: Option<char> = None;
    while let Some((offset, character)) = characters.next() {
        if let Some(open) = quote {
            classified.push((offset, character, Kind::Text));
            if character == '\\' {
                if let Some((offset, escaped)) = characters.next() {
                    classified.push((offset, escaped, Kind::Text));
                }
            } else if character == open {
                quote = None;
            }
        } else if character == '/' && characters.peek().is_some_and(|(_, next)| *next == '/') {
            classified.push((offset, character, Kind::Comment));
            while let Some(&(offset, next)) = characters.peek() {
                if next == '\n' {
                    break;
                }
                classified.push((offset, next, Kind::Comment));
                characters.next();
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
            classified.push((offset, character, Kind::Text));
        } else {
            classified.push((offset, character, Kind::Code));
        }
    }
    classified
}

/// The identifiers in code (not in comments or strings), with where each starts and ends. A word
/// that starts with a digit, such as `5m` or `1e5`, is not one.
fn identifiers(classified: &[(usize, char, Kind)], text: &str) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < classified.len() {
        let (start, character, kind) = classified[index];
        if kind != Kind::Code || !(character.is_alphanumeric() || character == '_') {
            index += 1;
            continue;
        }
        let mut end_index = index;
        while end_index < classified.len()
            && classified[end_index].2 == Kind::Code
            && (classified[end_index].1.is_alphanumeric() || classified[end_index].1 == '_')
        {
            end_index += 1;
        }
        let end = classified
            .get(end_index)
            .map_or(text.len(), |(offset, _, _)| *offset);
        if !character.is_ascii_digit() {
            found.push((text[start..end].to_string(), start, end));
        }
        index = end_index;
    }
    found
}

fn next_code_character(classified: &[(usize, char, Kind)], from: usize) -> Option<char> {
    classified
        .iter()
        .skip_while(|(offset, _, _)| *offset < from)
        .find(|(_, character, kind)| *kind == Kind::Code && !character.is_whitespace())
        .map(|(_, character, _)| *character)
}

/// The names a text defines for itself with `let name =`.
fn names_defined_by(text: &str) -> HashSet<String> {
    let classified = classify(text);
    identifiers(&classified, text)
        .windows(2)
        .filter_map(|pair| {
            let [(keyword, _, _), (name, _, name_end)] = pair else {
                return None;
            };
            (keyword == "let" && next_code_character(&classified, *name_end) == Some('='))
                .then(|| name.clone())
        })
        .collect()
}

/// The names that appear in a text as words, outside comments and strings. A name that is also
/// a column or a property counts too, which only costs an unused `let`.
fn names_used_by(text: &str) -> HashSet<String> {
    let classified = classify(text);
    identifiers(&classified, text)
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// The statements of a text, each from its first character of code to its `;` (or to the end of
/// the text), with the offset it starts at. Brackets and strings do not end a statement.
fn statements(text: &str) -> Vec<(usize, String)> {
    let classified = classify(text);
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    for &(offset, character, kind) in &classified {
        if kind == Kind::Code && start.is_none() && !character.is_whitespace() {
            start = Some(offset);
        }
        if kind != Kind::Code {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => {
                if let Some(begin) = start.take() {
                    found.push((begin, text[begin..=offset].to_string()));
                }
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        found.push((begin, text[begin..].trim_end().to_string()));
    }
    found
}

/// The name a statement defines, if it is `let name = …`.
fn definition_name(statement: &str) -> Option<String> {
    let classified = classify(statement);
    let words = identifiers(&classified, statement);
    let [(keyword, _, _), (name, _, name_end), ..] = words.as_slice() else {
        return None;
    };
    (keyword == "let" && next_code_character(&classified, *name_end) == Some('='))
        .then(|| name.clone())
}

/// What a functions block defines, in the order written: every `let` statement. Any other
/// statement of the block is left out, and one that lacks its `;` gets it.
pub fn definitions_in(block: &str) -> Vec<FunctionDefinition> {
    statements(block)
        .into_iter()
        .filter_map(|(_, statement)| {
            let name = definition_name(&statement)?;
            let text = if statement.ends_with(';') {
                statement
            } else {
                format!("{statement};")
            };
            Some(FunctionDefinition { name, text })
        })
        .collect()
}

/// The definitions after `definition` is made: it replaces one of the same name where that was,
/// so the definitions after it, which may use it, keep coming after it; otherwise it goes last.
pub fn define(functions: &mut Vec<FunctionDefinition>, definition: FunctionDefinition) {
    match functions
        .iter_mut()
        .find(|existing| existing.name == definition.name)
    {
        Some(existing) => *existing = definition,
        None => functions.push(definition),
    }
}

/// The statements of the functions a query uses, and those they use in turn, in the order they
/// were defined. A function the query defines for itself with `let` is the query's own, and a
/// control command uses none.
pub fn functions_used_by(query: &str, functions: &[FunctionDefinition]) -> Vec<String> {
    if functions.is_empty() || is_control_command(query) {
        return Vec::new();
    }
    let own = names_defined_by(query);
    let mut needed: HashSet<&str> = HashSet::new();
    let mut pending = vec![names_used_by(query)];
    while let Some(names) = pending.pop() {
        for function in functions {
            if names.contains(&function.name)
                && !own.contains(&function.name)
                && needed.insert(function.name.as_str())
            {
                pending.push(names_used_by(&function.text));
            }
        }
    }
    functions
        .iter()
        .filter(|function| needed.contains(function.name.as_str()))
        .map(|function| function.text.clone())
        .collect()
}

/// The query with the statements of `functions` added after its leading `declare` and `set`
/// statements, which they may depend on, and before the rest. Leading comments stay on top. A
/// control command is left as it is.
pub fn with_functions(query: &str, functions: &[String]) -> String {
    if functions.is_empty() || is_control_command(query) {
        return query.to_string();
    }
    let classified = classify(query);
    let words = identifiers(&classified, query);
    let mut position = 0;
    let mut after_prologue: Option<usize> = None;
    while let Some(start) = next_code_offset(&classified, position) {
        let is_prologue = words
            .iter()
            .find(|(_, word_start, _)| *word_start == start)
            .is_some_and(|(word, _, _)| matches!(word.to_ascii_lowercase().as_str(), "declare" | "set"));
        let Some(end) = is_prologue
            .then(|| end_of_statement(&classified, start))
            .flatten()
        else {
            break;
        };
        position = end;
        after_prologue = Some(end);
    }
    let added = functions.join("\n");
    match after_prologue {
        None => {
            let first = next_code_offset(&classified, 0).unwrap_or(0);
            let line_start = query
                .get(..first)
                .and_then(|before| before.rfind('\n'))
                .map_or(0, |newline| newline + 1);
            format!("{}{added}\n{}", &query[..line_start], &query[line_start..])
        }
        Some(end) => {
            let rest = query.get(end..).unwrap_or_default();
            let rest_of_line = rest.split('\n').next().unwrap_or_default();
            if !rest_of_line.trim().is_empty() {
                format!("{}\n{added}\n{rest}", &query[..end])
            } else if let Some(newline) = rest.find('\n') {
                let next_line = end + newline + 1;
                format!("{}{added}\n{}", &query[..next_line], &query[next_line..])
            } else {
                format!("{query}\n{added}")
            }
        }
    }
}

/// Where the first character of code at or after `from` is.
fn next_code_offset(classified: &[(usize, char, Kind)], from: usize) -> Option<usize> {
    classified
        .iter()
        .find(|(offset, character, kind)| {
            *offset >= from && *kind == Kind::Code && !character.is_whitespace()
        })
        .map(|(offset, _, _)| *offset)
}

/// Where the statement that starts at `from` ends: just after its `;` outside brackets.
fn end_of_statement(classified: &[(usize, char, Kind)], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    for &(offset, character, kind) in classified {
        if offset < from || kind != Kind::Code {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return Some(offset + 1),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(name: &str, text: &str) -> FunctionDefinition {
        FunctionDefinition {
            name: name.into(),
            text: text.into(),
        }
    }

    fn names(definitions: &[FunctionDefinition]) -> Vec<&str> {
        definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect()
    }

    #[test]
    fn the_marker_is_a_directive_line_on_its_own() {
        assert!(is_functions_marker("// :functions"));
        assert!(is_functions_marker("//:functions  "));
        assert!(is_functions_marker("  //  :  functions\r"));
        assert!(!is_functions_marker("// :functions()"));
        assert!(!is_functions_marker("// functions"));
        assert!(!is_functions_marker("// :functions are here"));
        assert!(!is_functions_marker("T | take 1 // :functions"));
    }

    #[test]
    fn a_block_gives_its_definitions_whole_and_in_order() {
        let block = "// :functions\nlet Double = (x:long) { x * 2 };\n// a note\nlet Recent = (count:long, since:datetime = datetime(2024-01-01)) {\n    Events\n    | where At > since // not a ; end\n    | top count by At desc\n};\nlet Limit = 5;\nlet Window = ago(7d);\nlet ViewOfIt = view () { Events };\nlet Few = Events | take 3;";
        let found = definitions_in(block);
        assert_eq!(
            names(&found),
            ["Double", "Recent", "Limit", "Window", "ViewOfIt", "Few"]
        );
        assert_eq!(found[0].text, "let Double = (x:long) { x * 2 };");
        assert!(
            found[1].text.starts_with("let Recent = (count:long")
                && found[1].text.ends_with("top count by At desc\n};"),
            "{:?}",
            found[1].text
        );
        assert_eq!(found[2].text, "let Limit = 5;");
        assert_eq!(found[3].text, "let Window = ago(7d);");
        assert_eq!(found[4].text, "let ViewOfIt = view () { Events };");
        assert_eq!(found[5].text, "let Few = Events | take 3;");
    }

    #[test]
    fn a_semicolon_in_a_string_or_a_body_does_not_end_a_statement() {
        let found = definitions_in(
            "let A = (x:string) { strcat(x, \";\", 'it\\'s;') };\nlet B = () { let y = 1; y };\nlet C = 'a;b';",
        );
        assert_eq!(names(&found), ["A", "B", "C"]);
        assert_eq!(found[2].text, "let C = 'a;b';");
        assert_eq!(found[1].text, "let B = () { let y = 1; y };");
    }

    #[test]
    fn a_definition_without_its_semicolon_gets_one() {
        let found = definitions_in("// :functions\nlet A = () { 1 }");
        assert_eq!(found, [definition("A", "let A = () { 1 };")]);
    }

    #[test]
    fn only_a_let_statement_is_a_definition() {
        assert!(definitions_in("print 1;\nT | take 1;\nset notruncation").is_empty());
        assert!(definitions_in("// let A = () { 1 };").is_empty());
        assert!(definitions_in("let;\nlet a;\nlet 5 = 1;").is_empty());
        assert!(definitions_in("").is_empty());
    }

    #[test]
    fn defining_a_name_again_replaces_it_where_it_was() {
        let mut functions = vec![definition("A", "one"), definition("B", "two")];
        define(&mut functions, definition("A", "three"));
        define(&mut functions, definition("C", "four"));
        assert_eq!(
            functions,
            [definition("A", "three"), definition("B", "two"), definition("C", "four")]
        );
    }

    fn defined() -> Vec<FunctionDefinition> {
        definitions_in(
            "let Double = (x:long) { x * 2 };\nlet Quad = (x:long) { Double(Double(x)) };\nlet Unused = () { Nope };\nlet Other = () { 'Unused in a string' };",
        )
    }

    #[test]
    fn only_the_functions_a_query_uses_are_added_with_those_they_use() {
        assert_eq!(
            functions_used_by("print Quad(2)", &defined()),
            [
                "let Double = (x:long) { x * 2 };",
                "let Quad = (x:long) { Double(Double(x)) };"
            ],
            "Double comes along with Quad, and in the order they were defined"
        );
        assert_eq!(
            functions_used_by("print Double(2) // Unused", &defined()),
            ["let Double = (x:long) { x * 2 };"],
            "a comment does not use a function"
        );
        assert!(functions_used_by("print 'Quad'", &defined()).is_empty());
        let with_value = definitions_in("let Limit = 5;\nlet Few = (x:long) { Events | take Limit };");
        assert_eq!(
            functions_used_by("Few(1)", &with_value),
            ["let Limit = 5;", "let Few = (x:long) { Events | take Limit };"],
            "a value that a function uses comes along with it"
        );
        assert_eq!(
            functions_used_by("print Limit", &with_value),
            ["let Limit = 5;"]
        );
        assert!(functions_used_by("T | take 1", &defined()).is_empty());
    }

    #[test]
    fn a_function_the_query_defines_for_itself_is_the_querys_own() {
        assert_eq!(
            functions_used_by(
                "let Double = (x:long) { x * 3 };\nprint Double(2), Quad(1)",
                &defined()
            ),
            ["let Quad = (x:long) { Double(Double(x)) };"],
            "Quad is added, and Double is not, because the query has its own"
        );
    }

    #[test]
    fn a_control_command_uses_no_function() {
        assert!(functions_used_by(".show function Double", &defined()).is_empty());
        assert_eq!(
            with_functions(".show function Double", &["let A = 1;".to_string()]),
            ".show function Double"
        );
    }

    fn added() -> Vec<String> {
        vec!["let A = () { 1 };".into(), "let B = () { 2 };".into()]
    }

    #[test]
    fn functions_go_before_the_query_and_after_its_leading_comments() {
        assert_eq!(
            with_functions("T | take 1", &added()),
            "let A = () { 1 };\nlet B = () { 2 };\nT | take 1"
        );
        assert_eq!(
            with_functions("// incident 5\n// second try\nT | take A()", &added()),
            "// incident 5\n// second try\nlet A = () { 1 };\nlet B = () { 2 };\nT | take A()"
        );
        assert_eq!(with_functions("T", &[]), "T");
    }

    #[test]
    fn functions_go_after_the_declarations_and_settings_they_may_depend_on() {
        assert_eq!(
            with_functions(
                "// label\ndeclare query_parameters(raid:string);\nset notruncation;\nT | where Id == raid",
                &added()
            ),
            "// label\ndeclare query_parameters(raid:string);\nset notruncation;\nlet A = () { 1 };\nlet B = () { 2 };\nT | where Id == raid"
        );
        assert_eq!(
            with_functions("DECLARE query_parameters(a:string,\n  b:long = 5); T", &added()),
            "DECLARE query_parameters(a:string,\n  b:long = 5);\nlet A = () { 1 };\nlet B = () { 2 };\n T",
            "a statement that shares its line with the declaration moves to the next one"
        );
        assert_eq!(
            with_functions("declare query_parameters(a:string);", &added()),
            "declare query_parameters(a:string);\nlet A = () { 1 };\nlet B = () { 2 };"
        );
    }

    #[test]
    fn a_query_that_is_not_a_declaration_is_not_mistaken_for_one() {
        assert_eq!(
            with_functions("setting | take 1\n// declare x;", &added()),
            "let A = () { 1 };\nlet B = () { 2 };\nsetting | take 1\n// declare x;"
        );
        assert_eq!(
            with_functions("print 'declare x;'", &added()),
            "let A = () { 1 };\nlet B = () { 2 };\nprint 'declare x;'"
        );
    }

    #[test]
    fn a_query_with_added_functions_has_no_blank_line_in_it() {
        let text = with_functions(
            "declare query_parameters(a:string);\nT\n| take 1",
            &added(),
        );
        assert!(!text.contains("\n\n"), "{text:?}");
    }
}
