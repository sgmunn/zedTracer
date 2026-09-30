//! The popover opened by a column header's funnel: up to two conditions, each an operator and a
//! value, and whether all or any of them must match.

use std::rc::Rc;

use editor::{Editor, EditorEvent};
use gpui::{
    Anchor, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, WeakEntity, Window, div, rems,
};
use gpui_util::ResultExt as _;
use kusto_results::ColumnKind;
use kusto_results::filter::{ColumnFilter, Condition, FilterOperator, Join, operators_for};
use ui::{ContextMenu, DropdownMenu, PopoverMenuHandle, prelude::*};

/// Called with the column's new filter, or `None` when it has none.
pub type FilterChanged = Rc<dyn Fn(Option<ColumnFilter>, &mut App)>;

const MAXIMUM_CONDITIONS: usize = 2;

struct ConditionRow {
    operator: FilterOperator,
    value_editor: Entity<Editor>,
    operator_menu: Entity<ContextMenu>,
    operator_handle: PopoverMenuHandle<ContextMenu>,
    _subscription: Subscription,
}

pub struct FilterPopover {
    focus_handle: FocusHandle,
    title: SharedString,
    kind: ColumnKind,
    rows: Vec<ConditionRow>,
    join: Join,
    join_menu: Entity<ContextMenu>,
    join_handle: PopoverMenuHandle<ContextMenu>,
    on_change: FilterChanged,
}

impl EventEmitter<DismissEvent> for FilterPopover {}

impl Focusable for FilterPopover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn default_operator(kind: ColumnKind) -> FilterOperator {
    operators_for(kind)
        .first()
        .map_or(FilterOperator::Contains, |option| option.operator)
}

fn requires_value(kind: ColumnKind, operator: FilterOperator) -> bool {
    operators_for(kind)
        .iter()
        .find(|option| option.operator == operator)
        .is_none_or(|option| option.requires_value)
}

impl FilterPopover {
    pub fn new(
        column_name: &str,
        kind: ColumnKind,
        existing: Option<&ColumnFilter>,
        on_change: FilterChanged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let join = existing.map_or(Join::All, |filter| filter.join);
        let popover = cx.weak_entity();
        let join_menu = ContextMenu::build(window, cx, |menu, _, _| {
            [
                (Join::All, "Match all conditions"),
                (Join::Any, "Match any condition"),
            ]
            .into_iter()
            .fold(menu, |menu, (join, label)| {
                let popover = popover.clone();
                menu.entry(label, None, move |_, cx| {
                    popover
                        .update(cx, |this, cx| {
                            this.join = join;
                            this.changed(cx);
                        })
                        .log_err();
                })
            })
        });

        let mut popover = Self {
            focus_handle: cx.focus_handle(),
            title: format!("Filter {column_name}").into(),
            kind,
            rows: Vec::new(),
            join,
            join_menu,
            join_handle: PopoverMenuHandle::default(),
            on_change,
        };
        let conditions = existing
            .map(|filter| filter.conditions.clone())
            .filter(|conditions| !conditions.is_empty())
            .unwrap_or_else(|| vec![Condition::new(default_operator(kind), "")]);
        for condition in conditions.into_iter().take(MAXIMUM_CONDITIONS) {
            popover.push_row(condition.operator, &condition.value, window, cx);
        }
        popover.refresh_placeholders(window, cx);
        if let Some(first) = popover.rows.first() {
            window.focus(&first.value_editor.focus_handle(cx), cx);
        }
        popover
    }

