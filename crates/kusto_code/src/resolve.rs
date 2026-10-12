//! Turns the keys of an event into places in the local repositories.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::{join_all, join4};
use parking_lot::Mutex;

use crate::keys::{CodeKeys, SymbolPath};
use crate::search::{GitSearch, GrepQuery, Hit, Repository};
use crate::source::{
    base_types, enclosing_method, enclosing_type, is_comment, is_marker_attribute,
    method_declaration_name, namespace_of, read_lines, string_constant_name, type_declaration_name,
    type_declared_below,
};

/// How far below `[CodeMarkerName]` the class it marks may be.
const ATTRIBUTE_REACH: usize = 8;
/// A constant that names a marker class that names the marker is two steps; more is not followed.
const MAX_DEFINITION_DEPTH: usize = 2;
const MAX_USES: usize = 8;
const MAX_DEFINITIONS: usize = 3;
const MAX_DECLARATIONS: usize = 3;
/// The lines shown for each run of a message.
const MAX_LOG_LINES: usize = 3;
/// A run of text that more lines than this contain is too common to say where a message came
/// from.
const MAX_COMMON_LOG_LINES: usize = 16;
/// How long a result is kept. A checkout moves under a running editor (a pull, a branch), and a
/// lookup is cheap enough to repeat.
const CACHE_LIFETIME: Duration = Duration::from_secs(300);
/// How many base classes up a method is looked for.
const MAX_BASE_DEPTH: usize = 3;
/// Declarations whose namespace is read to rank them; a name this common is not worth more.
const MAX_RANKED_FILES: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyOrigin {
    Marker,
    Prefix,
    Frame,
    /// The text of the message.
    Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeRefKind {
    /// Where code uses the marker.
    MarkerUse,
    /// Where the marker is declared.
    MarkerDefinition,
    Method,
    /// The type was found but not the method, which may be inherited.
    Type,
    /// A line with the text of the message in a string, so that it may have logged it.
    LogLine,
    /// The line of a call stack frame, in the file the frame names.
    StackLine,
}

/// A place in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRef {
    pub origin: KeyOrigin,
    pub kind: CodeRefKind,
    pub repository: String,
    pub path: PathBuf,
    /// Relative to the repository, with `/` separators.
    pub relative_path: String,
    /// 1-based.
    pub line: usize,
    /// What was matched: a marker class or constant, or `Class.Method`.
    pub symbol: String,
    /// For a use of a marker, the method it is in.
    pub enclosing: Option<String>,
    /// The text of the line, without its indentation.
    pub text: String,
    /// How many places are equally good: more than one when several lines of code hold the same
    /// text of a message.
    pub matches: usize,
}

struct Expiring<T> {
    stored: Instant,
    value: T,
}

impl<T> Expiring<T> {
    fn new(value: T) -> Self {
        Self {
            stored: Instant::now(),
            value,
        }
    }
}

/// The tracked files of the repositories by lower-case file name, so that the file a call stack
/// frame names is found without a search.
struct FileIndex {
    by_name: HashMap<String, Vec<(usize, String)>>,
}

#[derive(Debug, Clone)]
struct Located {
    repository_index: usize,
    hit: Hit,
}

/// Where a marker is declared: a class marked `[CodeMarkerName]` or a string constant.
#[derive(Debug, Clone)]
struct Definition {
    name: String,
    outer: Option<String>,
    location: Located,
}

enum Marked {
    Definition(Definition),
    Use,
}

pub struct CodeResolver {
    search: GitSearch,
    repositories: Vec<Repository>,
    marker_literals: Mutex<HashMap<String, Expiring<Vec<Located>>>>,
    cache: Mutex<HashMap<String, Expiring<Vec<CodeRef>>>>,
    files: Mutex<Option<Expiring<Arc<FileIndex>>>>,
    cache_lifetime: Duration,
}

impl CodeResolver {
    pub fn new(search: GitSearch, repositories: Vec<Repository>) -> Self {
        Self {
            search,
            repositories,
            marker_literals: Mutex::default(),
            cache: Mutex::default(),
            files: Mutex::default(),
            cache_lifetime: CACHE_LIFETIME,
        }
    }

    /// How long what was found is reused before it is looked up again.
    pub fn with_cache_lifetime(mut self, lifetime: Duration) -> Self {
        self.cache_lifetime = lifetime;
        self
    }

