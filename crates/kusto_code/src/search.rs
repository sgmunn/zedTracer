//! Finding text in local repositories with `git grep`.
//!
//! `git grep` reads the working tree, so uncommitted edits are searched too. Patterns are plain
//! text: a regular expression costs about twenty-five times the CPU of a plain string over these
//! repositories, and on macOS has no `\b` or `\s`, so a hit is filtered in Rust instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

/// Enough for any lookup this crate makes; a name that matches more than this is too common to
/// say anything.
const MAX_HITS: usize = 400;
const MAX_HIT_TEXT: usize = 300;

/// A git checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub root: PathBuf,
}

/// The repositories under `roots`: a root that is itself a checkout, and every directory one
/// level below a root that is. A root that cannot be read is logged and skipped.
pub fn discover_repositories(roots: &[PathBuf]) -> Vec<Repository> {
    let mut repositories = Vec::new();
    for root in roots {
        if is_checkout(root) {
            repositories.push(repository_at(root));
            continue;
        }
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!("cannot read the code folder {}: {error}", root.display());
                continue;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) if is_checkout(&entry.path()) => {
                    repositories.push(repository_at(&entry.path()));
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!("cannot read an entry of {}: {error}", root.display());
                }
            }
        }
    }
    repositories.sort_by(|left, right| left.name.cmp(&right.name));
    repositories.dedup_by(|left, right| left.root == right.root);
    repositories
}

/// A normal checkout has a `.git` directory; a linked worktree has a `.git` file.
fn is_checkout(path: &Path) -> bool {
    path.join(".git").exists()
}

