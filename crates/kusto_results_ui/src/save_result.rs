//! Saving a copy of a result into the project, to keep it: a save prompt in the folder of the
//! file being worked on, with a readable name made from the query and the time it ran.

use std::fmt::Display;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use fs::{CopyOptions, Fs};
use gpui::{Action, Context, Window};
use gpui_util::ResultExt as _;
use project::DirectoryLister;
use schemars::JsonSchema;
use serde::Deserialize;
use workspace::notifications::{DetachAndPromptErr as _, NotificationId};
use workspace::{OpenOptions, OpenVisible, Toast, Workspace};

use crate::history::{NO_QUERY_TEXT, title};
use crate::results_panel::ResultsPanel;
use crate::results_viewer::ResultsViewer;

/// Saves a copy of a result into your project, to keep it. It is asked where and under what name;
/// the name starts out as a short form of the query and the time.
#[derive(Clone, PartialEq, Debug, Default, Deserialize, JsonSchema, Action)]
#[action(namespace = kusto)]
#[serde(deny_unknown_fields)]
pub struct SaveResult {
    /// The result file to copy. When missing, the result of the active tab, or else of the
    /// Results panel.
    #[serde(default)]
    pub path: Option<String>,
    /// The name to suggest, without folders.
    #[serde(default)]
    pub suggested_name: Option<String>,
}

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, action: &SaveResult, window, cx| {
        save_result(workspace, action, window, cx)
    });
}

const LONGEST_SLUG: usize = 48;

/// A file name for a result: the query's title (its leading comments, then its first line of
/// code) and when it ran, such as `incident-123-t-where-id-raid-2026-10-01-1030.ktt`.
pub(crate) fn suggested_file_name<Zone: TimeZone>(
    query: &str,
    started_at: Option<&str>,
    zone: &Zone,
) -> String
where
    Zone::Offset: Display,
{
    let title = title(query);
    let title = if title == NO_QUERY_TEXT { "" } else { &title };
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    if slug.chars().count() > LONGEST_SLUG {
        slug = slug.chars().take(LONGEST_SLUG).collect();
        // Cut at a word rather than in the middle of one.
        if let Some(boundary) = slug.rfind('-')
            && boundary > LONGEST_SLUG / 2
        {
            slug.truncate(boundary);
        }
    }
    let slug = slug.trim_matches('-');
    let when = started_at
        .and_then(|started| DateTime::parse_from_rfc3339(started).ok())
        .map(|started| {
            started
                .with_timezone(zone)
                .format("%Y-%m-%d-%H%M")
                .to_string()
        });
    let stem = match (slug.is_empty(), when) {
        (true, Some(when)) => format!("result-{when}"),
        (true, None) => "result".to_string(),
        (false, Some(when)) => format!("{slug}-{when}"),
        (false, None) => slug.to_string(),
    };
    format!("{stem}.ktt")
}

/// The same, in the local time zone, which is what the person who ran the query remembers.
pub(crate) fn suggested_local_file_name(query: &str, started_at: Option<&str>) -> String {
    suggested_file_name(query, started_at, &Local)
}

fn save_result(
    workspace: &mut Workspace,
    action: &SaveResult,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let source = match (&action.path, &action.suggested_name) {
        (Some(path), name) => Some((PathBuf::from(path), name.clone())),
        (None, _) => {
            let viewer = workspace
                .active_item_as::<ResultsViewer>(cx)
                .or_else(|| {
                    workspace
                        .panel::<ResultsPanel>(cx)
                        .and_then(|panel| panel.read(cx).shown_viewer().cloned())
                })
                .and_then(|viewer| viewer.read(cx).save_action(cx));
            viewer.and_then(|save| Some((PathBuf::from(save.path?), save.suggested_name)))
        }
    };
    let Some((source, name)) = source else {
        workspace.show_error(anyhow::anyhow!("There is no result to save."), cx);
        return;
    };

    let fs = workspace.app_state().fs.clone();
    let project = workspace.project().clone();
    let choice = workspace.prompt_for_new_path(
        DirectoryLister::Project(project),
        name.or_else(|| {
            source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }),
        window,
        cx,
    );
    cx.spawn_in(window, async move |workspace, cx| {
        let Some(mut target) = choice
            .await
            .ok()
            .flatten()
            .and_then(|paths| paths.into_iter().next())
        else {
            return;
        };
        if target.extension().is_none() {
            target.set_extension("ktt");
        }
        let copied = copy_result(fs.as_ref(), &source, &target).await;
        workspace
            .update_in(cx, |workspace, _, cx| match copied {
                Ok(()) => {
                    let name = target
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let weak_workspace = cx.weak_entity();
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<SaveResult>(),
                            format!("Saved {name}"),
                        )
                        .on_click("Open", move |window, cx| {
                            let target = target.clone();
                            weak_workspace
                                .update(cx, |workspace, cx| {
                                    workspace
                                        .open_abs_path(
                                            target,
                                            OpenOptions {
                                                visible: Some(OpenVisible::None),
                                                ..Default::default()
                                            },
                                            window,
                                            cx,
                                        )
                                        .detach_and_prompt_err(
                                            "Could not open the saved result",
                                            window,
                                            cx,
                                            |_, _, _| None,
                                        )
                                })
                                .log_err();
                        })
                        .autohide(),
                        cx,
                    );
                }
                Err(error) => workspace.show_error(error, cx),
            })
            .log_err();
    })
    .detach();
}