    pub fn repositories(&self) -> &[Repository] {
        &self.repositories
    }

    fn cached(&self, key: &str) -> Option<Vec<CodeRef>> {
        self.cache
            .lock()
            .get(key)
            .filter(|entry| entry.stored.elapsed() < self.cache_lifetime)
            .map(|entry| entry.value.clone())
    }

    fn remember(&self, key: String, found: &[CodeRef]) {
        self.cache.lock().insert(key, Expiring::new(found.to_vec()));
    }

    /// Forget what was found, for when the repositories have changed.
    pub fn clear_cache(&self) {
        self.marker_literals.lock().clear();
        self.cache.lock().clear();
        self.files.lock().take();
    }

    /// Searches the repositories at `indexes` together. One that cannot be searched is logged and
    /// has no hits.
    async fn grep_repositories(&self, indexes: &[usize], query: &GrepQuery) -> Vec<Located> {
        let searches = indexes
            .iter()
            .filter_map(|index| Some((*index, self.repositories.get(*index)?)))
            .map(|(repository_index, repository)| async move {
                match self.search.grep(repository, query).await {
                    Ok(hits) => hits
                        .into_iter()
                        .map(|hit| Located {
                            repository_index,
                            hit,
                        })
                        .collect(),
                    Err(error) => {
                        log::warn!("searching {} failed: {error:#}", repository.name);
                        Vec::new()
                    }
                }
            });
        join_all(searches).await.into_iter().flatten().collect()
    }

    /// Looks up the string literals of all of `markers` with one search per repository, so that
    /// resolving each of them afterwards finds them ready. A trace has about sixty markers.
    pub async fn prefetch_markers(&self, markers: &[String]) {
        let mut seen = HashSet::new();
        let missing: Vec<&String> = {
            let known = self.marker_literals.lock();
            markers
                .iter()
                .filter(|marker| {
                    let fresh = known
                        .get(*marker)
                        .is_some_and(|entry| entry.stored.elapsed() < self.cache_lifetime);
                    !fresh && seen.insert(*marker)
                })
                .collect()
        };
        if missing.is_empty() {
            return;
        }

        let quoted: Vec<String> = missing
            .iter()
            .map(|marker| format!("\"{marker}\""))
            .collect();
        let everywhere: Vec<usize> = (0..self.repositories.len()).collect();
        let hits = self
            .grep_repositories(&everywhere, &GrepQuery::fixed(quoted.clone()))
            .await;

        let mut found: HashMap<String, Vec<Located>> = missing
            .iter()
            .map(|marker| ((*marker).clone(), Vec::new()))
            .collect();
        for location in hits {
            for (marker, needle) in missing.iter().zip(&quoted) {
                if location.hit.text.contains(needle.as_str())
                    && let Some(locations) = found.get_mut(marker.as_str())
                {
                    locations.push(location.clone());
                }
            }
        }
        self.marker_literals.lock().extend(
            found
                .into_iter()
                .map(|(marker, locations)| (marker, Expiring::new(locations))),
        );
    }

    pub async fn resolve(&self, keys: &CodeKeys) -> Vec<CodeRef> {
        let process = keys.process.as_deref();
        let marker = async {
            let Some(marker) = &keys.marker else {
                return Vec::new();
            };
            let found = self.resolve_marker(marker).await;
            if !found.is_empty() {
                return found;
            }
            // A marker named after a method, such as `Contoso.Orders.Checkout.Run`, may have
            // no declaration of its own.
            match SymbolPath::parse(marker) {
                Some(symbol) => {
                    self.resolve_symbol(&symbol, process, KeyOrigin::Marker)
                        .await
                }
                None => Vec::new(),
            }
        };
        let prefix = async {
            match &keys.prefix {
                Some(symbol) => {
                    self.resolve_symbol(symbol, process, KeyOrigin::Prefix)
                        .await
                }
                None => Vec::new(),
            }
        };
        let frames = join_all(
            keys.frames
                .iter()
                .map(|frame| self.resolve_symbol(frame, process, KeyOrigin::Frame)),
        );

        let message = self.resolve_message(&keys.literals, process);

        let (mut message, marker, prefix, frames) = join4(message, marker, prefix, frames).await;
        // The line that logged a message is likely to be in a file the other lookups found too.
        let known_files: HashSet<PathBuf> = marker
            .iter()
            .chain(&prefix)
            .chain(frames.iter().flatten())
            .map(|reference| reference.path.clone())
            .collect();
        message.sort_by_key(|reference| !known_files.contains(&reference.path));

        let mut seen = HashSet::new();
        message
            .into_iter()
            .chain(marker)
            .chain(prefix)
            .chain(frames.into_iter().flatten())
            .filter(|reference| {
                seen.insert((reference.path.clone(), reference.line, reference.kind))
            })
            .collect()
    }

