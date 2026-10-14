mod extension_snippet;
pub mod format;
mod registry;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use collections::{BTreeMap, BTreeSet, HashMap};
use format::VsSnippetsFile;
use fs::Fs;
use futures::stream::StreamExt;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Task, WeakEntity};
pub use registry::*;
use util::ResultExt;

pub fn init(cx: &mut App) {
    SnippetRegistry::init_global(cx);
    extension_snippet::init(cx);
}

/// Language name, or `None` if the snippet file is global.
type SnippetKind = Option<String>;
fn file_stem_to_key(stem: &str) -> SnippetKind {
    if stem == "snippets" {
        None
    } else {
        Some(stem.to_owned())
    }
}

pub fn file_to_snippets(
    file_contents: VsSnippetsFile,
    source: &Path,
) -> impl Iterator<Item = Result<Arc<Snippet>>> {
    file_contents
        .snippets
        .into_iter()
        .map(move |(name, snippet)| {
            let snippet_name = name.clone();
            let prefixes = snippet
                .prefix
                .map_or_else(move || vec![snippet_name], |prefixes| prefixes.into());
            let description = snippet
                .description
                .map(|description| description.to_string());
            let body = snippet.body.to_string();
            match snippet::Snippet::parse(&body) {
                Ok(_) => Ok(Arc::new(Snippet {
                    body,
                    prefix: prefixes,
                    description,
                    name,
                })),
                Err(e) => Err(anyhow::anyhow!(
                    "Invalid snippet '{name}' in {source:?}: {e:#}"
                )),
            }
        })
}

/// What adding a snippet to a snippets file did.
#[derive(Debug, PartialEq, Eq)]
pub enum AddedSnippet {
    Added(PathBuf),
    /// The file already has a snippet of that name, and is left as it was.
    NameTaken,
}

/// Adds a snippet whose body is `text`, as it is, to the file of the snippets of `scope` in
/// `directory`, creating the file when there is none. The other snippets of the file and its
/// comments are kept, so a file the user edits by hand is not rewritten.
pub async fn add_snippet(
    fs: &dyn Fs,
    directory: &Path,
    scope: &str,
    name: &str,
    prefix: &str,
    text: &str,
) -> Result<AddedSnippet> {
    let path = directory.join(format!("{scope}.json"));
    let contents = if fs.is_file(&path).await {
        fs.load(&path)
            .await
            .with_context(|| format!("could not read {path:?}"))?
    } else {
        String::new()
    };
    let Some(updated) = with_snippet(&contents, name, prefix, text)
        .with_context(|| format!("could not add the snippet to {path:?}"))?
    else {
        return Ok(AddedSnippet::NameTaken);
    };
    fs.create_dir(directory)
        .await
        .with_context(|| format!("could not create {directory:?}"))?;
    fs.atomic_write(path.clone(), updated)
        .await
        .with_context(|| format!("could not write {path:?}"))?;
    Ok(AddedSnippet::Added(path))
}

/// The text of a snippets file with one more snippet, or `None` when it has one of that name.
fn with_snippet(contents: &str, name: &str, prefix: &str, text: &str) -> Result<Option<String>> {
    let contents = if contents.trim().is_empty() {
        "{}"
    } else {
        contents
    };
    let existing: serde_json::Value =
        serde_json_lenient::from_str(contents).context("the file is not valid JSON")?;
    let existing = existing
        .as_object()
        .context("the file does not hold an object of snippets")?;
    if existing.contains_key(name) {
        return Ok(None);
    }
    let (last_entry_end, needs_comma) =
        end_of_entries(contents).context("the file does not hold an object of snippets")?;
    // A comment on the line of the last entry is about that entry, so the new one goes below it.
    let rest_of_line = contents[last_entry_end..]
        .split('\n')
        .next()
        .unwrap_or_default();
    let rest_of_line_is_comment = {
        let rest = rest_of_line.trim_start();
        rest.is_empty() || rest.starts_with("//")
    };
    let entry_at = if rest_of_line_is_comment {
        last_entry_end + rest_of_line.len()
    } else {
        last_entry_end
    };

    let body = text.split('\n').map(snippet::escape).collect::<Vec<_>>();
    let tab_size = settings_json::infer_json_indent_size(contents);
    let entry = settings_json::to_pretty_json(
        &serde_json::json!({ "prefix": prefix, "body": body }),
        tab_size,
        tab_size,
    );
    let (before_comma, rest) = contents.split_at(last_entry_end);
    let (rest_of_entry_line, after) = rest.split_at(entry_at - last_entry_end);
    let closes_on_own_line = after
        .trim_start_matches([' ', '\t'])
        .starts_with(['\n', '\r']);
    Ok(Some(format!(
        "{before_comma}{comma}{rest_of_entry_line}\n{indent}{key}: {entry}{newline}{after}{end}",
        comma = if needs_comma { "," } else { "" },
        indent = " ".repeat(tab_size),
        key = serde_json::to_string(name)?,
        newline = if closes_on_own_line { "" } else { "\n" },
        end = if after.ends_with('\n') { "" } else { "\n" },
    )))
}