/// Copies a result file. The prompt has already asked about replacing a file that is there.
async fn copy_result(
    fs: &dyn Fs,
    source: &std::path::Path,
    target: &std::path::Path,
) -> Result<()> {
    if source == target {
        return Ok(());
    }
    fs.copy_file(
        source,
        target,
        CopyOptions {
            overwrite: true,
            ignore_if_exists: false,
        },
    )
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "Could not save the result to {}: {error:#}",
            target.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;

    use super::*;
    use crate::run_query::tests::{ANSWER, run, setup};

    fn zone() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).expect("an offset")
    }

    #[test]
    fn a_name_is_the_title_and_the_local_time() {
        assert_eq!(
            suggested_file_name(
                "// Incident 123!\nT | where Id == raid",
                Some("2026-10-01T08:30:05.000Z"),
                &zone()
            ),
            "incident-123-t-where-id-raid-2026-10-01-1030.ktt"
        );
    }

    #[test]
    fn a_long_title_is_cut_at_a_word_and_a_missing_one_gives_a_plain_name() {
        let name = suggested_file_name(
            "ASEdog().ASTrace | where RootActivityId == 'something-very-long-indeed' | project x",
            None,
            &zone(),
        );
        assert!(name.ends_with(".ktt"));
        let stem = name.trim_end_matches(".ktt");
        assert!(stem.chars().count() <= LONGEST_SLUG, "{stem}");
        assert!(!stem.ends_with('-') && !stem.starts_with('-'), "{stem}");

        assert_eq!(
            suggested_file_name("", Some("2026-10-01T08:30:05Z"), &zone()),
            "result-2026-10-01-1030.ktt"
        );
        assert_eq!(suggested_file_name("//\n", None, &zone()), "result.ktt");
        assert_eq!(suggested_file_name("print 1", None, &zone()), "print-1.ktt");
    }

    #[test]
    fn a_name_has_nothing_a_file_system_dislikes() {
        let name = suggested_file_name(
            "let a = \"x/y\\\\z:*?\"; T | where `col` > 1",
            Some("2026-10-01T08:30:05Z"),
            &zone(),
        );
        assert!(
            name.chars().all(|character| character.is_alphanumeric()
                || character == '-'
                || character == '.'),
            "{name}"
        );
    }

    #[gpui::test]
    async fn copying_leaves_the_original_and_replaces_what_is_there(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/h",
            json!({ "a.ktt": "result", "kept": { "old.ktt": "older" } }),
        )
        .await;
        copy_result(
            fs.as_ref(),
            std::path::Path::new("/h/a.ktt"),
            std::path::Path::new("/h/kept/old.ktt"),
        )
        .await
        .expect("copies");
        assert_eq!(
            fs.load(std::path::Path::new("/h/kept/old.ktt"))
                .await
                .expect("there"),
            "result"
        );
        assert!(fs.is_file(std::path::Path::new("/h/a.ktt")).await);
        copy_result(
            fs.as_ref(),
            std::path::Path::new("/h/a.ktt"),
            std::path::Path::new("/h/a.ktt"),
        )
        .await
        .expect("copying a file onto itself is nothing");
    }

    #[gpui::test]
    async fn saving_the_shown_result_copies_it_to_the_chosen_name_in_the_project(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        let shown = panel
            .read_with(cx, |panel, cx| {
                panel
                    .shown_viewer()
                    .and_then(|viewer| viewer.read(cx).save_action(cx))
            })
            .expect("the shown result can be saved");
        let name = shown.suggested_name.clone().expect("a suggested name");
        assert!(name.starts_with("stormevents-20"), "{name}");
        assert!(name.ends_with(".ktt"));

        workspace.update_in(cx, |workspace, window, cx| {
            save_result(workspace, &SaveResult::default(), window, cx)
        });
        cx.run_until_parked();
        cx.simulate_new_path_selection(|folder| Some(folder.join("storm.ktt")));
        cx.run_until_parked();

        let saved = fs
            .load(std::path::Path::new("/root/storm.ktt"))
            .await
            .expect("the copy is in the project folder the prompt opened in");
        assert!(saved.contains("PrimaryResult"), "{saved}");
    }

    #[gpui::test]
    async fn cancelling_the_prompt_saves_nothing(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            save_result(workspace, &SaveResult::default(), window, cx)
        });
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        assert!(!fs.is_file(std::path::Path::new("/root/storm.ktt")).await);
    }

    #[gpui::test]
    async fn with_no_result_there_is_nothing_to_save(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        workspace.update_in(cx, |workspace, window, cx| {
            save_result(workspace, &SaveResult::default(), window, cx)
        });
        cx.run_until_parked();
        // No prompt was opened, so nothing is waiting for an answer.
        cx.update(|_, _| {});
    }
}