    /// The lines that may have logged the message. For each of the two longest runs of it that
    /// code could have written (`CodeKeys::literals`), the lines that hold the most of the run
    /// in a string, unless that is so many lines that it says nothing.
    async fn resolve_message(
        &self,
        literals: &[Vec<String>],
        process: Option<&str>,
    ) -> Vec<CodeRef> {
        if literals.is_empty() {
            return Vec::new();
        }
        let runs: Vec<&str> = literals
            .iter()
            .filter_map(|pieces| pieces.first().map(String::as_str))
            .collect();
        let cache_key = format!(
            "message:{}:{}",
            runs.join("\u{1f}"),
            process.unwrap_or_default()
        );
        if let Some(cached) = self.cached(&cache_key) {
            return cached;
        }

        let mut patterns: Vec<String> = Vec::new();
        for piece in literals.iter().flatten() {
            if !patterns.contains(piece) {
                patterns.push(piece.clone());
            }
        }
        let query = GrepQuery::fixed(patterns);
        let mut hits: Vec<Located> = Vec::new();
        for stage in self.search_stages(process) {
            hits.extend(
                self.grep_repositories(&stage, &query)
                    .await
                    .into_iter()
                    .filter(|location| {
                        location.hit.text.contains('"') && !is_comment(&location.hit.text)
                    }),
            );
            // A line that holds a whole run of the message cannot be bettered by another
            // repository; a line with only part of it can.
            let holds_a_whole_run = hits
                .iter()
                .any(|location| runs.iter().any(|run| location.hit.text.contains(run)));
            if holds_a_whole_run {
                break;
            }
        }

        let process_words = words(process.unwrap_or_default());
        let mut taken = HashSet::new();
        let mut found = Vec::new();
        for pieces in literals {
            // The pieces are longest first, so the first one a line holds is the most of the run
            // that it holds.
            let mut matched: Vec<(usize, &str, &Located)> = hits
                .iter()
                .filter_map(|location| {
                    let piece = pieces
                        .iter()
                        .find(|piece| location.hit.text.contains(piece.as_str()))?;
                    Some((piece.chars().count(), piece.as_str(), location))
                })
                .collect();
            let Some(best) = matched.iter().map(|(length, ..)| *length).max() else {
                continue;
            };
            matched.retain(|(length, ..)| *length == best);
            let equally_good = matched.len();
            if equally_good > MAX_COMMON_LOG_LINES {
                continue;
            }
            matched.sort_by_key(|(_, _, location)| {
                let shared_words = self
                    .repositories
                    .get(location.repository_index)
                    .map_or(0, |repository| {
                        words(&repository.name).intersection(&process_words).count()
                    });
                (
                    Reverse(shared_words),
                    is_test_path(&location.hit.path),
                    location.hit.path.clone(),
                    location.hit.line,
                )
            });
            for (_, piece, location) in matched.into_iter().take(MAX_LOG_LINES) {
                if !taken.insert((
                    location.repository_index,
                    location.hit.path.clone(),
                    location.hit.line,
                )) {
                    continue;
                }
                let enclosing = self.read(location).and_then(|lines| {
                    location
                        .hit
                        .line
                        .checked_sub(1)
                        .and_then(|index| enclosing_method(&lines, index))
                });
                found.extend(
                    self.reference(
                        KeyOrigin::Message,
                        CodeRefKind::LogLine,
                        location,
                        location.hit.line,
                        shortened(piece),
                        enclosing,
                    )
                    .map(|reference| CodeRef {
                        matches: equally_good,
                        ..reference
                    }),
                );
            }
        }
        self.remember(cache_key, &found);
        found
    }

