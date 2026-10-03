//! The findings of a trace as a strip above its views: one line while closed, a list when open.
//! Choosing a finding tells the viewer, which selects what it points at.

use std::sync::Arc;

use gpui::{Context, EventEmitter, IntoElement, Render, SharedString, Window};
use kusto_results::findings::{Finding, FindingSeverity, Findings};
use ui::prelude::*;

/// How many findings the open strip lists before saying how many more there are.
const LISTED: usize = 12;

pub(crate) struct FindingsStrip {
    findings: Arc<Findings>,
    expanded: bool,
    /// Something to say about the last choice, such as that its rows are hidden.
    note: Option<SharedString>,
}

pub(crate) enum FindingsStripEvent {
    /// The finding at this position in the list was chosen.
    Chosen(usize),
}

impl EventEmitter<FindingsStripEvent> for FindingsStrip {}

impl FindingsStrip {
    pub(crate) fn new(findings: Arc<Findings>) -> Self {
        Self {
            findings,
            expanded: false,
            note: None,
        }
    }

    pub(crate) fn finding(&self, index: usize) -> Option<&Finding> {
        self.findings.items.get(index)
    }

    pub(crate) fn set_note(&mut self, note: Option<SharedString>, cx: &mut Context<Self>) {
        self.note = note;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn is_expanded(&self) -> bool {
        self.expanded
    }

    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.findings.items.len()
    }

    #[cfg(test)]
    pub(crate) fn note(&self) -> Option<&SharedString> {
        self.note.as_ref()
    }

    fn summary(&self) -> String {
        let count = |severity: FindingSeverity| {
            self.findings
                .items
                .iter()
                .filter(|finding| finding.severity == severity)
                .count()
        };
        let mut parts = Vec::new();
        for (severity, one, many) in [
            (FindingSeverity::Error, "error", "errors"),
            (FindingSeverity::Warning, "warning", "warnings"),
            (FindingSeverity::Information, "note", "notes"),
        ] {
            match count(severity) {
                0 => {}
                1 => parts.push(format!("1 {one}")),
                found => parts.push(format!("{found} {many}")),
            }
        }
        format!("Findings: {}", parts.join(", "))
    }
}

fn severity_icon(severity: FindingSeverity) -> (IconName, Color) {
    match severity {
        FindingSeverity::Error => (IconName::XCircle, Color::Error),
        FindingSeverity::Warning => (IconName::Warning, Color::Warning),
        FindingSeverity::Information => (IconName::Info, Color::Muted),
    }
}

impl Render for FindingsStrip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().colors().border;
        let hover = cx.theme().colors().ghost_element_hover;
        let expanded = self.expanded;
        let headline = self
            .findings
            .items
            .first()
            .map(|finding| SharedString::from(finding.title.clone()));

        let header = h_flex()
            .id("findings-strip-header")
            .debug_selector(|| "findings-strip-header".to_string())
            .gap_2()
            .px_2()
            .py_1()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(|this, _, _, cx| {
                this.expanded = !this.expanded;
                cx.notify();
            }))
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::Small)
                .color(Color::Muted),
            )
            .child(Label::new(self.summary()).size(LabelSize::Small))
            .when(!expanded, |header| {
                header.children(headline.map(|headline| {
                    div().flex_1().min_w_0().overflow_hidden().child(
                        Label::new(headline)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                }))
            });

        let list = expanded.then(|| {
            let rows = self
                .findings
                .items
                .iter()
                .enumerate()
                .take(LISTED)
                .map(|(index, finding)| {
                    let (icon, color) = severity_icon(finding.severity);
                    h_flex()
                        .id(("finding", index))
                        .debug_selector(move || format!("finding-{index}"))
                        .items_start()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(FindingsStripEvent::Chosen(index));
                        }))
                        .child(Icon::new(icon).size(IconSize::Small).color(color))
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(Label::new(finding.title.clone()).size(LabelSize::Small))
                                .children(finding.detail.clone().map(|detail| {
                                    Label::new(detail)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted)
                                })),
                        )
                });
            let more = self.findings.items.len().saturating_sub(LISTED);
            v_flex()
                .id("findings-strip-list")
                .max_h(px(260.))
                .overflow_y_scroll()
                .children(rows)
                .when(more > 0, |list| {
                    list.child(
                        div().px_3().py_1().child(
                            Label::new(format!("{more} more"))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                    )
                })
        });

        v_flex()
            .border_b_1()
            .border_color(border)
            .child(header)
            .children(list)
            .children(self.note.clone().map(|note| {
                div().px_3().pb_1().child(
                    Label::new(note)
                        .size(LabelSize::XSmall)
                        .color(Color::Warning),
                )
            }))
    }
}
