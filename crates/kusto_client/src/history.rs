//! The runs kept in the history folder: what the picker lists, and which result files to delete
//! when there are too many.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::run_log::RunRecord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub cid: String,
    pub query: String,
    pub cluster: String,
    pub database: String,
    /// When the run started, as an RFC 3339 time.
    pub at: String,
    pub outcome: HistoryOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryOutcome {
    Finished {
        duration_ms: u64,
        rows: usize,
        path: PathBuf,
    },
    Failed {
        message: String,
    },
    /// A run with nothing to show: cancelled, or never reported an end.
    NoResult,
}

/// The runs in a run log, newest first. A run's later records replace its earlier ones, and a
/// line that is not a record is skipped.
pub fn history_entries(log: &str) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for record in log
        .lines()
        .filter_map(|line| serde_json::from_str::<RunRecord>(line).ok())
    {
        let (cid, query, cluster, database, at, outcome) = match record {
            RunRecord::Started {
                cid,
                query,
                cluster,
                database,
                at,
            } => (cid, query, cluster, database, at, HistoryOutcome::NoResult),
            RunRecord::Finished {
                cid,
                query,
                cluster,
                database,
                at,
                duration_ms,
                rows,
                path,
            } => (
                cid,
                query,
                cluster,
                database,
                at,
                HistoryOutcome::Finished {
                    duration_ms,
                    rows,
                    path: PathBuf::from(path),
                },
            ),
            RunRecord::Failed {
                cid,
                query,
                cluster,
                database,
                at,
                message,
            } => (
                cid,
                query,
                cluster,
                database,
                at,
                HistoryOutcome::Failed { message },
            ),
            RunRecord::Cancelled {
                cid,
                query,
                cluster,
                database,
                at,
            } => (cid, query, cluster, database, at, HistoryOutcome::NoResult),
        };
        let entry = HistoryEntry {
            cid: cid.clone(),
            query,
            cluster,
            database,
            at,
            outcome,
        };
        match positions.get(&cid) {
            Some(&position) => entries[position] = entry,
            None => {
                positions.insert(cid, entries.len());
                entries.push(entry);
            }
        }
    }
    entries.reverse();
    entries
}

