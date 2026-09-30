//! Runs the crate against the fixtures in `fork-docs/samples` and checks every result the
//! expected-results files record.

use std::path::PathBuf;
use std::time::Instant;

use kusto_results::activity::{HierarchyIssue, Strength, build_projection};
use kusto_results::filter::{ColumnFilter, Condition, FilterOperator, Join};
use kusto_results::inspector::{
    FieldValue, InspectorSubject, assemble_multipart, format_call_stack, merged_field_value,
    resolve_subject, structured_json,
};
use kusto_results::view::{
    SortColumn, SortDirection, SortState, ViewState, severity_column, severity_level, visible_rows,
};
use kusto_results::{Cell, ResultSet, Table};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn samples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fork-docs/samples")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(samples().join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn load(name: &str) -> ResultSet {
    ResultSet::from_json(&read(name)).unwrap_or_else(|error| panic!("{name}: {error:#}"))
}

fn expected(name: &str) -> Value {
    serde_json::from_str(&read(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn always() -> bool {
    true
}

fn rows_of(value: &Value) -> Vec<usize> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_u64().map(|row| row as usize))
                .collect()
        })
        .unwrap_or_default()
}

fn operator(name: &str) -> FilterOperator {
    match name {
        "contains" => FilterOperator::Contains,
        "notContains" => FilterOperator::NotContains,
        "equals" => FilterOperator::Equals,
        "notEquals" => FilterOperator::NotEquals,
        "startsWith" => FilterOperator::StartsWith,
        "greaterThan" => FilterOperator::GreaterThan,
        "greaterThanOrEqual" => FilterOperator::GreaterThanOrEqual,
        "lessThan" => FilterOperator::LessThan,
        "lessThanOrEqual" => FilterOperator::LessThanOrEqual,
        "isEmpty" => FilterOperator::IsEmpty,
        "isNotEmpty" => FilterOperator::IsNotEmpty,
        "isTrue" => FilterOperator::IsTrue,
        "isFalse" => FilterOperator::IsFalse,
        other => panic!("unknown operator {other}"),
    }
}

#[test]
fn types_fixture_round_trips_without_losing_anything() {
    let original_text = read("synthetic-types.ktt");
    let result = ResultSet::from_json(&original_text).unwrap();
    assert_eq!(
        result
            .tables
            .iter()
            .map(|table| table.rows.len())
            .collect::<Vec<_>>(),
        [40, 4, 0]
    );
    assert_eq!(result.total_rows(), 44);

    let written = result.to_json().unwrap();
    assert_eq!(ResultSet::from_json(&written).unwrap(), result);
    // The same JSON value, including number forms and dynamic key order.
    let before: Value = serde_json::from_str(&original_text).unwrap();
    let after: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(after, before);
    for literal in [
        "79228162514264337593543950335",
        "9007199254740993",
        "-9223372036854775808",
    ] {
        assert!(written.contains(literal), "{literal} lost");
    }
}

#[test]
fn legacy_files_open_the_same_way() {
    let result = load("synthetic-legacy.kqr");
    assert_eq!(result.tables.len(), 1);
    assert_eq!(result.tables[0].rows.len(), 10);
}

#[test]
fn types_fixture_keeps_the_saved_column_layout() {
    let result = load("synthetic-types.ktt");
    let view = result.table_view("PrimaryResult").unwrap();
    assert_eq!(view.gutter_width, Some(64));
    let order = kusto_results::view::display_column_order(12, Some(view));
    assert_eq!(order, [6, 0, 7, 1, 2, 3, 4, 5, 8, 9, 10, 11]);
}

#[test]
fn sorts_match_the_expected_orders() {
    let result = load("synthetic-types.ktt");
    let table = &result.tables[0];
    let expected = expected("synthetic-types.expected.json");
    for (column_name, orders) in expected["sorts"].as_object().unwrap() {
        let column = table.column_index(column_name).unwrap();
        for (label, direction) in [
            ("ascending", SortDirection::Ascending),
            ("descending", SortDirection::Descending),
        ] {
            let state = ViewState {
                sort: SortState {
                    active: Some((SortColumn::Column(column), direction)),
                },
                ..Default::default()
            };
            assert_eq!(
                visible_rows(table, &state, None, &always).unwrap(),
                rows_of(&orders[label]),
                "{column_name} {label}"
            );
        }
    }
}