fn repository_at(root: &Path) -> Repository {
    Repository {
        name: root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        root: root.to_path_buf(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepMode {
    /// The patterns are plain text.
    Fixed,
    /// The patterns are plain text that must stand alone as words.
    Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepQuery {
    pub mode: GrepMode,
    pub patterns: Vec<String>,
    /// Repository-relative files to search. Empty searches every `.cs` file.
    pub files: Vec<String>,
}

impl GrepQuery {
    pub fn fixed(patterns: Vec<String>) -> Self {
        Self::new(GrepMode::Fixed, patterns)
    }

    pub fn word(pattern: &str) -> Self {
        Self::words(vec![pattern.to_string()])
    }

    pub fn words(patterns: Vec<String>) -> Self {
        Self::new(GrepMode::Word, patterns)
    }

    pub fn in_files(mut self, files: Vec<String>) -> Self {
        self.files = files;
        self
    }

    fn new(mode: GrepMode, patterns: Vec<String>) -> Self {
        Self {
            mode,
            patterns,
            files: Vec::new(),
        }
    }

    fn arguments(&self) -> Vec<String> {
        let mut arguments: Vec<String> = ["grep", "-n", "-z", "-I", "--no-color"]
            .into_iter()
            .map(str::to_string)
            .collect();
        match self.mode {
            GrepMode::Fixed => arguments.push("-F".to_string()),
            GrepMode::Word => arguments.extend(["-F".to_string(), "-w".to_string()]),
        }
        for pattern in &self.patterns {
            arguments.push("-e".to_string());
            arguments.push(pattern.clone());
        }
        arguments.push("--".to_string());
        if self.files.is_empty() {
            arguments.push("*.cs".to_string());
        } else {
            arguments.extend(self.files.iter().map(|file| format!(":(literal){file}")));
        }
        arguments
    }
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Relative to the repository, with `/` separators.
    pub path: String,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

/// Reads `git grep -n -z` output: `path NUL line NUL text` on each line.
pub fn parse_grep_output(output: &[u8]) -> Vec<Hit> {
    let mut hits = Vec::new();
    for record in output.split(|byte| *byte == b'\n') {
        let mut fields = record.splitn(3, |byte| *byte == 0);
        let (Some(path), Some(line), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(line) = std::str::from_utf8(line)
            .ok()
            .and_then(|digits| digits.parse::<usize>().ok())
        else {
            continue;
        };
        let text = String::from_utf8_lossy(text);
        hits.push(Hit {
            path: String::from_utf8_lossy(path).into_owned(),
            line,
            text: text
                .trim_end_matches('\r')
                .chars()
                .take(MAX_HIT_TEXT)
                .collect(),
        });
        if hits.len() == MAX_HITS {
            break;
        }
    }
    hits
}

/// Runs `git grep` in repositories.
pub struct GitSearch {
    git: PathBuf,
    environment: HashMap<String, String>,
}

impl GitSearch {
    /// `environment` is the shell environment git runs in. A desktop app does not inherit the
    /// `PATH` of the user's shell.
    pub fn locate(environment: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let environment: HashMap<String, String> = environment.into_iter().collect();
        let git = match environment.get("PATH") {
            Some(search_path) => which::which_in("git", Some(search_path), "."),
            None => which::which("git"),
        }
        .map_err(|_| anyhow!("git was not found, so code cannot be searched"))?;
        Ok(Self::at(git, environment))
    }

    pub fn at(git: PathBuf, environment: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            git,
            environment: environment.into_iter().collect(),
        }
    }

    pub async fn grep(&self, repository: &Repository, query: &GrepQuery) -> Result<Vec<Hit>> {
        if query.patterns.is_empty() {
            return Ok(Vec::new());
        }
        let output = util::command::new_command(&self.git)
            .arg("-C")
            .arg(&repository.root)
            .args(query.arguments())
            .envs(&self.environment)
            // A search must not take the index lock a running editor or build may want.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(util::command::Stdio::null())
            .output()
            .await
            .with_context(|| format!("could not run git in {}", repository.root.display()))?;
        match output.status.code() {
            Some(0) => Ok(parse_grep_output(&output.stdout)),
            // git grep reports "nothing found" with status 1.
            Some(1) => Ok(Vec::new()),
            _ => bail!(
                "git grep failed in {}: {}",
                repository.root.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }

    /// The files git tracks in the repository, relative to it, with `/` separators.
    pub async fn list_files(&self, repository: &Repository) -> Result<Vec<String>> {
        let output = util::command::new_command(&self.git)
            .arg("-C")
            .arg(&repository.root)
            .args(["ls-files", "-z"])
            .envs(&self.environment)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(util::command::Stdio::null())
            .output()
            .await
            .with_context(|| format!("could not run git in {}", repository.root.display()))?;
        if !output.status.success() {
            bail!(
                "git ls-files failed in {}: {}",
                repository.root.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_nul_delimited_output() {
        let output = b"src/A.cs\x0012\x00    var x = 1;\r\nsrc/B:odd.cs\x003\x00y\n";
        assert_eq!(
            parse_grep_output(output),
            vec![
                Hit {
                    path: "src/A.cs".into(),
                    line: 12,
                    text: "    var x = 1;".into()
                },
                Hit {
                    path: "src/B:odd.cs".into(),
                    line: 3,
                    text: "y".into()
                },
            ]
        );
    }

    #[test]
    fn skips_records_that_are_not_hits() {
        assert!(parse_grep_output(b"").is_empty());
        assert!(parse_grep_output(b"Binary file matches\nsrc/A.cs\x00x\x00text\n").is_empty());
    }

    #[test]
    fn stops_at_the_hit_limit() {
        let output: Vec<u8> = (1..=MAX_HITS + 50)
            .flat_map(|line| format!("a.cs\0{line}\0x\n").into_bytes())
            .collect();
        assert_eq!(parse_grep_output(&output).len(), MAX_HITS);
    }

    #[test]
    fn builds_the_arguments_for_each_mode() {
        let word = GrepQuery::word("Name").arguments();
        assert_eq!(
            word,
            [
                "grep",
                "-n",
                "-z",
                "-I",
                "--no-color",
                "-F",
                "-w",
                "-e",
                "Name",
                "--",
                "*.cs"
            ]
        );

        let several = GrepQuery::words(vec!["class A".into(), "struct A".into()])
            .in_files(vec!["src/A.cs".into()])
            .arguments();
        assert_eq!(
            &several[5..],
            [
                "-F",
                "-w",
                "-e",
                "class A",
                "-e",
                "struct A",
                "--",
                ":(literal)src/A.cs"
            ]
        );

        let fixed = GrepQuery::fixed(vec!["\"x\"".into()]).arguments();
        assert_eq!(&fixed[5..], ["-F", "-e", "\"x\"", "--", "*.cs"]);
    }

    #[test]
    fn finds_checkouts_one_level_below_a_root() {
        let root = tempfile::tempdir().expect("a temporary directory");
        for name in ["beta", "alpha"] {
            fs::create_dir_all(root.path().join(name).join(".git")).expect("a checkout");
        }
        fs::create_dir_all(root.path().join("not-a-repository")).expect("a folder");
        // A linked worktree has a `.git` file, not a directory.
        fs::create_dir_all(root.path().join("linked")).expect("a folder");
        fs::write(root.path().join("linked").join(".git"), "gitdir: elsewhere").expect("a file");

        let found = discover_repositories(&[root.path().to_path_buf()]);
        let names: Vec<&str> = found
            .iter()
            .map(|repository| repository.name.as_str())
            .collect();
        assert_eq!(names, ["alpha", "beta", "linked"]);
    }

    #[test]
    fn a_root_that_is_a_checkout_is_the_repository() {
        let root = tempfile::tempdir().expect("a temporary directory");
        fs::create_dir_all(root.path().join(".git")).expect("a checkout");
        fs::create_dir_all(root.path().join("inner").join(".git")).expect("a nested checkout");

        let found = discover_repositories(&[root.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, root.path());
    }

    #[test]
    fn a_missing_root_is_skipped() {
        let root = tempfile::tempdir().expect("a temporary directory");
        assert!(discover_repositories(&[root.path().join("missing")]).is_empty());
    }
}