    async fn file_index(&self) -> Arc<FileIndex> {
        let known = self
            .files
            .lock()
            .as_ref()
            .filter(|entry| entry.stored.elapsed() < self.cache_lifetime)
            .map(|entry| entry.value.clone());
        if let Some(index) = known {
            return index;
        }
        let listings = join_all(self.repositories.iter().enumerate().map(
            |(repository_index, repository)| async move {
                match self.search.list_files(repository).await {
                    Ok(files) => (repository_index, files),
                    Err(error) => {
                        log::warn!("listing the files of {} failed: {error:#}", repository.name);
                        (repository_index, Vec::new())
                    }
                }
            },
        ))
        .await;
        let mut by_name: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        for (repository_index, files) in listings {
            for path in files {
                let name = path.rsplit('/').next().unwrap_or(&path).to_lowercase();
                by_name
                    .entry(name)
                    .or_default()
                    .push((repository_index, path));
            }
        }
        let index = Arc::new(FileIndex { by_name });
        *self.files.lock() = Some(Expiring::new(index.clone()));
        index
    }

    /// Where the file a call stack frame names is. `file_name` is as the stack gives it (a bare
    /// name, or a path from the machine that built the code, of which only the name is used),
    /// `line` is where the frame was, and `symbol` is its method. Files of that name are ordered
    /// by whether they declare the frame's class, then as declarations are (the process, the
    /// namespace). The line is the frame's, unless the checkout has moved on and that line is
    /// not in the frame's method, when it is the method's declaration.
    pub async fn find_source_file(
        &self,
        file_name: &str,
        line: usize,
        symbol: Option<&SymbolPath>,
        process: Option<&str>,
    ) -> Vec<CodeRef> {
        let name = file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(file_name)
            .trim()
            .to_lowercase();
        let index = self.file_index().await;
        let Some(files) = index.by_name.get(&name) else {
            return Vec::new();
        };
        let candidates: Vec<Located> = files
            .iter()
            .map(|(repository_index, path)| Located {
                repository_index: *repository_index,
                hit: Hit {
                    path: path.clone(),
                    line,
                    text: String::new(),
                },
            })
            .collect();
        let nameless = SymbolPath {
            namespace: Vec::new(),
            class_name: String::new(),
            method_name: String::new(),
        };
        let ranked = self.rank_declarations(candidates, symbol.unwrap_or(&nameless), process);

        let mut found: Vec<(bool, Located, Vec<String>)> = ranked
            .into_iter()
            .take(MAX_RANKED_FILES)
            .map(|location| {
                let lines = self.read(&location).unwrap_or_default();
                let declares_the_class = symbol.is_some_and(|symbol| {
                    lines.iter().any(|text| {
                        type_declaration_name(text).as_deref() == Some(symbol.class_name.as_str())
                    })
                });
                (declares_the_class, location, lines)
            })
            .collect();
        // Stable, so the order of the ranking stays among files that all declare the class or none do.
        found.sort_by_key(|(declares_the_class, ..)| !declares_the_class);

        found
            .into_iter()
            .take(MAX_DECLARATIONS)
            .filter_map(|(_, location, lines)| {
                let method = symbol.map(|symbol| symbol.method_name.as_str());
                let declaration_line = method.and_then(|method| {
                    lines
                        .iter()
                        .position(|text| method_declaration_name(text).as_deref() == Some(method))
                        .map(|index| index + 1)
                });
                let enclosing = line
                    .checked_sub(1)
                    .and_then(|index| enclosing_method(&lines, index));
                // The checkout is not the build the trace came from, so the line may have moved
                // out of the method. A line in no method at all counts, if the method is declared.
                let in_the_method = match (method, &enclosing) {
                    (Some(method), Some(found)) => method == found,
                    (Some(_), None) => declaration_line.is_none(),
                    (None, _) => true,
                };
                let shown_line = match declaration_line {
                    Some(declaration) if !(in_the_method && (1..=lines.len()).contains(&line)) => {
                        declaration
                    }
                    _ => line.clamp(1, lines.len().max(1)),
                };
                let located = Located {
                    hit: Hit {
                        line: shown_line,
                        text: lines
                            .get(shown_line - 1)
                            .map(|text| text.trim().to_string())
                            .unwrap_or_default(),
                        ..location.hit.clone()
                    },
                    ..location
                };
                let label = match symbol {
                    Some(symbol) => symbol.display(),
                    None => file_name.to_string(),
                };
                let enclosing = shown_line
                    .checked_sub(1)
                    .and_then(|index| enclosing_method(&lines, index));
                self.reference(
                    KeyOrigin::Frame,
                    CodeRefKind::StackLine,
                    &located,
                    shown_line,
                    label,
                    enclosing,
                )
            })
            .collect()
    }

