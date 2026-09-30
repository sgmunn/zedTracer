//! The popover opened by a column header's funnel: an operator choice and a value.

use std::rc::Rc;

use editor::{Editor, EditorEvent};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, Window, div, rems,
};
use kusto_results::ColumnKind;
use kusto_results::filter::{ColumnFilter, Condition, FilterOperator, Join, operators_for};
use ui::{ContextMenu, DropdownMenu, prelude::*};

/// Called with the column's new filter, or `None` when it has none.
pub type FilterChanged = Rc<dyn Fn(Option<ColumnFilter>, &mut App)>;

pub struct FilterPopover {
    focus_handle: FocusHandle,
    title: SharedString,
    kind: ColumnKind,
    operator: FilterOperator,
    value_editor: Entity<Editor>,
    operator_menu: Entity<ContextMenu>,
    on_change: FilterChanged,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for FilterPopover {}

impl Focusable for FilterPopover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
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
        let first = existing.and_then(|filter| filter.conditions.first());
        let operator = first.map_or_else(
            || {
                operators_for(kind)
                    .first()
                    .map_or(FilterOperator::Contains, |option| option.operator)
            },
            |condition| condition.operator,
        );
        let initial_value = first
            .map(|condition| condition.value.clone())
            .unwrap_or_default();

        let value_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(initial_value, window, cx);
            editor
        });
        let subscription = cx.subscribe(&value_editor, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                this.changed(cx);
            }
        });

        let popover = cx.weak_entity();
        let operator_menu = ContextMenu::build(window, cx, |mut menu, _, _| {
            for option in operators_for(kind) {
                let popover = popover.clone();
                menu = menu.entry(option.label, None, move |_, cx| {
                    popover
                        .update(cx, |this, cx| {
                            this.operator = option.operator;
                            this.changed(cx);
                        })
                        .ok();
                });
            }
            menu
        });

        let focus_handle = cx.focus_handle();
        window.focus(&value_editor.focus_handle(cx), cx);
        Self {
            focus_handle,
            title: format!("Filter {column_name}").into(),
            kind,
            operator,
            value_editor,
            operator_menu,
            on_change,
            _subscriptions: vec![subscription],
        }
    }

    fn current_filter(&self, cx: &App) -> Option<ColumnFilter> {
        let value = self.value_editor.read(cx).text(cx);
        ColumnFilter {
            join: Join::All,
            conditions: vec![Condition::new(self.operator, value)],
        }
        .usable(self.kind)
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        (self.on_change)(self.current_filter(cx), cx);
        cx.notify();
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.value_editor
            .update(cx, |editor, cx| editor.set_text("", window, cx));
        (self.on_change)(None, cx);
        cx.emit(DismissEvent);
    }
}

impl Render for FilterPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let operator_label = operators_for(self.kind)
            .into_iter()
            .find(|option| option.operator == self.operator)
            .map_or("", |option| option.label);
        let colors = cx.theme().colors();
        v_flex()
            .id("filter-popover")
            .debug_selector(|| "filter-popover".to_string())
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            // PopoverMenu dismisses only when its own trigger is clicked; dismissing on a click
            // anywhere else is the popover's job.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .w(rems(20.))
            .p_2()
            .gap_2()
            .elevation_2(cx)
            .child(Label::new(self.title.clone()))
            .child(DropdownMenu::new(
                "filter-operator",
                operator_label,
                self.operator_menu.clone(),
            ))
            .child(
                div()
                    .debug_selector(|| "filter-value".to_string())
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(colors.border)
                    .rounded_sm()
                    .child(self.value_editor.clone()),
            )
            .child(
                h_flex().justify_end().child(
                    div().debug_selector(|| "filter-clear".to_string()).child(
                        Button::new("filter-clear", "Clear")
                            .on_click(cx.listener(|this, _, window, cx| this.clear(window, cx))),
                    ),
                ),
            )
    }
}