/// Where a new entry goes in the text of a JSON object that may have comments: right after its
/// last entry, so that the comment above an entry stays with it, and whether a comma has to come
/// first.
fn end_of_entries(contents: &str) -> Option<(usize, bool)> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut last_significant: Option<(usize, char)> = None;
    let mut characters = contents.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        if in_string {
            match character {
                '\\' => {
                    characters.next();
                }
                '"' => {
                    in_string = false;
                    last_significant = Some((offset + 1, character));
                }
                _ => {}
            }
            continue;
        }
        match character {
            '/' if characters.peek().is_some_and(|(_, next)| *next == '/') => {
                for (_, next) in characters.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
                continue;
            }
            '/' if characters.peek().is_some_and(|(_, next)| *next == '*') => {
                characters.next();
                let mut previous = ' ';
                for (_, next) in characters.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
                continue;
            }
            _ if character.is_whitespace() => continue,
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let (end, last) = last_significant?;
                    return Some((end, last != '{' && last != ','));
                }
            }
            _ => {}
        }
        last_significant = Some((offset + character.len_utf8(), character));
    }
    None
}

// Snippet with all of the metadata
#[derive(Debug)]
pub struct Snippet {
    pub prefix: Vec<String>,
    pub body: String,
    pub description: Option<String>,
    pub name: String,
}

async fn process_updates(
    this: WeakEntity<SnippetProvider>,
    entries: Vec<PathBuf>,
    mut cx: AsyncApp,
) -> Result<()> {
    let fs = this.read_with(&cx, |this, _| this.fs.clone())?;
    for entry_path in entries {
        if entry_path
            .extension()
            .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let entry_metadata = fs.metadata(&entry_path).await;
        // Entry could have been removed, in which case we should no longer show completions for it.
        let entry_exists = entry_metadata.is_ok();
        if entry_metadata.is_ok_and(|entry| entry.is_some_and(|e| e.is_dir)) {
            // Don't process dirs.
            continue;
        }
        let Some(stem) = entry_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let key = file_stem_to_key(stem);

        let contents = if entry_exists {
            fs.load(&entry_path).await.ok()
        } else {
            None
        };

        this.update(&mut cx, move |this, _| {
            let snippets_of_kind = this.snippets.entry(key).or_default();
            if entry_exists {
                let Some(file_contents) = contents else {
                    return;
                };
                let Ok(as_json) = serde_json_lenient::from_str::<VsSnippetsFile>(&file_contents)
                else {
                    return;
                };
                let snippets = file_to_snippets(as_json, entry_path.as_path());
                *snippets_of_kind.entry(entry_path).or_default() =
                    snippets.filter_map(Result::log_err).collect();
            } else {
                snippets_of_kind.remove(&entry_path);
            }
        })?;
    }
    Ok(())
}

async fn initial_scan(
    this: WeakEntity<SnippetProvider>,
    path: Arc<Path>,
    cx: AsyncApp,
) -> Result<()> {
    let fs = this.read_with(&cx, |this, _| this.fs.clone())?;
    let entries = fs.read_dir(&path).await;
    if let Ok(entries) = entries {
        let entries = entries
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        process_updates(this, entries, cx).await?;
    }
    Ok(())
}

pub struct SnippetProvider {
    fs: Arc<dyn Fs>,
    snippets: HashMap<SnippetKind, BTreeMap<PathBuf, Vec<Arc<Snippet>>>>,
    watch_tasks: Vec<Task<Result<()>>>,
}

