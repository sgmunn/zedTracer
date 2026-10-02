//! What the editor records about each run, for the language server to show above the query.
//!
//! The editor appends one JSON line when a run starts and one when it ends, to `runs.jsonl` in
//! the history folder. The language server only reads the file, so nothing connects the two
//! processes but the file itself.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub const RUN_LOG_FILE: &str = "runs.jsonl";

/// A run is two records or more, so this keeps about the last two hundred runs.
const KEPT_RECORDS: usize = 400;

/// Every record names its run by client request id and carries the query, so a record is
/// meaningful on its own when the start of the run has been trimmed from the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunRecord {
    Started {
        cid: String,
        query: String,
        cluster: String,
        database: String,
        /// When the run started, as an RFC 3339 time.
        at: String,
    },
    Finished {
        cid: String,
        query: String,
        cluster: String,
        database: String,
        at: String,
        duration_ms: u64,
        rows: usize,
        /// The history file holding the result.
        path: String,
    },
    Failed {
        cid: String,
        query: String,
        cluster: String,
        database: String,
        at: String,
        message: String,
    },
    Cancelled {
        cid: String,
        query: String,
        cluster: String,
        database: String,
        at: String,
    },
}

/// The log with `record` added at the end and the oldest records dropped past the limit.
pub fn append_record(existing: &str, record: &RunRecord) -> Result<String> {
    let line = serde_json::to_string(record).context("could not write the run record")?;
    let mut lines: Vec<&str> = existing
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines.push(&line);
    let start = lines.len().saturating_sub(KEPT_RECORDS);
    let mut text = lines[start..].join("\n");
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(cid: &str) -> RunRecord {
        RunRecord::Started {
            cid: cid.into(),
            query: "T | take 1".into(),
            cluster: "help.kusto.windows.net".into(),
            database: "Samples".into(),
            at: "2026-10-01T14:00:00.000Z".into(),
        }
    }

    #[test]
    fn a_record_is_one_line_with_names_the_language_server_reads() {
        let text = append_record("", &started("id-1")).unwrap();
        assert_eq!(text.lines().count(), 1);
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["event"], "started");
        assert_eq!(value["cid"], "id-1");
        assert_eq!(value["query"], "T | take 1");

        let finished = RunRecord::Finished {
            cid: "id-1".into(),
            query: "q".into(),
            cluster: "c".into(),
            database: "d".into(),
            at: "2026-10-01T14:00:00.000Z".into(),
            duration_ms: 1840,
            rows: 1240,
            path: "/history/a.ktt".into(),
        };
        let value: serde_json::Value =
            serde_json::from_str(append_record("", &finished).unwrap().trim()).unwrap();
        assert_eq!(value["event"], "finished");
        assert_eq!(value["durationMs"], 1840);
        assert_eq!(value["rows"], 1240);
        assert_eq!(value["path"], "/history/a.ktt");
    }

    #[test]
    fn a_query_with_line_breaks_stays_on_one_line() {
        let record = RunRecord::Started {
            cid: "id".into(),
            query: "T\n| take 1".into(),
            cluster: "c".into(),
            database: "d".into(),
            at: "x".into(),
        };
        assert_eq!(append_record("", &record).unwrap().lines().count(), 1);
    }

    #[test]
    fn records_are_added_at_the_end() {
        let text = append_record("", &started("a")).unwrap();
        let text = append_record(&text, &started("b")).unwrap();
        let cids: Vec<String> = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["cid"].to_string())
            .collect();
        assert_eq!(cids, ["\"a\"", "\"b\""]);
    }

    #[test]
    fn the_oldest_records_are_dropped_past_the_limit() {
        let mut text = String::new();
        for index in 0..KEPT_RECORDS + 25 {
            text = append_record(&text, &started(&index.to_string())).unwrap();
        }
        assert_eq!(text.lines().count(), KEPT_RECORDS);
        assert!(text.lines().next().unwrap().contains("\"cid\":\"25\""));
        assert!(
            text.lines()
                .last()
                .unwrap()
                .contains(&format!("\"cid\":\"{}\"", KEPT_RECORDS + 24))
        );
    }

    #[test]
    fn blank_lines_in_an_existing_log_are_ignored() {
        let text = append_record("\n\n", &started("a")).unwrap();
        assert_eq!(text.lines().count(), 1);
    }
}