    fn reference(
        &self,
        origin: KeyOrigin,
        kind: CodeRefKind,
        location: &Located,
        line: usize,
        symbol: String,
        enclosing: Option<String>,
    ) -> Option<CodeRef> {
        let repository = self.repositories.get(location.repository_index)?;
        Some(CodeRef {
            origin,
            kind,
            repository: repository.name.clone(),
            path: repository.root.join(&location.hit.path),
            relative_path: location.hit.path.clone(),
            line,
            symbol,
            enclosing,
            text: location.hit.text.trim().to_string(),
            matches: 1,
        })
    }

    fn read(&self, location: &Located) -> Option<Vec<String>> {
        let repository = self.repositories.get(location.repository_index)?;
        match read_lines(&repository.root.join(&location.hit.path)) {
            Ok(lines) => Some(lines),
            Err(error) => {
                log::warn!("{error:#}");
                None
            }
        }
    }

    async fn resolve_marker(&self, marker: &str) -> Vec<CodeRef> {
        let cache_key = format!("marker:{marker}");
        if let Some(cached) = self.cached(&cache_key) {
            return cached;
        }

        self.prefetch_markers(&[marker.to_string()]).await;
        let literals = self
            .marker_literals
            .lock()
            .get(marker)
            .map(|entry| entry.value.clone())
            .unwrap_or_default();

        let mut definitions = VecDeque::new();
        let mut uses: Vec<CodeRef> = Vec::new();
        let mut declared: Vec<CodeRef> = Vec::new();
        for location in literals {
            if is_comment(&location.hit.text) {
                continue;
            }
            let Some(lines) = self.read(&location) else {
                continue;
            };
            match mark(&location, &lines) {
                Marked::Definition(definition) => {
                    let line = definition.location.hit.line;
                    declared.extend(self.reference(
                        KeyOrigin::Marker,
                        CodeRefKind::MarkerDefinition,
                        &definition.location,
                        line,
                        definition.name.clone(),
                        None,
                    ));
                    definitions.push_back((definition, 1));
                }
                Marked::Use => {
                    let enclosing = location
                        .hit
                        .line
                        .checked_sub(1)
                        .and_then(|index| enclosing_method(&lines, index));
                    uses.extend(self.reference(
                        KeyOrigin::Marker,
                        CodeRefKind::MarkerUse,
                        &location,
                        location.hit.line,
                        marker.to_string(),
                        enclosing,
                    ));
                }
            }
        }

        while let Some((definition, depth)) = definitions.pop_front() {
            // Uses in the repository of the declaration are searched first, and the others only if
            // there are none; a marker declared in a shared library is used elsewhere.
            let home = definition.location.repository_index;
            let elsewhere: Vec<usize> = (0..self.repositories.len())
                .filter(|index| *index != home)
                .collect();
            let query = GrepQuery::word(&definition.name);
            let mut candidates = Vec::new();
            let mut wrapped_a_class = false;
            for stage in [vec![home], elsewhere] {
                for location in self.grep_repositories(&stage, &query).await {
                    if is_declaration_of(&location, &definition) {
                        continue;
                    }
                    if is_marker_attribute(&location.hit.text) {
                        // A constant that names a marker class: the class is what code uses.
                        if depth < MAX_DEFINITION_DEPTH
                            && let Some(lines) = self.read(&location)
                            && let Marked::Definition(wrapped) = mark(&location, &lines)
                        {
                            definitions.push_back((wrapped, depth + 1));
                            wrapped_a_class = true;
                        }
                        continue;
                    }
                    candidates.push(location);
                }
                if !candidates.is_empty() || wrapped_a_class {
                    break;
                }
            }
            uses.extend(self.rank_uses(&definition, candidates));
        }

        declared.truncate(MAX_DEFINITIONS);
        let mut found = uses;
        found.extend(declared);
        self.remember(cache_key, &found);
        found
    }