#[test]
fn filters_match_the_expected_rows() {
    let result = load("synthetic-types.ktt");
    let table = &result.tables[0];
    let expected = expected("synthetic-types.expected.json");
    let mut checked = 0;
    for case in expected["filters"].as_array().unwrap() {
        let column_name = case["column"].as_str().unwrap();
        if column_name == "combined" {
            continue;
        }
        let column = table.column_index(column_name).unwrap();
        let value = match &case["value"] {
            Value::Null => String::new(),
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let mut state = ViewState::default();
        state.filters.insert(
            column,
            ColumnFilter {
                join: Join::All,
                conditions: vec![Condition::new(
                    operator(case["operator"].as_str().unwrap()),
                    value,
                )],
            },
        );
        assert_eq!(
            visible_rows(table, &state, None, &always).unwrap(),
            rows_of(&case["matchingRows"]),
            "{column_name} {} {}",
            case["operator"],
            case["value"]
        );
        checked += 1;
    }
    assert_eq!(checked, 15);
}

#[test]
fn natural_order_facts_hold() {
    use kusto_results::typed::natural_cmp;
    let expected = expected("synthetic-types.expected.json");
    for pair in expected["pairwiseStringOrder"].as_array().unwrap() {
        let before = pair["before"].as_str().unwrap().to_lowercase();
        let after = pair["after"].as_str().unwrap().to_lowercase();
        assert!(natural_cmp(&before, &after).is_lt(), "{before} < {after}");
    }
}

fn collect_call_stacks(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_call_stacks(item, found)),
        Value::Object(map) => {
            for (key, child) in map {
                match child {
                    Value::String(stack) if key.eq_ignore_ascii_case("callstack") => {
                        found.push(stack.clone())
                    }
                    other => collect_call_stacks(other, found),
                }
            }
        }
        _ => {}
    }
}

fn trace_edge() -> (ResultSet, Value) {
    (
        load("synthetic-trace-edge.ktt"),
        expected("synthetic-trace-edge.expected.json"),
    )
}

