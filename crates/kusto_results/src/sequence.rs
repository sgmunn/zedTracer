//! The sequence view: the calls between the actors of a trace, found from its activity tree.
//!
//! An actor is whatever the actor column names, usually a process. A call is a child
//! activity that runs in a different actor from its parent. Everything that stays inside one
//! actor is left out, which is what keeps a trace of thousands of activities readable.
//!
//! The projection is a model, not text. [`SequenceDiagram::to_mermaid`] is one way to show it.

use std::collections::HashMap;

use crate::activity::ActivityProjection;
use crate::result::Table;
use crate::trace_schema::TraceColumns;
use crate::typed::{TICKS_PER_SECOND, parse_datetime_ticks};
use crate::view::severity_level;

/// Calls nested deeper than this are counted but not drawn. Fifty nested activation bars are
/// already unreadable, and the limit keeps drawing from recursing as deep as a hostile trace.
const MAX_CALL_NESTING: usize = 50;

const UNKNOWN_ACTOR: &str = "(unknown)";
const TICKS_PER_MILLISECOND: f64 = 10_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct SequenceOptions {
    /// Levels below the framing root at which an activity becomes the *step* that groups the
    /// calls it caused. `None` draws no steps.
    pub step_depth: Option<usize>,
    /// Draw consecutive identical calls as one loop.
    pub collapse_repeats: bool,
    /// Markers that say what kind of request an activity is, not why it was made. A `*` at
    /// either end matches any text.
    pub wrapper_markers: Vec<String>,
    /// Trailing name segments that tell actors apart badly, such as `Service`.
    pub generic_actor_suffixes: Vec<String>,
    pub note_characters: usize,
    pub notes_per_call: usize,
}