    /// The use sites of a marker, best first. Code reaches a marker as `Outer.Name`, so lines
    /// that say so come before others; then the sites nearest the declaration, and tests last.
    fn rank_uses(&self, definition: &Definition, mut candidates: Vec<Located>) -> Vec<CodeRef> {
        // A short name is rarely unique, and another repository may well have its own class by
        // the same name. A marker declared in a shared library has no uses of its own repository.
        let home = definition.location.repository_index;
        if candidates
            .iter()
            .any(|candidate| candidate.repository_index == home)
        {
            candidates.retain(|candidate| candidate.repository_index == home);
        }
        if let Some(outer) = &definition.outer {
            let qualified = format!("{outer}.{}", definition.name);
            if candidates
                .iter()
                .any(|candidate| candidate.hit.text.contains(&qualified))
            {
                candidates.retain(|candidate| candidate.hit.text.contains(&qualified));
            }
        }
        candidates.sort_by_key(|candidate| {
            (
                Reverse(candidate.repository_index == definition.location.repository_index),
                Reverse(
                    if candidate.repository_index == definition.location.repository_index {
                        shared_directories(&candidate.hit.path, &definition.location.hit.path)
                    } else {
                        0
                    },
                ),
                is_test_path(&candidate.hit.path),
                candidate.hit.path.clone(),
                candidate.hit.line,
            )
        });
        candidates.truncate(MAX_USES);

        let symbol = match &definition.outer {
            Some(outer) => format!("{outer}.{}", definition.name),
            None => definition.name.clone(),
        };
        candidates
            .iter()
            .filter_map(|candidate| {
                let enclosing = self.read(candidate).and_then(|lines| {
                    candidate
                        .hit
                        .line
                        .checked_sub(1)
                        .and_then(|index| enclosing_method(&lines, index))
                });
                self.reference(
                    KeyOrigin::Marker,
                    CodeRefKind::MarkerUse,
                    candidate,
                    candidate.hit.line,
                    symbol.clone(),
                    enclosing,
                )
            })
            .collect()
    }

    async fn resolve_symbol(
        &self,
        symbol: &SymbolPath,
        process: Option<&str>,
        origin: KeyOrigin,
    ) -> Vec<CodeRef> {
        let cache_key = format!(
            "symbol:{}:{}",
            symbol.full_name(),
            process.unwrap_or_default()
        );
        let found = match self.cached(&cache_key) {
            Some(found) => found,
            None => {
                let found = self.find_symbol(symbol, process).await;
                self.remember(cache_key, &found);
                found
            }
        };
        found
            .into_iter()
            .map(|reference| CodeRef {
                origin,
                ..reference
            })
            .collect()
    }