// Watches global snippet directory, is created just once and reused across multiple projects
struct GlobalSnippetWatcher(Entity<SnippetProvider>);

impl GlobalSnippetWatcher {
    fn new(fs: Arc<dyn Fs>, cx: &mut App) -> Self {
        let global_snippets_dir = paths::snippets_dir();
        let provider = cx.new(|_cx| SnippetProvider {
            fs,
            snippets: Default::default(),
            watch_tasks: vec![],
        });
        provider.update(cx, |this, cx| this.watch_directory(global_snippets_dir, cx));
        Self(provider)
    }
}

impl gpui::Global for GlobalSnippetWatcher {}

impl SnippetProvider {
    pub fn new(fs: Arc<dyn Fs>, dirs_to_watch: BTreeSet<PathBuf>, cx: &mut App) -> Entity<Self> {
        cx.new(move |cx| {
            if !cx.has_global::<GlobalSnippetWatcher>() {
                let global_watcher = GlobalSnippetWatcher::new(fs.clone(), cx);
                cx.set_global(global_watcher);
            }
            let mut this = Self {
                fs,
                watch_tasks: Vec::new(),
                snippets: Default::default(),
            };

            for dir in dirs_to_watch {
                this.watch_directory(&dir, cx);
            }

            this
        })
    }

    /// Add directory to be watched for content changes
    fn watch_directory(&mut self, path: &Path, cx: &Context<Self>) {
        let path: Arc<Path> = Arc::from(path);

        self.watch_tasks.push(cx.spawn(async move |this, cx| {
            let fs = this.read_with(cx, |this, _| this.fs.clone())?;
            let watched_path = path.clone();
            let watcher = fs.watch(&watched_path, Duration::from_secs(1));
            initial_scan(this.clone(), path, cx.clone()).await?;

            let (mut entries, _) = watcher.await;
            while let Some(entries) = entries.next().await {
                process_updates(
                    this.clone(),
                    entries.into_iter().map(|event| event.path).collect(),
                    cx.clone(),
                )
                .await?;
            }
            Ok(())
        }));
    }