impl Default for SequenceOptions {
    fn default() -> Self {
        Self {
            step_depth: Some(1),
            collapse_repeats: true,
            wrapper_markers: vec!["*IncomingRequest".into()],
            generic_actor_suffixes: vec!["EntryPoint".into(), "Service".into()],
            note_characters: 110,
            notes_per_call: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub name: String,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub actor: usize,
    pub marker: String,
    pub text: String,
    /// How many errors merged into this note.
    pub count: usize,
    /// The first source row it came from.
    pub source_row: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ErrorNotes {
    pub shown: Vec<Note>,
    /// Distinct notes left out because the scope already shows its limit.
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub caller: usize,
    pub callee: usize,
    pub label: String,
    pub callee_marker: Option<String>,
    pub caller_activity: usize,
    pub activities: Vec<usize>,
    pub offset_seconds: Option<f64>,
    pub duration_ms: Option<f64>,
    pub failed: bool,
    pub items: Vec<Item>,
    pub notes: ErrorNotes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Repeat {
    pub caller: usize,
    pub callee: usize,
    pub label: String,
    pub first_offset_seconds: Option<f64>,
    pub last_offset_seconds: Option<f64>,
    pub shortest_ms: Option<f64>,
    pub longest_ms: Option<f64>,
    pub calls: Vec<Call>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub label: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Call(Call),
    Repeat(Repeat),
    Step(Step),
}

/// A root activity and everything it contains. Its caller is not in the result.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub actor: usize,
    pub label: String,
    pub activity: usize,
    pub duration_ms: Option<f64>,
    pub failed: bool,
    pub items: Vec<Item>,
    pub notes: ErrorNotes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SequenceDiagram {
    /// Indexed by actor id.
    pub actors: Vec<Actor>,
    /// Actor ids in the order they are drawn, left to right.
    pub order: Vec<usize>,
    pub frames: Vec<Frame>,
    /// Roots that were not drawn because they hold no calls, and the activities in them.
    pub omitted_roots: usize,
    pub omitted_activities: usize,
    /// Calls nested too deeply to draw.
    pub omitted_calls: usize,
}

struct CallData {
    caller_activity: usize,
    caller_actor: usize,
    callee_actor: usize,
    kids: Vec<usize>,
    start: Option<i64>,
    end: Option<i64>,
}

struct Builder<'a> {
    table: &'a Table,
    projection: &'a ActivityProjection,
    columns: &'a TraceColumns,
    options: &'a SequenceOptions,
    actor_of: Vec<usize>,
    start: Vec<Option<i64>>,
    end: Vec<Option<i64>>,
    owner: Vec<Option<usize>>,
    root_of: Vec<usize>,
    subtree_error: Vec<bool>,
    /// Whether some activity below this one has an error: this activity's own error is then
    /// the same failure passing through, not where it started.
    descendant_error: Vec<bool>,
    error_row: Vec<Option<usize>>,
    /// Activities with an error of their own, by the call they run in.
    call_errors: Vec<Vec<usize>>,
    /// The same for activities that run in no call, by the root they belong to.
    root_errors: HashMap<usize, Vec<usize>>,
    calls: Vec<CallData>,
    nested_calls: Vec<Vec<usize>>,
    trace_start: Option<i64>,
    omitted_calls: usize,
}

/// Builds the sequence of a trace, or `None` when its columns do not say who called whom.
pub fn build_sequence(
    table: &Table,
    projection: &ActivityProjection,
    columns: &TraceColumns,
    options: &SequenceOptions,
) -> Option<SequenceDiagram> {
    let actor_column = columns.actor?;
    if columns.timestamp.is_none() || projection.activities.is_empty() {
        return None;
    }

    let mut actor_ids: HashMap<String, usize> = HashMap::new();
    let mut actor_names: Vec<String> = Vec::new();
    let mut actor_of = Vec::with_capacity(projection.activities.len());
    for activity in &projection.activities {
        let name = activity
            .event_rows
            .first()
            .and_then(|row| table.cell(*row, actor_column))
            .map(|cell| cell.display_text().trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| UNKNOWN_ACTOR.to_string());
        let id = *actor_ids.entry(name.clone()).or_insert_with(|| {
            actor_names.push(name);
            actor_names.len() - 1
        });
        actor_of.push(id);
    }

    let mut builder = Builder::new(table, projection, columns, options, actor_of);
    let frames = builder.frames();
    let omitted_calls = builder.omitted_calls;
    let (omitted_roots, omitted_activities) = builder.unframed(&frames);
    let order = actor_order(&frames, actor_names.len());
    let displays = display_names(&actor_names, &options.generic_actor_suffixes);
    let actors = actor_names
        .into_iter()
        .zip(displays)
        .map(|(name, display)| Actor { name, display })
        .collect();
    Some(SequenceDiagram {
        actors,
        order,
        frames,
        omitted_roots,
        omitted_activities,
        omitted_calls,
    })
}

impl<'a> Builder<'a> {
    fn new(
        table: &'a Table,
        projection: &'a ActivityProjection,
        columns: &'a TraceColumns,
        options: &'a SequenceOptions,
        actor_of: Vec<usize>,
    ) -> Self {
        let count = projection.activities.len();
        let mut builder = Self {
            table,
            projection,
            columns,
            options,
            actor_of,
            start: vec![None; count],
            end: vec![None; count],
            owner: vec![None; count],
            root_of: vec![0; count],
            subtree_error: vec![false; count],
            descendant_error: vec![false; count],
            error_row: vec![None; count],
            call_errors: Vec::new(),
            root_errors: HashMap::new(),
            calls: Vec::new(),
            nested_calls: Vec::new(),
            trace_start: None,
            omitted_calls: 0,
        };
        builder.read_times_and_errors();
        builder.find_calls();
        builder.walk_tree();
        builder
    }

    fn read_times_and_errors(&mut self) {
        let (table, projection) = (self.table, self.projection);
        for (index, activity) in projection.activities.iter().enumerate() {
            if let Some(column) = self.columns.timestamp {
                let ticks = activity.event_rows.iter().filter_map(|row| {
                    parse_datetime_ticks(&table.cell(*row, column)?.display_text())
                });
                let (earliest, latest) = ticks.fold((None, None), |(low, high), tick| {
                    (
                        Some(low.map_or(tick, |low: i64| low.min(tick))),
                        Some(high.map_or(tick, |high: i64| high.max(tick))),
                    )
                });
                self.start[index] = earliest;
                self.end[index] = latest;
            }
            self.error_row[index] = self.first_error_row(&activity.event_rows);
        }
        self.trace_start = self.start.iter().flatten().min().copied();
    }

    fn first_error_row(&self, rows: &[usize]) -> Option<usize> {
        let severity = self.columns.severity?;
        rows.iter().copied().find(|row| {
            severity_level(self.table.cell(*row, severity)).is_some_and(|level| level <= 2)
                && !self.message_of(*row).is_some_and(|text| is_filler(&text))
        })
    }

    fn message_of(&self, row: usize) -> Option<String> {
        let column = self.columns.message?;
        Some(self.table.cell(row, column)?.display_text().into_owned())
    }

    fn marker_of(&self, activity: usize) -> Option<&str> {
        self.projection.activities.get(activity)?.marker_name.as_deref()
    }

    /// One call per parent activity, callee actor and burst of overlapping child activities:
    /// a request often leaves two sibling activities in the callee, and one parent can make
    /// dozens of separate calls.
    fn find_calls(&mut self) {
        let mut buckets: Vec<((usize, usize), Vec<usize>)> = Vec::new();
        let mut bucket_of: HashMap<(usize, usize), usize> = HashMap::new();
        for (index, activity) in self.projection.activities.iter().enumerate() {
            let Some(parent) = activity.parent else {
                continue;
            };
            if self.actor_of[parent] == self.actor_of[index] {
                continue;
            }
            let key = (parent, self.actor_of[index]);
            let slot = *bucket_of.entry(key).or_insert_with(|| {
                buckets.push((key, Vec::new()));
                buckets.len() - 1
            });
            buckets[slot].1.push(index);
        }

        let mut calls = Vec::new();
        for ((parent, callee_actor), mut kids) in buckets {
            kids.sort_by_key(|kid| (self.start[*kid].unwrap_or(i64::MAX), self.first_row(*kid)));
            let mut cluster: Vec<usize> = Vec::new();
            let mut cluster_end: Option<i64> = None;
            for kid in kids {
                let overlaps = match (self.start[kid], cluster_end) {
                    (Some(start), Some(end)) => start <= end,
                    _ => false,
                };
                if !overlaps && !cluster.is_empty() {
                    calls.push(self.call_data(parent, callee_actor, std::mem::take(&mut cluster)));
                    cluster_end = None;
                }
                cluster_end = match (cluster_end, self.end[kid]) {
                    (Some(current), Some(end)) => Some(current.max(end)),
                    (None, end) => end,
                    (current, None) => current,
                };
                cluster.push(kid);
            }
            if !cluster.is_empty() {
                calls.push(self.call_data(parent, callee_actor, cluster));
            }
        }
        calls.sort_by_key(|call: &CallData| {
            (
                call.start.unwrap_or(i64::MAX),
                call.kids.first().map_or(0, |kid| self.first_row(*kid)),
            )
        });
        self.calls = calls;
    }

    fn call_data(&self, parent: usize, callee_actor: usize, kids: Vec<usize>) -> CallData {
        let start = kids.iter().filter_map(|kid| self.start[*kid]).min();
        let end = kids.iter().filter_map(|kid| self.end[*kid]).max();
        CallData {
            caller_activity: parent,
            caller_actor: self.actor_of[parent],
            callee_actor,
            kids,
            start,
            end,
        }
    }

    fn first_row(&self, activity: usize) -> usize {
        self.projection.activities[activity]
            .event_rows
            .first()
            .copied()
            .unwrap_or(0)
    }

    /// Nesting comes from the tree, not from timestamps: an activity's first logged event can
    /// come after its children's, and the clocks of different processes disagree.
    fn walk_tree(&mut self) {
        let projection = self.projection;
        let activities = &projection.activities;
        let mut call_of_kid: HashMap<usize, usize> = HashMap::new();
        for (call_index, call) in self.calls.iter().enumerate() {
            for kid in &call.kids {
                call_of_kid.insert(*kid, call_index);
            }
        }
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); activities.len()];
        let mut roots = Vec::new();
        for (index, activity) in activities.iter().enumerate() {
            match activity.parent {
                Some(parent) => children[parent].push(index),
                None => roots.push(index),
            }
        }

        let mut preorder = Vec::with_capacity(activities.len());
        let mut stack: Vec<(usize, Option<usize>, usize)> =
            roots.iter().rev().map(|root| (*root, None, *root)).collect();
        while let Some((activity, inherited, root)) = stack.pop() {
            let current = call_of_kid.get(&activity).copied().or(inherited);
            self.owner[activity] = current;
            self.root_of[activity] = root;
            preorder.push(activity);
            for child in children[activity].iter().rev() {
                stack.push((*child, current, root));
            }
        }

        for activity in 0..activities.len() {
            self.subtree_error[activity] = self.error_row[activity].is_some();
        }
        for activity in preorder.iter().rev() {
            if self.subtree_error[*activity] {
                if let Some(parent) = activities[*activity].parent {
                    self.subtree_error[parent] = true;
                    self.descendant_error[parent] = true;
                }
            }
        }

        self.nested_calls = vec![Vec::new(); self.calls.len()];
        for (call_index, call) in self.calls.iter().enumerate() {
            if let Some(outer) = self.owner[call.caller_activity] {
                self.nested_calls[outer].push(call_index);
            }
        }

        self.call_errors = vec![Vec::new(); self.calls.len()];
        for activity in &preorder {
            if self.error_row[*activity].is_none() || self.descendant_error[*activity] {
                continue;
            }
            match self.owner[*activity] {
                Some(call) => self.call_errors[call].push(*activity),
                None => self
                    .root_errors
                    .entry(self.root_of[*activity])
                    .or_default()
                    .push(*activity),
            }
        }
    }

    fn frames(&mut self) -> Vec<Frame> {
        let projection = self.projection;
        let activities = &projection.activities;
        let roots: Vec<usize> = (0..activities.len())
            .filter(|index| activities[*index].parent.is_none())
            .collect();
        let Some(largest) = roots
            .iter()
            .copied()
            .reduce(|best, root| {
                if activities[root].subtree_activity_count > activities[best].subtree_activity_count
                {
                    root
                } else {
                    best
                }
            })
        else {
            return Vec::new();
        };

        let mut top_calls: HashMap<usize, Vec<usize>> = HashMap::new();
        for (call_index, call) in self.calls.iter().enumerate() {
            if self.owner[call.caller_activity].is_none() {
                top_calls
                    .entry(self.root_of[call.caller_activity])
                    .or_default()
                    .push(call_index);
            }
        }

        let mut framed: Vec<usize> = roots
            .iter()
            .copied()
            .filter(|root| *root == largest || top_calls.contains_key(root))
            .collect();
        framed.sort_by_key(|root| (*root != largest, self.first_row(*root)));

        framed
            .into_iter()
            .map(|root| self.frame(root, top_calls.remove(&root).unwrap_or_default()))
            .collect()
    }

    fn frame(&mut self, root: usize, top_calls: Vec<usize>) -> Frame {
        let items = self.top_items(root, &top_calls);
        let scope_errors = self.root_errors.remove(&root).unwrap_or_default();
        Frame {
            actor: self.actor_of[root],
            label: self
                .marker_of(root)
                .map(short_marker)
                .unwrap_or_else(|| "(root)".to_string()),
            activity: root,
            duration_ms: self.duration_ms(self.start[root], self.end[root]),
            failed: self.subtree_error[root],
            items,
            notes: self.notes(scope_errors),
        }
    }

    /// The calls made directly by the root's own actor, grouped into steps.
    fn top_items(&mut self, root: usize, calls: &[usize]) -> Vec<Item> {
        let Some(step_depth) = self.options.step_depth else {
            let built = self.build_calls(calls, 0);
            return self.collapse(built);
        };
        let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();
        for call in calls {
            let label = self.step_label(root, self.calls[*call].caller_activity, step_depth);
            match groups.last_mut() {
                Some((current, members)) if *current == label => members.push(*call),
                _ => groups.push((label, vec![*call])),
            }
        }
        let mut items = Vec::new();
        for (label, members) in groups {
            let built = self.build_calls(&members, 0);
            let inner = self.collapse(built);
            match label {
                Some(label) => items.push(Item::Step(Step {
                    label,
                    items: inner,
                })),
                None => items.extend(inner),
            }
        }
        items
    }

    /// The marker of the ancestor `depth` levels below the root that is not itself a wrapper;
    /// when that one is, the nearest descendant on the way to the caller that is not.
    fn step_label(&self, root: usize, caller: usize, depth: usize) -> Option<String> {
        let activities = &self.projection.activities;
        let target = activities[root].depth + depth;
        let mut chain = Vec::new();
        let mut current = Some(caller);
        while let Some(activity) = current {
            if activities[activity].depth < target {
                break;
            }
            chain.push(activity);
            current = activities[activity].parent;
        }
        chain
            .iter()
            .rev()
            .filter_map(|activity| self.marker_of(*activity))
            .find(|marker| !self.is_wrapper(marker))
            .map(short_marker)
    }

    fn is_wrapper(&self, marker: &str) -> bool {
        self.options
            .wrapper_markers
            .iter()
            .any(|pattern| matches_pattern(pattern, marker))
    }

    fn build_calls(&mut self, indexes: &[usize], nesting: usize) -> Vec<Call> {
        let mut built = Vec::new();
        for index in indexes {
            if nesting >= MAX_CALL_NESTING {
                self.omitted_calls += 1 + self.count_nested(*index);
                continue;
            }
            let nested = self.nested_calls[*index].clone();
            let inner = self.build_calls(&nested, nesting + 1);
            let items = self.collapse(inner);
            built.push(self.call(*index, items));
        }
        built
    }

    fn count_nested(&self, call: usize) -> usize {
        let mut total = 0;
        let mut pending = self.nested_calls[call].clone();
        while let Some(next) = pending.pop() {
            total += 1;
            pending.extend(self.nested_calls[next].iter().copied());
        }
        total
    }

    fn call(&self, index: usize, items: Vec<Item>) -> Call {
        let data = &self.calls[index];
        let caller_marker = self.marker_of(data.caller_activity);
        let callee_marker = data
            .kids
            .first()
            .and_then(|kid| self.marker_of(*kid))
            .map(short_marker);
        let label = match (caller_marker, &callee_marker) {
            (Some(marker), Some(callee)) if self.is_wrapper(marker) => {
                format!("{} → {callee}", short_marker(marker))
            }
            (Some(marker), _) => short_marker(marker),
            (None, Some(callee)) => callee.clone(),
            (None, None) => "call".to_string(),
        };
        Call {
            caller: data.caller_actor,
            callee: data.callee_actor,
            label,
            callee_marker,
            caller_activity: data.caller_activity,
            activities: data.kids.clone(),
            offset_seconds: self.offset_seconds(data.start),
            duration_ms: self.duration_ms(data.start, data.end),
            failed: data.kids.iter().any(|kid| self.subtree_error[*kid]),
            items,
            notes: self.notes(self.call_errors[index].clone()),
        }
    }

    fn collapse(&self, calls: Vec<Call>) -> Vec<Item> {
        if !self.options.collapse_repeats {
            return calls.into_iter().map(Item::Call).collect();
        }
        let mut items: Vec<Item> = Vec::new();
        let mut run: Vec<Call> = Vec::new();
        for call in calls {
            let simple = call.items.is_empty() && !call.failed && call.notes.shown.is_empty();
            let continues = simple
                && run.first().is_some_and(|first| {
                    (first.caller, first.callee, &first.label)
                        == (call.caller, call.callee, &call.label)
                });
            if continues {
                run.push(call);
                continue;
            }
            self.flush_run(&mut run, &mut items);
            if simple {
                run.push(call);
            } else {
                items.push(Item::Call(call));
            }
        }
        self.flush_run(&mut run, &mut items);
        items
    }

    fn flush_run(&self, run: &mut Vec<Call>, items: &mut Vec<Item>) {
        let calls = std::mem::take(run);
        match calls.len() {
            0 => {}
            1 => items.extend(calls.into_iter().map(Item::Call)),
            _ => {
                let durations = calls.iter().filter_map(|call| call.duration_ms);
                let shortest = durations.clone().reduce(f64::min);
                let longest = durations.reduce(f64::max);
                let first = &calls[0];
                items.push(Item::Repeat(Repeat {
                    caller: first.caller,
                    callee: first.callee,
                    label: first.label.clone(),
                    first_offset_seconds: first.offset_seconds,
                    last_offset_seconds: calls.last().and_then(|call| call.offset_seconds),
                    shortest_ms: shortest,
                    longest_ms: longest,
                    calls,
                }));
            }
        }
    }

    /// One note per distinct error, for the errors that started in this scope, so an error that
    /// is logged again by each of five callers is told once, where it began.
    fn notes(&self, mut activities: Vec<usize>) -> ErrorNotes {
        activities.sort_by_key(|activity| {
            (
                self.start[*activity].unwrap_or(i64::MAX),
                self.first_row(*activity),
            )
        });
        let mut groups: Vec<Note> = Vec::new();
        for activity in activities {
            let Some(row) = self.error_row[activity] else {
                continue;
            };
            let marker = self
                .marker_of(activity)
                .map(short_marker)
                .unwrap_or_default();
            let text = self
                .message_of(row)
                .map(|message| note_text(&message, self.options.note_characters))
                .unwrap_or_default();
            let actor = self.actor_of[activity];
            match groups
                .iter_mut()
                .find(|note| (note.actor, &note.marker, &note.text) == (actor, &marker, &text))
            {
                Some(existing) => existing.count += 1,
                None => groups.push(Note {
                    actor,
                    marker,
                    text,
                    count: 1,
                    source_row: row,
                }),
            }
        }
        let omitted = groups.len().saturating_sub(self.options.notes_per_call);
        groups.truncate(self.options.notes_per_call);
        ErrorNotes {
            shown: groups,
            omitted,
        }
    }

    fn offset_seconds(&self, ticks: Option<i64>) -> Option<f64> {
        Some((ticks? - self.trace_start?) as f64 / TICKS_PER_SECOND as f64)
    }

    fn duration_ms(&self, start: Option<i64>, end: Option<i64>) -> Option<f64> {
        Some((end? - start?) as f64 / TICKS_PER_MILLISECOND)
    }

    /// Roots that were not drawn, and the activities in them.
    fn unframed(&self, frames: &[Frame]) -> (usize, usize) {
        let framed: Vec<usize> = frames.iter().map(|frame| frame.activity).collect();
        self.projection
            .activities
            .iter()
            .enumerate()
            .filter(|(index, activity)| activity.parent.is_none() && !framed.contains(index))
            .fold((0, 0), |(roots, activities), (_, activity)| {
                (roots + 1, activities + activity.subtree_activity_count)
            })
    }
}

/// A row whose text says nothing about what went wrong: a later part of a split message, or a
/// notice that a message was split.
fn is_filler(text: &str) -> bool {
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
fn split_part_prefix(text: &str) -> Option<(usize, &str)> {
    let (part, rest) = text.split_once('/')?;
    let (total, rest) = rest.split_once(':')?;
    if total.is_empty() || !total.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let part: usize = part.parse().ok()?;
    Some((part, rest.trim_start()))
}

/// What a note says: the `message` of a JSON error when there is one, else the text itself,
/// on one line and cut to `limit` characters.
fn note_text(message: &str, limit: usize) -> String {
    let text = match split_part_prefix(message.trim_start()) {
        Some((_, rest)) => rest,
        None => message.trim_start(),
    };
    let text = json_message(text).unwrap_or_else(|| text.to_string());
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > limit {
        let cut: String = one_line.chars().take(limit).collect();
        format!("{}…", cut.trim_end())
    } else {
        one_line
    }
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
fn short_marker(marker: &str) -> String {
    let segments: Vec<&str> = marker.split('.').collect();
    match segments.len() {
        0..=2 => marker.to_string(),
        count => segments[count - 2..].join("."),
    }
}

fn matches_pattern(pattern: &str, text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    let core = pattern.trim_matches('*');
    match (pattern.starts_with('*'), pattern.len() > 1 && pattern.ends_with('*')) {
        (true, true) => text.contains(core),
        (true, false) => text.ends_with(core),
        (false, true) => text.starts_with(core),
        (false, false) => text == core,
    }
}

/// Short names for the actors: generic trailing segments dropped, then the fewest trailing
/// segments that tell every actor apart.
fn display_names(names: &[String], generic_suffixes: &[String]) -> Vec<String> {
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

/// Left to right: callers before the actors they call, so arrows mostly point right. An
/// actor's place is the longest chain of calls above it, then the order it first appears.
fn actor_order(frames: &[Frame], actor_count: usize) -> Vec<usize> {
    let mut first_seen: Vec<usize> = Vec::new();
    let mut calls: Vec<(usize, usize)> = Vec::new();
    let mut pending: Vec<&Item> = Vec::new();
    for frame in frames {
        if !first_seen.contains(&frame.actor) {
            first_seen.push(frame.actor);
        }
        pending.extend(frame.items.iter().rev());
        while let Some(item) = pending.pop() {
            match item {
                Item::Call(call) => {
                    for actor in [call.caller, call.callee] {
                        if !first_seen.contains(&actor) {
                            first_seen.push(actor);
                        }
                    }
                    calls.push((call.caller, call.callee));
                    pending.extend(call.items.iter().rev());
                }
                Item::Repeat(repeat) => {
                    for actor in [repeat.caller, repeat.callee] {
                        if !first_seen.contains(&actor) {
                            first_seen.push(actor);
                        }
                    }
                    calls.push((repeat.caller, repeat.callee));
                }
                Item::Step(step) => pending.extend(step.items.iter().rev()),
            }
        }
    }
    let mut level = vec![0usize; actor_count];
    for _ in 0..first_seen.len() {
        for (caller, callee) in &calls {
            level[*callee] = level[*callee].max(level[*caller] + 1).min(first_seen.len());
        }
    }
    first_seen.sort_by_key(|actor| level[*actor]);
    first_seen
}

impl SequenceDiagram {
    /// Mermaid `sequenceDiagram` text. Values are cleaned so the text always parses.
    pub fn to_mermaid(&self) -> String {
        let mut lines = vec!["sequenceDiagram".to_string(), "    autonumber".to_string()];
        if !self.frames.is_empty() {
            lines.push("    participant caller as (caller not in result)".to_string());
        }
        for actor in &self.order {
            lines.push(format!(
                "    participant {} as {}",
                alias(*actor),
                clean(&self.actors[*actor].display)
            ));
        }
        for frame in &self.frames {
            let actor = alias(frame.actor);
            lines.push(format!("    caller->>+{actor}: {}", clean(&frame.label)));
            self.write_items(&frame.items, 1, frame.actor, &mut lines);
            self.write_notes(&frame.notes, 1, &mut lines);
            lines.push(format!(
                "    {actor}-->>-caller: {}{}",
                duration_text(frame.duration_ms),
                if frame.failed { " ✗ error" } else { "" }
            ));
        }
        if self.omitted_roots > 0 || self.omitted_calls > 0 {
            if let (Some(first), Some(last)) = (self.order.first(), self.order.last()) {
                let mut parts = Vec::new();
                if self.omitted_roots > 0 {
                    parts.push(format!(
                        "{} activities in {} other {} are not shown",
                        self.omitted_activities,
                        self.omitted_roots,
                        if self.omitted_roots == 1 { "root" } else { "roots" }
                    ));
                }
                if self.omitted_calls > 0 {
                    parts.push(format!(
                        "{} calls nested too deeply are not shown",
                        self.omitted_calls
                    ));
                }
                lines.push(format!(
                    "    Note over {},{}: {}",
                    alias(*first),
                    alias(*last),
                    parts.join(", ")
                ));
            }
        }
        lines.join("\n") + "\n"
    }

    fn write_items(&self, items: &[Item], depth: usize, frame_actor: usize, lines: &mut Vec<String>) {
        let indent = "    ".repeat(depth + 1);
        for item in items {
            match item {
                Item::Call(call) => {
                    let (caller, callee) = (alias(call.caller), alias(call.callee));
                    lines.push(format!("{indent}{caller}->>+{callee}: {}", clean(&call.label)));
                    self.write_items(&call.items, depth + 1, frame_actor, lines);
                    self.write_notes(&call.notes, depth + 1, lines);
                    lines.push(format!(
                        "{indent}{callee}-->>-{caller}: {}{}",
                        duration_text(call.duration_ms),
                        if call.failed { " ✗ error" } else { "" }
                    ));
                }
                Item::Repeat(repeat) => {
                    let (caller, callee) = (alias(repeat.caller), alias(repeat.callee));
                    lines.push(format!(
                        "{indent}loop {} ×{}{}",
                        clean(&repeat.label),
                        repeat.calls.len(),
                        repeat_detail(repeat)
                    ));
                    lines.push(format!("{indent}    {caller}->>{callee}: call"));
                    lines.push(format!("{indent}    {callee}-->>{caller}: done"));
                    lines.push(format!("{indent}end"));
                }
                Item::Step(step) => {
                    lines.push(format!("{indent}rect rgba(128, 128, 128, 0.12)"));
                    lines.push(format!(
                        "{indent}    Note over {}: {}",
                        alias(frame_actor),
                        clean(&step.label)
                    ));
                    self.write_items(&step.items, depth + 1, frame_actor, lines);
                    lines.push(format!("{indent}end"));
                }
            }
        }
    }

    fn write_notes(&self, notes: &ErrorNotes, depth: usize, lines: &mut Vec<String>) {
        let indent = "    ".repeat(depth + 1);
        for note in &notes.shown {
            let count = if note.count > 1 {
                format!(" ×{}", note.count)
            } else {
                String::new()
            };
            lines.push(format!(
                "{indent}Note over {}: ✗ {}: {}{count}",
                alias(note.actor),
                clean(&note.marker),
                clean(&note.text)
            ));
        }
        if notes.omitted > 0 {
            if let Some(note) = notes.shown.first() {
                lines.push(format!(
                    "{indent}Note over {}: ✗ +{} more distinct errors",
                    alias(note.actor),
                    notes.omitted
                ));
            }
        }
    }
}

fn alias(actor: usize) -> String {
    format!("a{actor}")
}

fn duration_text(duration_ms: Option<f64>) -> String {
    match duration_ms {
        Some(duration) => format!("{duration:.0} ms"),
        None => "return".to_string(),
    }
}

fn repeat_detail(repeat: &Repeat) -> String {
    let span = match (repeat.first_offset_seconds, repeat.last_offset_seconds) {
        (Some(first), Some(last)) => Some(format!("+{first:.3}s to +{last:.3}s")),
        _ => None,
    };
    let durations = match (repeat.shortest_ms, repeat.longest_ms) {
        (Some(shortest), Some(longest)) => Some(format!("{shortest:.0}–{longest:.0} ms each")),
        _ => None,
    };
    let parts: Vec<String> = [span, durations].into_iter().flatten().collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// Text that is safe inside a Mermaid line: `;` ends a statement and `#` and `%` start an
/// entity or a comment, and quotes and backslashes upset some versions.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, ';' | '#' | '%' | '"' | '\\'))
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::build_projection_with;
    use crate::result::{Cell, Column};
    use pretty_assertions::assert_eq;

    struct Event {
        activity: &'static str,
        parent: &'static str,
        actor: &'static str,
        marker: &'static str,
        millis: i64,
        level: i64,
        message: String,
    }

    fn event(
        activity: &'static str,
        parent: &'static str,
        actor: &'static str,
        marker: &'static str,
        millis: i64,
    ) -> Event {
        Event {
            activity,
            parent,
            actor,
            marker,
            millis,
            level: 4,
            message: "ok".into(),
        }
    }

    fn failing(mut event: Event, message: &str) -> Event {
        event.level = 2;
        event.message = message.into();
        event
    }

    fn trace(events: Vec<Event>) -> Table {
        Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
                Column::new("ProcessName", "string"),
                Column::new("MarkerName", "string"),
                Column::new("TIMESTAMP", "datetime"),
                Column::new("Level", "long"),
                Column::new("MessageText", "string"),
            ],
            rows: events
                .into_iter()
                .map(|event| {
                    let text = |value: &str| {
                        if value.is_empty() {
                            Cell::Null
                        } else {
                            Cell::Text(value.into())
                        }
                    };
                    vec![
                        text(event.activity),
                        text(event.parent),
                        text(event.actor),
                        text(event.marker),
                        Cell::Text(format!(
                            "2026-01-01T00:00:{:02}.{:07}Z",
                            event.millis / 1000,
                            (event.millis % 1000) * 10_000
                        )),
                        Cell::Int(event.level),
                        Cell::Text(event.message),
                    ]
                })
                .collect(),
        }
    }

    fn diagram_with(table: &Table, options: &SequenceOptions) -> Option<SequenceDiagram> {
        let columns = TraceColumns::detect(table);
        let projection = build_projection_with(table, &columns)?;
        build_sequence(table, &projection, &columns, options)
    }

    fn diagram(table: &Table) -> SequenceDiagram {
        diagram_with(table, &SequenceOptions::default()).expect("a sequence")
    }

    fn calls(items: &[Item]) -> Vec<&Call> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Call(call) => Some(call),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn needs_an_actor_and_a_timestamp_column() {
        let mut table = trace(vec![event("r", "", "A", "Run", 0)]);
        table.columns.remove(2);
        for row in &mut table.rows {
            row.remove(2);
        }
        assert!(diagram_with(&table, &SequenceOptions::default()).is_none());
    }

    #[test]
    fn draws_a_call_between_actors_inside_a_frame() {
        let table = trace(vec![
            event("r", "", "Svc.A.Service", "Job.Run", 0),
            event("s", "r", "Svc.A.Service", "Job.Prepare", 5),
            event("c", "s", "Svc.A.Service", "Client.GetToken", 10),
            event("k", "c", "Svc.B.Service", "WebApi-IncomingRequest", 12),
            event("k", "c", "Svc.B.Service", "WebApi-IncomingRequest", 14),
            event("r", "", "Svc.A.Service", "Job.Run", 100),
        ]);
        assert_eq!(
            diagram(&table).to_mermaid(),
            "sequenceDiagram
    autonumber
    participant caller as (caller not in result)
    participant a0 as A
    participant a1 as B
    caller->>+a0: Job.Run
        rect rgba(128, 128, 128, 0.12)
            Note over a0: Job.Prepare
            a0->>+a1: Client.GetToken
            a1-->>-a0: 2 ms
        end
    a0-->>-caller: 100 ms
"
        );
    }

    #[test]
    fn work_inside_one_actor_is_not_a_call() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("x", "r", "A", "Inner", 1),
            event("y", "x", "A", "Deeper", 2),
        ]);
        let result = diagram(&table);
        assert_eq!(result.frames.len(), 1);
        assert!(result.frames[0].items.is_empty());
    }

    #[test]
    fn overlapping_child_activities_are_one_call_and_a_later_burst_is_another() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("p", "r", "A", "Ask", 5),
            event("k1", "p", "B", "Entry", 10),
            event("k1", "p", "B", "Entry", 20),
            event("k2", "p", "B", "Middleware", 10),
            event("k2", "p", "B", "Middleware", 20),
            event("k3", "p", "B", "Entry", 50),
            event("k3", "p", "B", "Entry", 60),
        ]);
        let result = diagram(&table);
        let found = result.frames[0]
            .items
            .iter()
            .flat_map(|item| match item {
                Item::Step(step) => step.items.clone(),
                other => vec![other.clone()],
            })
            .collect::<Vec<_>>();
        let sizes: Vec<usize> = found
            .iter()
            .flat_map(|item| match item {
                Item::Call(call) => vec![call.activities.len()],
                Item::Repeat(repeat) => repeat.calls.iter().map(|c| c.activities.len()).collect(),
                Item::Step(_) => Vec::new(),
            })
            .collect();
        assert_eq!(sizes, vec![2, 1]);
    }

    #[test]
    fn nesting_follows_the_tree_when_clocks_disagree() {
        let table = trace(vec![
            event("r", "", "A", "Run", 1000),
            event("c1", "r", "A", "Ask", 1010),
            event("k1", "c1", "B", "Entry", 10),
            event("c2", "k1", "B", "AskAgain", 11),
            event("k2", "c2", "C", "Entry", 5000),
            event("k2", "c2", "C", "Entry", 5001),
        ]);
        let result = diagram_with(
            &table,
            &SequenceOptions {
                step_depth: None,
                ..SequenceOptions::default()
            },
        )
        .expect("a sequence");
        let outer = calls(&result.frames[0].items);
        assert_eq!(outer.len(), 1);
        let inner = calls(&outer[0].items);
        assert_eq!(inner.len(), 1);
        assert_eq!(
            (inner[0].caller, inner[0].callee),
            (outer[0].callee, 2)
        );
    }

    fn repeated_calls(count: i64) -> Table {
        let mut events = vec![event("r", "", "A", "Run", 0)];
        let callers = ["p0", "p1", "p2", "p3"];
        let kids = ["k0", "k1", "k2", "k3"];
        for index in 0..count as usize {
            let start = 10 + index as i64 * 20;
            events.push(event(callers[index], "r", "A", "Poll", start));
            events.push(event(kids[index], callers[index], "B", "Entry", start + 1));
            events.push(event(kids[index], callers[index], "B", "Entry", start + 3 + index as i64));
        }
        trace(events)
    }

    #[test]
    fn consecutive_identical_calls_collapse_into_a_loop() {
        let result = diagram(&repeated_calls(3));
        let items: Vec<&Item> = result.frames[0]
            .items
            .iter()
            .flat_map(|item| match item {
                Item::Step(step) => step.items.iter().collect(),
                other => vec![other],
            })
            .collect();
        assert_eq!(items.len(), 1);
        let Item::Repeat(repeat) = items[0] else {
            panic!("expected a loop");
        };
        assert_eq!(repeat.calls.len(), 3);
        assert_eq!(repeat.shortest_ms, Some(2.0));
        assert_eq!(repeat.longest_ms, Some(4.0));
        let text = result.to_mermaid();
        assert!(text.contains("loop Poll ×3 (+0.011s to +0.051s, 2–4 ms each)"), "{text}");
    }

    #[test]
    fn collapsing_can_be_switched_off() {
        let table = repeated_calls(3);
        let options = SequenceOptions {
            collapse_repeats: false,
            step_depth: None,
            ..SequenceOptions::default()
        };
        let result = diagram_with(&table, &options).expect("a sequence");
        assert_eq!(calls(&result.frames[0].items).len(), 3);
    }

    #[test]
    fn an_error_is_noted_where_it_was_logged_and_marks_every_caller() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("c1", "r", "A", "Ask", 10),
            event("k1", "c1", "B", "Entry", 11),
            event("c2", "k1", "B", "Query", 12),
            event("k2", "c2", "C", "Db", 13),
            failing(
                event("k2", "c2", "C", "Db", 14),
                "1/3: {\"code\":\"InternalError\",\"message\":\"Row not found!\",\"timeStamp\":\"x\"",
            ),
            event("k2", "c2", "C", "Db", 15),
        ]);
        let options = SequenceOptions {
            step_depth: None,
            ..SequenceOptions::default()
        };
        let result = diagram_with(&table, &options).expect("a sequence");
        let frame = &result.frames[0];
        assert!(frame.failed);
        assert!(frame.notes.shown.is_empty());
        let outer = calls(&frame.items)[0];
        assert!(outer.failed);
        assert!(outer.notes.shown.is_empty());
        let inner = calls(&outer.items)[0];
        assert!(inner.failed);
        assert_eq!(inner.notes.shown.len(), 1);
        assert_eq!(inner.notes.shown[0].text, "Row not found!");
        assert_eq!(inner.notes.shown[0].marker, "Db");
    }

    #[test]
    fn an_error_logged_again_on_the_way_up_is_noted_once_where_it_began() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("c", "r", "A", "Ask", 10),
            failing(event("k", "c", "B", "Entry", 11), "request failed"),
            event("inner", "k", "B", "Query", 12),
            failing(event("inner", "k", "B", "Query", 13), "disk is full"),
        ]);
        let result = diagram(&table);
        let found = result.to_mermaid();
        assert!(found.contains("disk is full"), "{found}");
        assert!(!found.contains("request failed"), "{found}");
        assert!(result.frames[0].failed);
    }

    #[test]
    fn rows_that_say_nothing_are_never_the_note() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("c", "r", "A", "Ask", 10),
            failing(event("k", "c", "B", "Entry", 11), "The message is splitted into 3 parts."),
            failing(event("k", "c", "B", "Entry", 12), "2/3: at Some.Frame()"),
            failing(event("k", "c", "B", "Entry", 13), "Monitored scope end."),
            failing(event("k", "c", "B", "Entry", 14), "Disk is full"),
        ]);
        let result = diagram(&table);
        let found = result.to_mermaid();
        assert!(found.contains("Disk is full"), "{found}");
        assert!(!found.contains("splitted"), "{found}");
    }

    #[test]
    fn identical_errors_merge_and_distinct_ones_are_limited() {
        let mut events = vec![
            event("r", "", "A", "Run", 0),
            event("c", "r", "A", "Ask", 10),
        ];
        for (index, id) in ["k1", "k2", "k3", "k4", "k5"].into_iter().enumerate() {
            events.push(failing(event(id, "c", "B", "Entry", 11), if index < 2 { "same" } else { ["x", "y", "z"][index - 2] }));
            events.push(event(id, "c", "B", "Entry", 12));
        }
        let result = diagram(&trace(events));
        let call = match &result.frames[0].items[0] {
            Item::Step(step) => calls(&step.items)[0].clone(),
            Item::Call(call) => call.clone(),
            Item::Repeat(_) => panic!("unexpected loop"),
        };
        assert_eq!(call.notes.shown.len(), 3);
        assert_eq!(call.notes.shown[0].text, "same");
        assert_eq!(call.notes.shown[0].count, 2);
        assert_eq!(call.notes.omitted, 1);
    }

    #[test]
    fn text_that_would_break_mermaid_is_cleaned() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("c", "r", "A", "Ask;#1", 10),
            failing(event("k", "c", "B", "Entry", 11), "50% \"quoted\" a;b # c \\ d"),
        ]);
        let text = diagram(&table).to_mermaid();
        let line = text
            .lines()
            .find(|line| line.contains("Note over a1"))
            .expect("a note");
        for banned in [';', '#', '%', '"', '\\'] {
            assert!(!line.contains(banned), "{line}");
        }
        assert!(text.contains("Ask1"), "{text}");
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
    fn a_wrapper_marker_is_not_a_label_or_a_step() {
        let table = trace(vec![
            event("r", "", "A", "Job.Run", 0),
            event("w", "r", "A", "WebApi-IncomingRequest", 5),
            event("d", "w", "A", "Real.Reason", 6),
            event("c", "d", "A", "Web-IncomingRequest", 7),
            event("k", "c", "B", "Entry", 8),
        ]);
        let result = diagram(&table);
        let Item::Step(step) = &result.frames[0].items[0] else {
            panic!("expected a step");
        };
        assert_eq!(step.label, "Real.Reason");
        assert_eq!(calls(&step.items)[0].label, "Web-IncomingRequest → Entry");
    }

    #[test]
    fn steps_can_be_turned_off() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("s", "r", "A", "Phase", 5),
            event("c", "s", "A", "Ask", 6),
            event("k", "c", "B", "Entry", 7),
        ]);
        let options = SequenceOptions {
            step_depth: None,
            ..SequenceOptions::default()
        };
        let text = diagram_with(&table, &options).expect("a sequence").to_mermaid();
        assert!(!text.contains("rect"), "{text}");
    }

    #[test]
    fn roots_without_calls_are_counted_and_the_largest_root_is_always_drawn() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("x", "r", "A", "Inner", 1),
            event("o", "", "A", "Other", 2),
        ]);
        let result = diagram(&table);
        assert_eq!(result.frames.len(), 1);
        assert_eq!(result.frames[0].label, "Run");
        assert_eq!((result.omitted_roots, result.omitted_activities), (1, 1));
        assert!(result.to_mermaid().contains("1 activities in 1 other root are not shown"));

        let alone = diagram(&trace(vec![event("r", "", "A", "Run", 0)]));
        assert_eq!(alone.frames.len(), 1);
        assert!(!alone.to_mermaid().contains("not shown"));
    }

    #[test]
    fn a_second_root_that_holds_calls_gets_its_own_frame() {
        let table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("x", "r", "A", "Inner", 1),
            event("y", "x", "A", "Deeper", 2),
            event("o", "", "A", "Other", 20),
            event("c", "o", "A", "Ask", 21),
            event("k", "c", "B", "Entry", 22),
        ]);
        let result = diagram(&table);
        assert_eq!(result.frames.len(), 2);
        assert_eq!(result.frames[0].label, "Run");
        assert_eq!(result.frames[1].label, "Other");
        assert_eq!(result.omitted_roots, 0);
    }

    #[test]
    fn unreadable_timestamps_leave_durations_out() {
        let mut table = trace(vec![
            event("r", "", "A", "Run", 0),
            event("c", "r", "A", "Ask", 1),
            event("k", "c", "B", "Entry", 2),
        ]);
        for row in &mut table.rows {
            row[4] = Cell::Text("not a time".into());
        }
        let result = diagram(&table);
        assert_eq!(result.frames[0].duration_ms, None);
        assert!(result.to_mermaid().contains(": return"));
    }

    #[test]
    fn a_very_deep_chain_of_calls_is_limited_not_overflowed() {
        let ids: Vec<String> = (0..200).map(|index| format!("n{index}")).collect();
        let ids: Vec<&'static str> = ids
            .into_iter()
            .map(|id| &*Box::leak(id.into_boxed_str()))
            .collect();
        let mut events = vec![event("root", "", "A0", "Run", 0)];
        for (index, id) in ids.iter().enumerate() {
            let parent = if index == 0 { "root" } else { ids[index - 1] };
            let actor: &'static str = if index % 2 == 0 { "B0" } else { "A0" };
            events.push(event(id, parent, actor, "Hop", index as i64 + 1));
        }
        let options = SequenceOptions {
            step_depth: None,
            ..SequenceOptions::default()
        };
        let result = diagram_with(&trace(events), &options).expect("a sequence");
        assert_eq!(result.omitted_calls, 200 - MAX_CALL_NESTING);
        assert!(result.to_mermaid().contains("calls nested too deeply"));
    }

    /// Needs the real traces in `fork-docs/samples`, which are not committed.
    /// Run with `cargo test -p kusto_results --lib real_traces -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_traces() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fork-docs/samples");
        for name in ["sample1.ktt", "sample2.ktt"] {
            let Ok(text) = std::fs::read_to_string(root.join(name)) else {
                eprintln!("{name} is not here, skipped");
                continue;
            };
            let result = crate::result::ResultSet::from_json(&text).expect("a result file");
            let table = &result.tables[0];
            let started = std::time::Instant::now();
            let diagram = diagram(table);
            eprintln!("{name}: built in {:?}", started.elapsed());
            let mermaid = diagram.to_mermaid();
            std::fs::write(
                std::env::temp_dir().join(format!("{name}.rust.mmd")),
                &mermaid,
            )
            .expect("write the diagram");
            let drawn = mermaid.matches("->>").count();
            eprintln!("{name}: {} frames, {drawn} arrows drawn", diagram.frames.len());
            assert!(drawn > 0);
        }
    }
}