    /// The method, or failing that the type, that `symbol` names. The segment before the method
    /// is the type; if none is declared by that name, the one before it is tried.
    async fn find_symbol(&self, symbol: &SymbolPath, process: Option<&str>) -> Vec<CodeRef> {
        let outer_class = symbol.namespace.last().filter(|segment| {
            segment
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_uppercase())
        });
        for class_name in std::iter::once(&symbol.class_name).chain(outer_class) {
            if let Some(found) = self.find_in_hierarchy(class_name, symbol, process).await {
                return found;
            }
        }
        Vec::new()
    }

    /// The declaration of `symbol`'s method in the type or, if the type does not declare it, in
    /// its base classes. When no method is found, the type itself. `None` if there is no such
    /// type.
    async fn find_in_hierarchy(
        &self,
        class_name: &str,
        symbol: &SymbolPath,
        process: Option<&str>,
    ) -> Option<Vec<CodeRef>> {
        let mut queue = VecDeque::from([(class_name.to_string(), 0)]);
        let mut visited = HashSet::new();
        let mut type_only: Option<Vec<CodeRef>> = None;
        while let Some((name, depth)) = queue.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let declarations = self.type_declarations(&name, process).await;
            if declarations.is_empty() {
                continue;
            }

            let ranked = self.rank_declarations(declarations, symbol, process);
            let methods = self.method_declarations(&ranked, &symbol.method_name).await;
            if !methods.is_empty() {
                let label = format!("{name}.{}", symbol.method_name);
                return Some(
                    methods
                        .iter()
                        .take(MAX_DECLARATIONS)
                        .filter_map(|location| {
                            self.reference(
                                KeyOrigin::Prefix,
                                CodeRefKind::Method,
                                location,
                                location.hit.line,
                                label.clone(),
                                None,
                            )
                        })
                        .collect(),
                );
            }

            if type_only.is_none() {
                type_only = Some(
                    ranked
                        .iter()
                        .take(MAX_DECLARATIONS)
                        .filter_map(|location| {
                            self.reference(
                                KeyOrigin::Prefix,
                                CodeRefKind::Type,
                                location,
                                location.hit.line,
                                name.clone(),
                                None,
                            )
                        })
                        .collect(),
                );
            }
            if depth < MAX_BASE_DEPTH {
                for declaration in ranked.iter().take(MAX_DECLARATIONS) {
                    let Some(lines) = self.read(declaration) else {
                        continue;
                    };
                    let Some(index) = declaration.hit.line.checked_sub(1) else {
                        continue;
                    };
                    for base in base_types(&lines, index) {
                        queue.push_back((base, depth + 1));
                    }
                }
            }
        }
        type_only
    }

    /// Where a class, struct, record or interface called `name` is declared: in the repositories
    /// named like the process, or failing that in the others.
    async fn type_declarations(&self, name: &str, process: Option<&str>) -> Vec<Located> {
        let query = GrepQuery::words(
            ["class", "struct", "record", "interface"]
                .map(|keyword| format!("{keyword} {name}"))
                .to_vec(),
        );
        for stage in self.search_stages(process) {
            let declarations: Vec<Located> = self
                .grep_repositories(&stage, &query)
                .await
                .into_iter()
                .filter(|location| {
                    !is_comment(&location.hit.text)
                        && type_declaration_name(&location.hit.text).as_deref() == Some(name)
                })
                .collect();
            if !declarations.is_empty() {
                return declarations;
            }
        }
        Vec::new()
    }

    /// The method declarations of `method_name` in the files that declare the type, in the order
    /// of `declarations`.
    async fn method_declarations(
        &self,
        declarations: &[Located],
        method_name: &str,
    ) -> Vec<Located> {
        let mut files_by_repository: HashMap<usize, Vec<String>> = HashMap::new();
        for declaration in declarations {
            let files = files_by_repository
                .entry(declaration.repository_index)
                .or_default();
            if !files.contains(&declaration.hit.path) {
                files.push(declaration.hit.path.clone());
            }
        }

        let searches = files_by_repository.iter().map(|(index, files)| async move {
            let Some(repository) = self.repositories.get(*index) else {
                return Vec::new();
            };
            let query = GrepQuery::word(method_name).in_files(files.clone());
            match self.search.grep(repository, &query).await {
                Ok(hits) => hits
                    .into_iter()
                    .filter(|hit| {
                        method_declaration_name(&hit.text).as_deref() == Some(method_name)
                    })
                    .map(|hit| Located {
                        repository_index: *index,
                        hit,
                    })
                    .collect(),
                Err(error) => {
                    log::warn!("searching {} failed: {error:#}", repository.name);
                    Vec::new()
                }
            }
        });
        let mut found: Vec<Located> = join_all(searches).await.into_iter().flatten().collect();
        let order: HashMap<(usize, &str), usize> = declarations
            .iter()
            .enumerate()
            .map(|(position, declaration)| {
                (
                    (declaration.repository_index, declaration.hit.path.as_str()),
                    position,
                )
            })
            .collect();
        found.sort_by_key(|location| {
            (
                order
                    .get(&(location.repository_index, location.hit.path.as_str()))
                    .copied()
                    .unwrap_or(usize::MAX),
                location.hit.line,
            )
        });
        found
    }

    /// The repositories to search for a symbol, in turn: those named like the process first,
    /// then the rest. A type is nearly always in the repository of the process that logs it.
    fn search_stages(&self, process: Option<&str>) -> Vec<Vec<usize>> {
        let process_words = words(process.unwrap_or_default());
        let (named_like_process, others): (Vec<usize>, Vec<usize>) = (0..self.repositories.len())
            .partition(|index| {
                self.repositories
                    .get(*index)
                    .is_some_and(|repository| !words(&repository.name).is_disjoint(&process_words))
            });
        [named_like_process, others]
            .into_iter()
            .filter(|stage| !stage.is_empty())
            .collect()
    }

    /// Orders declarations of one type name: repositories named like the process first, then
    /// files whose namespace starts like the symbol or the process. It orders; nothing is dropped.
    fn rank_declarations(
        &self,
        declarations: Vec<Located>,
        symbol: &SymbolPath,
        process: Option<&str>,
    ) -> Vec<Located> {
        let process_words = words(process.unwrap_or_default());
        let process_segments: Vec<&str> = process.unwrap_or_default().split('.').collect();
        let mut scored: Vec<((Reverse<usize>, Reverse<usize>), Located)> = declarations
            .into_iter()
            .enumerate()
            .map(|(position, location)| {
                let repository_score = self
                    .repositories
                    .get(location.repository_index)
                    .map_or(0, |repository| {
                        words(&repository.name).intersection(&process_words).count()
                    });
                let namespace_score = if position < MAX_RANKED_FILES {
                    self.read(&location)
                        .and_then(|lines| namespace_of(&lines))
                        .map_or(0, |namespace| {
                            let namespace: Vec<&str> = namespace.split('.').collect();
                            let symbol_segments: Vec<&str> =
                                symbol.namespace.iter().map(String::as_str).collect();
                            leading_overlap(&namespace, &symbol_segments)
                                .max(leading_overlap(&namespace, &process_segments))
                        })
                } else {
                    0
                };
                (
                    (Reverse(repository_score), Reverse(namespace_score)),
                    location,
                )
            })
            .collect();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            left_score
                .cmp(right_score)
                .then_with(|| left.hit.path.cmp(&right.hit.path))
                .then_with(|| left.hit.line.cmp(&right.hit.line))
        });
        scored.into_iter().map(|(_, location)| location).collect()
    }
}