    fn lookup_snippets<'a, const LOOKUP_GLOBALS: bool>(
        &'a self,
        language: &'a SnippetKind,
        cx: &App,
    ) -> Vec<Arc<Snippet>> {
        let mut user_snippets: Vec<_> = self
            .snippets
            .get(language)
            .cloned()
            .unwrap_or_default()
            .into_values()
            .flat_map(|snippets| snippets.into_iter())
            .collect();
        if LOOKUP_GLOBALS {
            if let Some(global_watcher) = cx.try_global::<GlobalSnippetWatcher>() {
                user_snippets.extend(
                    global_watcher
                        .0
                        .read(cx)
                        .lookup_snippets::<false>(language, cx),
                );
            }

            let Some(registry) = SnippetRegistry::try_global(cx) else {
                return user_snippets;
            };

            let registry_snippets = registry.get_snippets(language);
            user_snippets.extend(registry_snippets);
        }

        user_snippets
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn add_snippet_for_test(
        &mut self,
        language: SnippetKind,
        path: PathBuf,
        snippet: Vec<Arc<Snippet>>,
    ) {
        self.snippets
            .entry(language)
            .or_default()
            .insert(path, snippet);
    }

    pub fn snippets_for(&self, language: SnippetKind, cx: &App) -> Vec<Arc<Snippet>> {
        let mut requested_snippets = self.lookup_snippets::<true>(&language, cx);

        if language.is_some() {
            // Look up global snippets as well.
            requested_snippets.extend(self.lookup_snippets::<true>(&None, cx));
        }
        requested_snippets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui;
    use gpui::TestAppContext;
    use indoc::indoc;

    const SNIPPETS: &str = "/snippets";

    async fn add(fs: &FakeFs, name: &str, text: &str) -> Result<AddedSnippet> {
        add_snippet(fs, Path::new(SNIPPETS), "kusto", name, name, text).await
    }

    /// The snippets a file defines, as Zed reads them, with the text each one inserts.
    async fn snippets_of(fs: &FakeFs) -> Vec<(String, String)> {
        let path = Path::new(SNIPPETS).join("kusto.json");
        let file: VsSnippetsFile =
            serde_json_lenient::from_str(&fs.load(&path).await.unwrap()).unwrap();
        let mut snippets = file_to_snippets(file, &path)
            .map(|snippet| {
                let snippet = snippet.unwrap();
                let inserted = snippet::Snippet::parse(&snippet.body).unwrap().text;
                (snippet.name.clone(), inserted)
            })
            .collect::<Vec<_>>();
        snippets.sort();
        snippets
    }

    fn with_top(contents: &str) -> String {
        with_snippet(contents, "top", "top", "T | top 5 by N")
            .unwrap()
            .unwrap()
    }

    #[test]
    fn test_new_snippet_goes_after_the_last_one_so_comments_stay_with_theirs() {
        let existing = indoc! {r#"
            {
                // Counts the rows of a table.
                "count": {
                    "prefix": "count",
                    "body": ["T | count"],
                },
            }
        "#};
        assert_eq!(
            with_top(existing),
            indoc! {r#"
            {
                // Counts the rows of a table.
                "count": {
                    "prefix": "count",
                    "body": ["T | count"],
                },
                "top": {
                    "prefix": "top",
                    "body": [
                        "T | top 5 by N"
                    ]
                }
            }
        "#}
        );
    }

    #[test]
    fn test_new_snippet_goes_below_a_comment_on_the_line_of_the_last_one() {
        let existing = indoc! {r#"
            {
                "count": { "prefix": "count", "body": "T | count" } // the first
                // Snippets for joins go here.
            }
        "#};
        assert_eq!(
            with_top(existing),
            indoc! {r#"
            {
                "count": { "prefix": "count", "body": "T | count" }, // the first
                "top": {
                    "prefix": "top",
                    "body": [
                        "T | top 5 by N"
                    ]
                }
                // Snippets for joins go here.
            }
        "#}
        );
    }

    #[test]
    fn test_new_snippet_in_a_file_with_nothing_but_comments_in_its_object() {
        // With no snippet to take the indentation from, the new one is indented as JSON is by default.
        let existing = "{\n    // \"Example\": { \"prefix\": \"x\", \"body\": \"}\" }\n}\n";
        assert_eq!(
            with_top(existing),
            concat!(
                "{\n",
                "  \"top\": {\n",
                "    \"prefix\": \"top\",\n",
                "    \"body\": [\n",
                "      \"T | top 5 by N\"\n",
                "    ]\n",
                "  }\n",
                "    // \"Example\": { \"prefix\": \"x\", \"body\": \"}\" }\n",
                "}\n",
            )
        );
        assert_eq!(
            with_top(""),
            concat!(
                "{\n",
                "  \"top\": {\n",
                "    \"prefix\": \"top\",\n",
                "    \"body\": [\n",
                "      \"T | top 5 by N\"\n",
                "    ]\n",
                "  }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn test_braces_quotes_and_slashes_in_strings_and_comments_are_not_structure() {
        let existing = indoc! {r#"
            {
                /* a "quote and a } brace */
                "url": { "prefix": "url", "body": "http://x/ \" } {" }
            }
        "#};
        let updated = with_top(existing);
        let file: serde_json::Value = serde_json_lenient::from_str(&updated).unwrap();
        assert_eq!(
            file["url"]["body"], "http://x/ \" } {",
            "the existing snippet is kept as it was"
        );
        assert_eq!(file["top"]["body"], serde_json::json!(["T | top 5 by N"]));
        assert!(
            updated.contains("/* a \"quote and a } brace */"),
            "{updated}"
        );
    }

    #[test]
    fn test_a_file_that_is_not_an_object_is_an_error() {
        for contents in ["[]", "42", "{ \"a\": "] {
            assert!(
                with_snippet(contents, "top", "top", "T").is_err(),
                "{contents}"
            );
        }
    }

    #[gpui::test]
    async fn test_add_snippet_creates_the_file(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        let query = "StormEvents\n| where State == \"TEXAS\"\n| take 10";

        let added = add(&fs, "texas", query).await.unwrap();

        assert_eq!(
            added,
            AddedSnippet::Added(Path::new(SNIPPETS).join("kusto.json"))
        );
        assert_eq!(
            snippets_of(&fs).await,
            vec![("texas".to_string(), query.to_string())]
        );
    }

    #[gpui::test]
    async fn test_add_snippet_inserts_the_query_as_written(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        let query = "T\n| join kind=inner (U) on $left.Id == $right.Id\n| where P matches regex @\"C:\\\\${1}\" }";

        add(&fs, "joined", query).await.unwrap();

        assert_eq!(
            snippets_of(&fs).await,
            vec![("joined".to_string(), query.to_string())]
        );
    }

    #[gpui::test]
    async fn test_add_snippet_keeps_comments_and_other_snippets(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            SNIPPETS,
            serde_json::json!({
                "kusto.json": indoc! {r#"
                    {
                        // Counts the rows of a table.
                        "count": {
                            "prefix": "count",
                            "body": ["T | count"],
                        },
                    }
                "#},
            }),
        )
        .await;

        add(&fs, "top", "T | top 5 by N").await.unwrap();

        let contents = fs
            .load(&Path::new(SNIPPETS).join("kusto.json"))
            .await
            .unwrap();
        assert!(
            contents.contains("// Counts the rows of a table."),
            "{contents}"
        );
        assert_eq!(
            snippets_of(&fs).await,
            vec![
                ("count".to_string(), "T | count".to_string()),
                ("top".to_string(), "T | top 5 by N".to_string()),
            ]
        );
    }

    #[gpui::test]
    async fn test_add_snippet_leaves_a_snippet_of_the_same_name(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        add(&fs, "count", "T | count").await.unwrap();
        let before = fs
            .load(&Path::new(SNIPPETS).join("kusto.json"))
            .await
            .unwrap();

        let added = add(&fs, "count", "U | count").await.unwrap();

        assert_eq!(added, AddedSnippet::NameTaken);
        let after = fs
            .load(&Path::new(SNIPPETS).join("kusto.json"))
            .await
            .unwrap();
        assert_eq!(after, before);
    }

    #[gpui::test]
    async fn test_add_snippet_does_not_overwrite_a_file_it_cannot_read(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        let broken = "{ \"count\": { \"body\": ";
        fs.insert_tree(SNIPPETS, serde_json::json!({ "kusto.json": broken }))
            .await;

        let error = add(&fs, "top", "T | top 5 by N").await.unwrap_err();

        assert!(format!("{error:#}").contains("not valid JSON"), "{error:#}");
        let contents = fs
            .load(&Path::new(SNIPPETS).join("kusto.json"))
            .await
            .unwrap();
        assert_eq!(contents, broken);
    }

    #[gpui::test]
    async fn test_snippet_provider_picks_up_an_added_snippet(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.create_dir(Path::new(SNIPPETS)).await.unwrap();
        let provider = cx.update(|cx| {
            SnippetRegistry::init_global(cx);
            SnippetProvider::new(fs.clone(), BTreeSet::from([PathBuf::from(SNIPPETS)]), cx)
        });
        cx.run_until_parked();
        let before = cx.update(|cx| provider.read(cx).snippets_for(Some("kusto".to_owned()), cx));
        assert!(before.is_empty());

        add(&fs, "top", "T | top 5 by N").await.unwrap();
        cx.run_until_parked();

        let found = cx.update(|cx| provider.read(cx).snippets_for(Some("kusto".to_owned()), cx));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].prefix, vec!["top".to_string()]);
        assert_eq!(
            snippet::Snippet::parse(&found[0].body).unwrap().text,
            "T | top 5 by N"
        );
    }

    #[gpui::test]
    fn test_lookup_snippets_dup_registry_snippets(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        cx.update(|cx| {
            SnippetRegistry::init_global(cx);
            SnippetRegistry::global(cx)
                .register_snippets(
                    "ruby".as_ref(),
                    indoc! {r#"
                    {
                      "Log to console": {
                        "prefix": "log",
                        "body": ["console.info(\"Hello, ${1:World}!\")", "$0"],
                        "description": "Logs to console"
                      }
                    }
            "#},
                )
                .unwrap();
            let provider = SnippetProvider::new(fs.clone(), Default::default(), cx);
            cx.update_entity(&provider, |provider, cx| {
                assert_eq!(1, provider.snippets_for(Some("ruby".to_owned()), cx).len());
            });
        });
    }
}