#[test]
fn activities_match_the_expected_hierarchy_and_severity() {
    let (result, expected) = trace_edge();
    let table = &result.tables[0];
    let projection = build_projection(table).unwrap();
    let structured = &expected["structured"];
    let listed = structured["activities"].as_array().unwrap();
    assert_eq!(projection.activities.len(), listed.len());

    let by_first_row = |row: u64| {
        projection
            .activities
            .iter()
            .position(|activity| activity.event_rows.first() == Some(&(row as usize)))
            .unwrap_or_else(|| panic!("no activity starts at row {row}"))
    };
    let name_of = |index: usize| {
        let first = projection.activities[index].event_rows[0] as u64;
        listed
            .iter()
            .find(|entry| entry["firstSourceRow"] == first)
            .and_then(|entry| entry["name"].as_str())
            .unwrap()
            .to_string()
    };

    for entry in listed {
        let name = entry["name"].as_str().unwrap();
        let index = by_first_row(entry["firstSourceRow"].as_u64().unwrap());
        let activity = &projection.activities[index];
        assert_eq!(
            activity.parent.map(name_of).as_deref(),
            entry["parent"].as_str(),
            "{name} parent"
        );
        let issue = match entry["hierarchyIssue"].as_str() {
            Some("orphan") => Some(HierarchyIssue::Orphan),
            Some("conflictingParents") => Some(HierarchyIssue::ConflictingParents),
            Some("cycle") => Some(HierarchyIssue::Cycle),
            _ => None,
        };
        assert_eq!(activity.issue, issue, "{name} issue");
        assert_eq!(
            activity.has_activity_id,
            entry["hierarchyIssue"] != "missingCurrentActivityId",
            "{name} id"
        );
        let severity = activity.severity.map(|severity| {
            (
                severity.level as u64,
                match severity.strength {
                    Strength::Full => "full",
                    Strength::Muted => "muted",
                },
            )
        });
        let wanted = entry["severity"].as_object().map(|severity| {
            (
                severity["level"].as_u64().unwrap(),
                severity["strength"].as_str().unwrap(),
            )
        });
        assert_eq!(severity, wanted, "{name} severity");
        assert_eq!(
            activity.shows_warning_triangle(),
            entry["warningTriangle"].as_bool().unwrap(),
            "{name} triangle"
        );
        assert_eq!(
            activity.event_rows.len() as u64,
            entry["eventCount"],
            "{name} events"
        );
        assert_eq!(
            activity.subtree_activity_count as u64, entry["subtreeActivityCount"],
            "{name} subtree"
        );
        assert_eq!(
            activity.max_descendant_depth as u64, entry["maxDescendantDepth"],
            "{name} depth"
        );
    }

    let roots: Vec<String> = projection
        .activities
        .iter()
        .enumerate()
        .filter(|(_, activity)| activity.parent.is_none())
        .map(|(index, _)| name_of(index))
        .collect();
    let wanted_roots: Vec<String> = structured["rootsInFirstObservedOrder"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_string())
        .collect();
    assert_eq!(roots, wanted_roots);

    let (depth, deepest) = projection.deepest();
    assert_eq!(depth as u64, structured["deepest"]["depth"]);
    let deepest_names: Vec<String> = deepest.into_iter().map(name_of).collect();
    let wanted_deepest: Vec<String> = structured["deepest"]["activities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_string())
        .collect();
    assert_eq!(deepest_names, wanted_deepest);
    assert_eq!(projection.rows.len(), table.rows.len());
}

#[test]
fn severity_tints_match_the_expected_rows() {
    let (result, expected) = trace_edge();
    let table = &result.tables[0];
    let column = severity_column(table).unwrap();
    let untinted: Vec<usize> = (0..table.rows.len())
        .filter(|row| severity_level(table.cell(*row, column)).is_none())
        .collect();
    assert_eq!(untinted, rows_of(&expected["severityRows"]["untintedRows"]));
    for (level, count) in expected["severityRows"]["tintedCountByLevel"]
        .as_object()
        .unwrap()
    {
        let level: u8 = level.parse().unwrap();
        let tinted = (0..table.rows.len())
            .filter(|row| severity_level(table.cell(*row, column)) == Some(level))
            .count();
        assert_eq!(tinted as u64, *count, "level {level}");
    }
}

#[test]
fn multipart_selections_match_the_expected_outcomes() {
    let (result, expected) = trace_edge();
    let table = &result.tables[0];
    for case in expected["multipart"].as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let rows = rows_of(&case["rows"]);
        let fields = assemble_multipart(table, &rows);
        let outcome = &case["expect"];
        if outcome["assembled"].as_bool().unwrap() {
            assert_eq!(fields.len(), 1, "{name}");
            assert_eq!(fields[0].text, outcome["text"].as_str().unwrap(), "{name}");
            assert_eq!(fields[0].total, rows.len(), "{name}");
            let is_json = matches!(merged_field_value(&fields[0].text), FieldValue::Json(_));
            assert_eq!(is_json, outcome["isJson"].as_bool().unwrap(), "{name} json");
            assert!(matches!(
                resolve_subject(table, &rows),
                InspectorSubject::Assembled { .. }
            ));
        } else {
            assert!(fields.is_empty(), "{name}");
            assert!(matches!(
                resolve_subject(table, &rows),
                InspectorSubject::FirstOfMany { .. }
            ));
        }
    }
}