/// Reads what a line that holds a marker's string says about it.
fn mark(location: &Located, lines: &[String]) -> Marked {
    let Some(index) = location.hit.line.checked_sub(1) else {
        return Marked::Use;
    };
    if is_marker_attribute(&location.hit.text) {
        let declared = type_declaration_name(&location.hit.text)
            .map(|name| (name, index))
            .or_else(|| type_declared_below(lines, index, ATTRIBUTE_REACH));
        if let Some((name, type_index)) = declared {
            return Marked::Definition(Definition {
                name,
                outer: enclosing_type(lines, type_index),
                location: Located {
                    repository_index: location.repository_index,
                    hit: Hit {
                        line: type_index + 1,
                        text: lines.get(type_index).cloned().unwrap_or_default(),
                        ..location.hit.clone()
                    },
                },
            });
        }
        return Marked::Use;
    }
    if let Some(name) = string_constant_name(&location.hit.text) {
        return Marked::Definition(Definition {
            name,
            outer: enclosing_type(lines, index),
            location: location.clone(),
        });
    }
    Marked::Use
}

/// Whether a hit on a marker's name is a declaration of it, which is not a use.
fn is_declaration_of(location: &Located, definition: &Definition) -> bool {
    let text = &location.hit.text;
    is_comment(text)
        || type_declaration_name(text).as_deref() == Some(definition.name.as_str())
        || string_constant_name(text).as_deref() == Some(definition.name.as_str())
}

/// The text of a run for a label, cut at 60 characters.
fn shortened(text: &str) -> String {
    const LIMIT: usize = 60;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let cut: String = text.chars().take(LIMIT).collect();
    format!("{}…", cut.trim_end())
}

fn shared_directories(left: &str, right: &str) -> usize {
    let directories = |path: &str| -> Vec<String> {
        path.rsplit_once('/')
            .map_or("", |(directory, _)| directory)
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    };
    directories(left)
        .iter()
        .zip(directories(right))
        .take_while(|(left, right)| *left == right)
        .count()
}

fn is_test_path(path: &str) -> bool {
    path.split('/')
        .any(|part| part.to_ascii_lowercase().contains("test"))
}

fn words(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.len() >= 3)
        .map(str::to_lowercase)
        .collect()
}

fn leading_overlap(left: &[&str], right: &[&str]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left.eq_ignore_ascii_case(right))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_directories_two_paths_share() {
        assert_eq!(shared_directories("a/b/c/X.cs", "a/b/d/Y.cs"), 2);
        assert_eq!(shared_directories("X.cs", "Y.cs"), 0);
        assert_eq!(shared_directories("a/X.cs", "b/Y.cs"), 0);
    }

    #[test]
    fn recognises_test_paths() {
        assert!(is_test_path("test/Foo/Bar.cs"));
        assert!(is_test_path("src/Foo.Tests/Bar.cs"));
        assert!(!is_test_path("src/Foo/Bar.cs"));
    }

    #[test]
    fn shares_words_between_a_repository_and_a_process() {
        let shared = words("workload-dms")
            .intersection(&words("Microsoft.Dms.Service.EntryPoint"))
            .count();
        assert_eq!(shared, 1);
        assert_eq!(
            leading_overlap(
                &["Microsoft", "Dms", "Metadata"],
                &["microsoft", "dms", "Other"]
            ),
            2
        );
    }
}