    fn push_row(
        &mut self,
        operator: FilterOperator,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(value, window, cx);
            editor
        });
        let subscription = cx.subscribe(&value_editor, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                this.changed(cx);
            }
        });
        let popover = cx.weak_entity();
        let index = self.rows.len();
        let operator_menu = Self::operator_menu(self.kind, index, popover, window, cx);
        self.rows.push(ConditionRow {
            operator,
            value_editor,
            operator_menu,
            operator_handle: PopoverMenuHandle::default(),
            _subscription: subscription,
        });
    }

    fn operator_menu(
        kind: ColumnKind,
        index: usize,
        popover: WeakEntity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ContextMenu> {
        ContextMenu::build(window, cx, |mut menu, _, _| {
            for option in operators_for(kind) {
                let popover = popover.clone();
                menu = menu.entry(option.label, None, move |_, cx| {
                    popover
                        .update(cx, |this, cx| {
                            if let Some(row) = this.rows.get_mut(index) {
                                row.operator = option.operator;
                            }
                            this.changed(cx);
                        })
                        .log_err();
                });
            }
            menu
        })
    }

    fn refresh_placeholders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own = match self.kind {
            ColumnKind::DateTime => Some("ISO date/time"),
            ColumnKind::TimeSpan => Some("d.hh:mm:ss"),
            _ => None,
        };
        let several = self.rows.len() > 1;
        for (index, row) in self.rows.iter().enumerate() {
            let placeholder = match (index, several, own) {
                (0, true, _) => "First value",
                (1, true, _) => "Second value",
                (_, _, Some(own)) => own,
                _ => "Value",
            };
            row.value_editor.update(cx, |editor, cx| {
                editor.set_placeholder_text(placeholder, window, cx)
            });
        }
    }

    /// A dropdown's list is drawn outside the popover, so a click on it is also a click outside
    /// the popover, which must not dismiss it.
    fn a_menu_is_open(&self) -> bool {
        self.join_handle.is_deployed()
            || self
                .rows
                .iter()
                .any(|row| row.operator_handle.is_deployed())
    }

    fn current_filter(&self, cx: &App) -> Option<ColumnFilter> {
        ColumnFilter {
            join: self.join,
            conditions: self
                .rows
                .iter()
                .map(|row| Condition::new(row.operator, row.value_editor.read(cx).text(cx)))
                .collect(),
        }
        .usable(self.kind)
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        (self.on_change)(self.current_filter(cx), cx);
        cx.notify();
    }

    fn add_condition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.len() >= MAXIMUM_CONDITIONS {
            return;
        }
        self.push_row(default_operator(self.kind), "", window, cx);
        self.refresh_placeholders(window, cx);
        if let Some(row) = self.rows.last() {
            window.focus(&row.value_editor.focus_handle(cx), cx);
        }
        self.changed(cx);
    }

    fn remove_condition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.len() < MAXIMUM_CONDITIONS {
            return;
        }
        self.rows.pop();
        self.refresh_placeholders(window, cx);
        self.changed(cx);
    }

    fn clear(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        (self.on_change)(None, cx);
        cx.emit(DismissEvent);
    }
}

impl Render for FilterPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let kind = self.kind;
        let several = self.rows.len() > 1;
        let join_label = match self.join {
            Join::All => "Match all conditions",
            Join::Any => "Match any condition",
        };
        let condition_rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let operator_label = operators_for(kind)
                    .into_iter()
                    .find(|option| option.operator == row.operator)
                    .map_or("", |option| option.label);
                v_flex()
                    .gap_1()
                    .child(
                        DropdownMenu::new(
                            ("filter-operator", index),
                            operator_label,
                            row.operator_menu.clone(),
                        )
                        .handle(row.operator_handle.clone())
                        .attach(Anchor::BottomLeft),
                    )
                    .when(requires_value(kind, row.operator), |column| {
                        column.child(
                            div()
                                .debug_selector(|| {
                                    if index == 0 {
                                        "filter-value".to_string()
                                    } else {
                                        format!("filter-value-{index}")
                                    }
                                })
                                .px_2()
                                .py_1()
                                .border_1()
                                .border_color(colors.border)
                                .rounded_sm()
                                .child(row.value_editor.clone()),
                        )
                    })
            })
            .collect();
        v_flex()
            .id("filter-popover")
            .debug_selector(|| "filter-popover".to_string())
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            // PopoverMenu dismisses only when its own trigger is clicked; dismissing on a click
            // anywhere else is the popover's job.
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if !this.a_menu_is_open() {
                    cx.emit(DismissEvent)
                }
            }))
            .w(rems(20.))
            .p_2()
            .gap_2()
            .elevation_2(cx)
            .child(Label::new(self.title.clone()))
            .children(condition_rows.into_iter().enumerate().map(|(index, row)| {
                v_flex()
                    .gap_2()
                    .when(index == 1, |column| {
                        column.child(
                            h_flex().child(
                                div().debug_selector(|| "filter-join".to_string()).child(
                                    DropdownMenu::new(
                                        "filter-join",
                                        join_label,
                                        self.join_menu.clone(),
                                    )
                                    .handle(self.join_handle.clone())
                                    .attach(Anchor::BottomLeft),
                                ),
                            ),
                        )
                    })
                    .child(row)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(!several, |buttons| {
                        buttons.child(div().debug_selector(|| "filter-add".to_string()).child(
                            Button::new("filter-add", "Add condition").on_click(
                                cx.listener(|this, _, window, cx| this.add_condition(window, cx)),
                            ),
                        ))
                    })
                    .when(several, |buttons| {
                        buttons.child(div().debug_selector(|| "filter-remove".to_string()).child(
                            Button::new("filter-remove", "Remove condition").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.remove_condition(window, cx)
                                }),
                            ),
                        ))
                    })
                    .child(
                        div().debug_selector(|| "filter-clear".to_string()).child(
                            Button::new("filter-clear", "Clear").on_click(
                                cx.listener(|this, _, window, cx| this.clear(window, cx)),
                            ),
                        ),
                    ),
            )
    }
}