#[test]
fn call_stacks_match_the_expected_formatting() {
    let (result, expected) = trace_edge();
    let table = &result.tables[0];
    for case in expected["callStacks"].as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let row = case["row"].as_u64().unwrap() as usize;
        let column = table
            .column_index(case["column"].as_str().unwrap())
            .unwrap();
        let cell = table.cell(row, column).unwrap();
        let structured = structured_json(cell);
        assert_eq!(
            structured.is_some(),
            case["isJson"].as_bool().unwrap(),
            "{name} json"
        );
        let mut stacks = Vec::new();
        if let Some(value) = &structured {
            collect_call_stacks(value, &mut stacks);
        }
        let formatted: Vec<String> = stacks
            .iter()
            .map(|stack| format_call_stack(stack))
            .collect();
        let wanted: Vec<String> = case["expectedCallStacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stack| stack.as_str().unwrap().to_string())
            .collect();
        assert_eq!(formatted, wanted, "{name}");
    }
}

#[test]
fn a_selection_of_one_row_resolves_to_that_row() {
    let (result, _) = trace_edge();
    assert_eq!(
        resolve_subject(&result.tables[0], &[5]),
        InspectorSubject::Row { row: 5 }
    );
}

/// The real captures are git-ignored and exist only on the machine that made them, so these
/// checks run when the files are present and quietly pass otherwise.
#[test]
fn real_captures_load_when_present() {
    for (name, rows) in [("sample1.ktt", 1002), ("sample2.ktt", 6413)] {
        if !samples().join(name).exists() {
            continue;
        }
        let result = load(name);
        let table = &result.tables[0];
        assert_eq!(table.rows.len(), rows, "{name}");
        let projection = build_projection(table).unwrap();
        assert_eq!(projection.rows.len(), rows, "{name}");
        assert_eq!(
            result
                .to_json()
                .map(|text| ResultSet::from_json(&text).map(|again| again == result))
                .ok()
                .and_then(Result::ok),
            Some(true),
            "{name} round trip"
        );
    }
}

/// A scale baseline on the generated 200,000-row file. Run it on demand:
/// `cargo test -p kusto_results --release --test sample_fixtures -- --ignored --nocapture`
#[test]
#[ignore = "needs generated/synthetic-large.ktt; run generate_samples.py --large first"]
fn large_fixture_baseline() {
    let path = samples().join("generated/synthetic-large.ktt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    let time = |label: &str, started: Instant| {
        println!(
            "{label:<34} {:>8.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    };

    let started = Instant::now();
    let result = ResultSet::from_json(&text).unwrap();
    time("parse file", started);
    drop(text);
    let table: &Table = &result.tables[0];
    println!(
        "{} rows x {} columns",
        table.rows.len(),
        table.columns.len()
    );

    let sort = |name: &str, direction: SortDirection| {
        let column = table.column_index(name).unwrap();
        let state = ViewState {
            sort: SortState {
                active: Some((SortColumn::Column(column), direction)),
            },
            ..Default::default()
        };
        let started = Instant::now();
        let rows = visible_rows(table, &state, None, &always).unwrap();
        time(&format!("sort {name} {direction:?}"), started);
        rows
    };
    sort("Id", SortDirection::Descending);
    sort("TIMESTAMP", SortDirection::Descending);
    sort("Elapsed", SortDirection::Ascending);
    sort("MessageText", SortDirection::Ascending);
    sort("Bucket", SortDirection::Ascending);

    let mut state = ViewState::default();
    state.filters.insert(
        table.column_index("Level").unwrap(),
        ColumnFilter {
            join: Join::All,
            conditions: vec![Condition::new(FilterOperator::LessThanOrEqual, "3")],
        },
    );
    let started = Instant::now();
    let filtered = visible_rows(table, &state, None, &always).unwrap();
    time("filter Level <= 3", started);
    println!("  -> {} rows", filtered.len());

    let state = ViewState {
        search: "timeout retry".into(),
        ..Default::default()
    };
    let started = Instant::now();
    let searched = visible_rows(table, &state, None, &always).unwrap();
    time("search \"timeout retry\"", started);
    println!("  -> {} rows", searched.len());

    let started = Instant::now();
    let projection = build_projection(table).unwrap();
    time("activity projection", started);
    println!("  -> {} activities", projection.activities.len());

    let started = Instant::now();
    let written = result.to_json().unwrap();
    time("write file", started);
    println!("  -> {} bytes", written.len());
    println!("size of one cell: {} bytes", std::mem::size_of::<Cell>());
}
