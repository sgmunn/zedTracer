//! The query a result came from, to read and copy but not to edit: where it ran, the values its
//! parameters had, and its text, coloured as Kusto when the language is available.

use std::sync::Arc;

use editor::Editor;
use gpui::{AppContext as _, Context, Entity, IntoElement, Render, SharedString, Task, Window};
use gpui_util::ResultExt as _;
use kusto_results::ResultSet;
use language::LanguageRegistry;
use language::language_settings::SoftWrap;
use ui::prelude::*;

use crate::results_panel::summarize;

pub(crate) struct QueryView {
    editor: Entity<Editor>,
    summary: SharedString,
    parameters: Vec<(String, String)>,
    _language: Task<()>,
}

/// The values of a result's parameters as text, in name order.
pub(crate) fn parameter_values(result: &ResultSet) -> Vec<(String, String)> {
    let Some(parameters) = &result.parameters else {
        return Vec::new();
    };
    let mut values: Vec<(String, String)> = parameters
        .iter()
        .map(|(name, value)| {
            let text = match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            (name.clone(), text)
        })
        .collect();
    values.sort();
    values
}

impl QueryView {
    pub(crate) fn new(
        result: &ResultSet,
        languages: Option<Arc<LanguageRegistry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = result.query.clone().unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            editor.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
            editor.set_text(query, window, cx);
            editor.set_read_only(true);
            editor
        });
        let language = cx.spawn(async move |this, cx| {
            let Some(languages) = languages else {
                return;
            };
            let Some(language) = languages.language_for_name("Kusto").await.log_err() else {
                return;
            };
            this.update(cx, |this, cx| {
                let buffer = this.editor.read(cx).buffer().read(cx).as_singleton();
                if let Some(buffer) = buffer {
                    buffer.update(cx, |buffer, cx| buffer.set_language(Some(language), cx));
                }
            })
            .log_err();
        });
        Self {
            editor,
            summary: summarize(result).into(),
            parameters: parameter_values(result),
            _language: language,
        }
    }

    #[cfg(test)]
    pub(crate) fn parameters(&self) -> &[(String, String)] {
        &self.parameters
    }

    #[cfg(test)]
    pub(crate) fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }
}

impl Render for QueryView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_0p5()
                    .child(
                        Label::new(self.summary.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .children(self.parameters.iter().map(|(name, value)| {
                        Label::new(format!("{name} = {value}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                    })),
            )
            .child(div().flex_1().min_h_0().child(self.editor.clone()))
    }
}