/// Whether a file name is one a run makes, `<UTC date>-<UTC time>-<uuid>.ktt`. Only such files
/// are ever deleted, so a result saved into the folder by hand is safe.
pub fn is_history_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".ktt") else {
        return false;
    };
    let mut parts = stem.splitn(3, '-');
    let (Some(date), Some(time), Some(id)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let digits = |text: &str, length: usize| {
        text.len() == length && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    digits(date, 8) && digits(time, 6) && uuid::Uuid::parse_str(id).is_ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryFile {
    pub path: PathBuf,
    pub size: u64,
}

/// How much history to keep. A limit of zero is no limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_results: usize,
    pub max_bytes: u64,
}

/// The files to delete to meet the limits, oldest first. Files are kept newest first until one
/// would pass a limit; that one and every older one go. A file in `keep` is never listed, but
/// still counts against the limits, so what is open does not push out a newer result.
pub fn files_to_prune(
    mut files: Vec<HistoryFile>,
    keep: &[PathBuf],
    limits: HistoryLimits,
) -> Vec<PathBuf> {
    files.retain(|file| {
        file.path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_history_file_name)
    });
    files.sort_by(|left, right| right.path.file_name().cmp(&left.path.file_name()));

    let mut kept_count = 0;
    let mut kept_bytes = 0u64;
    let mut over_limit = false;
    let mut doomed = Vec::new();
    for file in files {
        if !over_limit {
            let count_ok = limits.max_results == 0 || kept_count < limits.max_results;
            let bytes_ok = limits.max_bytes == 0
                || kept_bytes.saturating_add(file.size) <= limits.max_bytes
                || kept_count == 0;
            over_limit = !(count_ok && bytes_ok);
        }
        if over_limit && !keep.contains(&file.path) {
            doomed.push(file.path);
        } else {
            kept_count += 1;
            kept_bytes = kept_bytes.saturating_add(file.size);
        }
    }
    doomed.reverse();
    doomed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::append_record;

    fn log(records: &[RunRecord]) -> String {
        records.iter().fold(String::new(), |log, record| {
            append_record(&log, record).expect("a record is written")
        })
    }

    fn started(cid: &str, at: &str) -> RunRecord {
        RunRecord::Started {
            cid: cid.into(),
            query: format!("query {cid}"),
            cluster: "help.kusto.windows.net".into(),
            database: "Samples".into(),
            at: at.into(),
        }
    }

    fn finished(cid: &str, at: &str, path: &str) -> RunRecord {
        RunRecord::Finished {
            cid: cid.into(),
            query: format!("query {cid}"),
            cluster: "help.kusto.windows.net".into(),
            database: "Samples".into(),
            at: at.into(),
            duration_ms: 1840,
            rows: 7,
            path: path.into(),
        }
    }

    #[test]
    fn a_runs_last_record_is_what_it_came_to_and_the_newest_run_is_first() {
        let text = log(&[
            started("a", "2026-10-01T10:00:00Z"),
            started("b", "2026-10-01T11:00:00Z"),
            finished("a", "2026-10-01T10:00:00Z", "/h/a.ktt"),
            RunRecord::Failed {
                cid: "b".into(),
                query: "query b".into(),
                cluster: "help.kusto.windows.net".into(),
                database: "Samples".into(),
                at: "2026-10-01T11:00:00Z".into(),
                message: "Semantic error".into(),
            },
            started("c", "2026-10-01T12:00:00Z"),
        ]);
        let entries = history_entries(&text);
        let summary: Vec<(&str, &HistoryOutcome)> = entries
            .iter()
            .map(|entry| (entry.cid.as_str(), &entry.outcome))
            .collect();
        assert_eq!(
            summary,
            [
                ("c", &HistoryOutcome::NoResult),
                (
                    "b",
                    &HistoryOutcome::Failed {
                        message: "Semantic error".into()
                    }
                ),
                (
                    "a",
                    &HistoryOutcome::Finished {
                        duration_ms: 1840,
                        rows: 7,
                        path: PathBuf::from("/h/a.ktt")
                    }
                ),
            ]
        );
        assert_eq!(entries[2].query, "query a");
        assert_eq!(entries[2].at, "2026-10-01T10:00:00Z");
    }

    #[test]
    fn a_run_whose_start_was_trimmed_is_still_listed_and_bad_lines_are_skipped() {
        let mut text = String::from("not json\n\n{\"event\":\"unknown\"}\n");
        text.push_str(&log(&[finished("a", "2026-10-01T10:00:00Z", "/h/a.ktt")]));
        let entries = history_entries(&text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cid, "a");
    }

    #[test]
    fn a_cancelled_run_has_no_result() {
        let text = log(&[RunRecord::Cancelled {
            cid: "a".into(),
            query: "q".into(),
            cluster: "c".into(),
            database: "d".into(),
            at: "2026-10-01T10:00:00Z".into(),
        }]);
        assert_eq!(history_entries(&text)[0].outcome, HistoryOutcome::NoResult);
    }

    #[test]
    fn only_the_files_a_run_makes_are_history_files() {
        let id = "da6a8e58-6566-4f58-bc02-34659abf407a";
        assert!(is_history_file_name(&format!("20261001-203917-{id}.ktt")));
        assert!(!is_history_file_name(&format!("20261001-203917-{id}.kqr")));
        assert!(!is_history_file_name(&format!(
            "20261001-203917-{id}.ktt.bak"
        )));
        assert!(!is_history_file_name("20261001-203917-notauuid.ktt"));
        assert!(!is_history_file_name("my-saved-result.ktt"));
        assert!(!is_history_file_name("runs.jsonl"));
        assert!(!is_history_file_name(&format!("2026100-203917-{id}.ktt")));
    }

    fn file(minute: u32, megabytes: u64) -> HistoryFile {
        HistoryFile {
            path: PathBuf::from(format!(
                "/h/20261001-10{minute:02}00-da6a8e58-6566-4f58-bc02-34659abf407a.ktt"
            )),
            size: megabytes * 1_000_000,
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy()[11..15].to_string())
            .collect()
    }

    #[test]
    fn the_oldest_results_past_the_count_go_first() {
        let files = (0..5).map(|minute| file(minute, 1)).collect();
        let limits = HistoryLimits {
            max_results: 3,
            max_bytes: 0,
        };
        assert_eq!(names(&files_to_prune(files, &[], limits)), ["0000", "0100"]);
    }

    #[test]
    fn the_size_limit_keeps_the_newest_results_that_fit() {
        let files = vec![file(1, 40), file(2, 40), file(3, 40), file(4, 10)];
        let limits = HistoryLimits {
            max_results: 0,
            max_bytes: 100_000_000,
        };
        // 10 + 40 + 40 fits; the next 40 would pass 100, so it and everything older go.
        assert_eq!(names(&files_to_prune(files, &[], limits)), ["0100"]);
    }

    #[test]
    fn a_result_that_alone_is_over_the_size_limit_is_still_kept_as_the_newest() {
        let files = vec![file(1, 500), file(2, 900)];
        let limits = HistoryLimits {
            max_results: 0,
            max_bytes: 100_000_000,
        };
        assert_eq!(names(&files_to_prune(files, &[], limits)), ["0100"]);
    }

    #[test]
    fn an_open_result_is_kept_but_counts_and_older_ones_still_go() {
        let files: Vec<HistoryFile> = (0..5).map(|minute| file(minute, 1)).collect();
        let open = vec![files[0].path.clone()];
        let limits = HistoryLimits {
            max_results: 2,
            max_bytes: 0,
        };
        assert_eq!(
            names(&files_to_prune(files, &open, limits)),
            ["0100", "0200"]
        );
    }

    #[test]
    fn no_limits_prune_nothing_and_other_files_are_never_listed() {
        let mut files: Vec<HistoryFile> = (0..5).map(|minute| file(minute, 1)).collect();
        files.push(HistoryFile {
            path: PathBuf::from("/h/mine.ktt"),
            size: 1_000_000_000,
        });
        let none = HistoryLimits {
            max_results: 0,
            max_bytes: 0,
        };
        assert!(files_to_prune(files.clone(), &[], none).is_empty());
        let tight = HistoryLimits {
            max_results: 1,
            max_bytes: 1,
        };
        let doomed = files_to_prune(files, &[], tight);
        assert_eq!(doomed.len(), 4);
        assert!(
            doomed
                .iter()
                .all(|path| path != &PathBuf::from("/h/mine.ktt"))
        );
    }
}
