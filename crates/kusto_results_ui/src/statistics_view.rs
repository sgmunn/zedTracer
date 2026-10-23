//! The Stats tab: what a query run cost the service, read from the statistics the result file
//! keeps, with the payload itself to read as JSON.

use editor::Editor;
use gpui::{AppContext as _, Context, Entity, IntoElement, Render, SharedString, Window};
use kusto_results::ResultSet;
use kusto_results::statistics::{ResourceUsage, format_bytes};
use language::language_settings::SoftWrap;
use ui::prelude::*;

/// A box of figures under a title.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Card {
    pub title: &'static str,
    pub figures: Vec<(SharedString, SharedString)>,
}

/// The cards of one cluster. The first section is the cluster the query was sent to and has no
/// heading; each other cluster the query reached follows under its name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Section {
    pub heading: Option<SharedString>,
    pub cards: Vec<Card>,
}

/// The cards the Stats tab shows. A figure the service did not send is left out, and so is a
/// card with no figures.
pub(crate) fn statistics_sections(result: &ResultSet) -> Vec<Section> {
    let Some(statistics) = result.query_statistics() else {
        return Vec::new();
    };
    let mut cards = Vec::new();

    let mut time = Vec::new();
    if let Some(seconds) = statistics.execution_seconds {
        time.push(figure("Execution time", format!("{seconds:.3} s")));
    }
    if let Some(milliseconds) = result.execution_duration_ms {
        time.push(figure(
            "Round trip",
            format!("{:.3} s", milliseconds as f64 / 1000.0),
        ));
    }
    if let Some(started) = &result.execution_started_at {
        time.push(figure("Started", started.clone()));
    }
    cards.push(Card {
        title: "Time",
        figures: time,
    });

    cards.extend(usage_cards(&statistics.usage));

    let mut result_figures = Vec::new();
    let several_tables = statistics.result_tables.len() > 1;
    for (position, table) in statistics.result_tables.iter().enumerate() {
        let name = |what: &str| {
            if several_tables {
                format!("Table {} {what}", position + 1)
            } else {
                capitalized(what)
            }
        };
        result_figures.push(figure(name("rows"), table.rows.to_string()));
        result_figures.push(figure(name("size"), format_bytes(table.bytes)));
    }
    cards.push(Card {
        title: "Result",
        figures: result_figures,
    });

    let mut scan = Vec::new();
    if let Some(extents) = statistics.extents {
        scan.push(figure(
            "Extents scanned",
            format!("{} of {}", extents.scanned, extents.total),
        ));
    }
    if let Some(rows) = statistics.rows {
        scan.push(figure(
            "Rows scanned",
            format!("{} of {}", rows.scanned, rows.total),
        ));
    }
    cards.push(Card {
        title: "Scan",
        figures: scan,
    });

    let mut general = Vec::new();
    if let Some(request_id) = &result.client_request_id {
        general.push(figure("Client request ID", request_id.clone()));
    }
    if let Some(hash) = &statistics.query_hash {
        general.push(figure("Query hash", hash.clone()));
    }
    cards.push(Card {
        title: "General",
        figures: general,
    });

    let mut sections = vec![Section {
        heading: None,
        cards,
    }];
    sections.extend(
        statistics
            .cross_cluster
            .iter()
            .map(|(cluster, usage)| Section {
                heading: Some(cluster.trim_end_matches('/').to_string().into()),
                cards: usage_cards(usage).collect(),
            }),
    );
    for section in &mut sections {
        section.cards.retain(|card| !card.figures.is_empty());
    }
    sections.retain(|section| !section.cards.is_empty());
    sections
}

fn usage_cards(usage: &ResourceUsage) -> impl Iterator<Item = Card> {
    let text = |label: &str, value: &Option<String>| {
        value.as_ref().map(|value| figure(label, value.clone()))
    };
    let bytes =
        |label: &str, value: Option<u64>| value.map(|value| figure(label, format_bytes(value)));

    let cpu = [
        text("Total", &usage.cpu_total),
        text("Kernel", &usage.cpu_kernel),
        text("User", &usage.cpu_user),
        text("Query execution", &usage.cpu_execution),
        text("Query planning", &usage.cpu_planning),
    ];
    let memory = [bytes("Peak per node", usage.memory_peak_per_node)];
    let cache = [
        bytes("Hot hits", usage.hot_cache.map(|cache| cache.hit)),
        bytes("Hot misses", usage.hot_cache.map(|cache| cache.miss)),
        bytes("Cold hits", usage.cold_cache.map(|cache| cache.hit)),
        bytes("Cold misses", usage.cold_cache.map(|cache| cache.miss)),
        bytes(
            "Retrieved",
            match (usage.hot_cache, usage.cold_cache) {
                (None, None) => None,
                (hot, cold) => Some(
                    hot.map_or(0, |cache| cache.retrieve) + cold.map_or(0, |cache| cache.retrieve),
                ),
            },
        ),
        bytes("Bypassed", usage.cache_bypass_bytes),
    ];
    let network = [
        bytes("Inter-cluster", usage.inter_cluster_bytes),
        bytes("Cross-cluster", usage.cross_cluster_bytes),
    ];
    [
        ("CPU", cpu.into_iter().flatten().collect()),
        ("Memory", memory.into_iter().flatten().collect()),
        ("Disk cache", cache.into_iter().flatten().collect()),
        ("Network", network.into_iter().flatten().collect()),
    ]
    .into_iter()
    .map(|(title, figures)| Card { title, figures })
}

fn figure(label: impl Into<String>, value: impl Into<String>) -> (SharedString, SharedString) {
    (label.into().into(), value.into().into())
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

pub(crate) struct StatisticsView {
    sections: Vec<Section>,
    /// The payload as the service sent it, shown instead of the cards while `show_json` is on.
    json_editor: Entity<Editor>,
    show_json: bool,
}

impl StatisticsView {
    pub(crate) fn new(result: &ResultSet, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let json = result
            .statistics
            .as_ref()
            .and_then(|statistics| serde_json::to_string_pretty(statistics).ok())
            .unwrap_or_default();
        let json_editor = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            editor.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
            editor.set_text(json, window, cx);
            editor.set_read_only(true);
            editor
        });
        Self {
            sections: statistics_sections(result),
            json_editor,
            show_json: false,
        }
    }

    fn render_card(card: &Card, cx: &App) -> impl IntoElement {
        v_flex()
            .min_w(px(220.))
            .p_3()
            .gap_2()
            .border_1()
            .rounded_md()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().surface_background)
            .child(
                Label::new(card.title)
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .items_start()
                    .gap_x_4()
                    .gap_y_2()
                    .children(card.figures.iter().map(|(label, value)| {
                        v_flex()
                            .pl_2()
                            .border_l_2()
                            .border_color(cx.theme().colors().border_focused)
                            .child(
                                Label::new(label.clone())
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(Label::new(value.clone()))
                    })),
            )
    }
}

impl Render for StatisticsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_json = self.show_json;
        let toggle = h_flex().px_3().pt_2().child(
            div()
                .debug_selector(|| "statistics-json".to_string())
                .child(
                    Button::new("statistics-json", "Raw JSON")
                        .toggle_state(show_json)
                        .tooltip(ui::Tooltip::text(
                            "Show the statistics exactly as the service sent them",
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_json = !this.show_json;
                            cx.notify();
                        })),
                ),
        );
        let body =
            if show_json {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.json_editor.clone())
                    .into_any_element()
            } else {
                v_flex()
                    .id("statistics-cards")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_4()
                    .children(self.sections.iter().map(|section| {
                        v_flex()
                            .gap_2()
                            .when_some(section.heading.clone(), |this, heading| {
                                this.child(Label::new(heading).color(Color::Muted))
                            })
                            .child(h_flex().flex_wrap().items_start().gap_3().children(
                                section.cards.iter().map(|card| Self::render_card(card, cx)),
                            ))
                    }))
                    .into_any_element()
            };
        v_flex().size_full().child(toggle).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULT: &str = r#"{"tables":[], "executionStartedAt": "2026-10-23T15:51:00.000Z",
      "executionDurationMs": 28994, "clientRequestId": "Kusto.Zed;b8f5f",
      "queryStatistics": {
        "QueryHash": "be3cfb5d5a8bdc89",
        "ExecutionTime": 28.8199127,
        "resource_usage": {
          "cache": {"shards": {"hot": {"hitbytes": 0, "missbytes": 0, "retrievebytes": 0},
                               "cold": {"hitbytes": 0, "missbytes": 0, "retrievebytes": 0},
                               "bypassbytes": 0}},
          "cpu": {"user": "00:00:00.0156250", "kernel": "00:00:00", "total cpu": "00:00:00.0156250",
                  "breakdown": {"query execution": "00:00:00", "query planning": "00:00:00.0156250"}},
          "memory": {"peak_per_node": 1050944},
          "network": {"inter_cluster_total_bytes": 693, "cross_cluster_total_bytes": 3251}
        },
        "input_dataset_statistics": {"extents": {"total": 10, "scanned": 2}, "rows": {"total": 100, "scanned": 20}},
        "dataset_statistics": [{"table_row_count": 3, "table_size": 54}],
        "cross_cluster_resource_usage": {
          "https://other.kusto.windows.net/": {
            "cpu": {"total cpu": "00:18:02.2812500"},
            "memory": {"peak_per_node": 2442367072}
          }
        }
      }}"#;

    fn figures(card: &Card) -> Vec<(&str, &str)> {
        card.figures
            .iter()
            .map(|(label, value)| (label.as_ref(), value.as_ref()))
            .collect()
    }

    #[test]
    fn the_cards_show_the_figures_of_each_cluster() {
        let result = ResultSet::from_json(RESULT).unwrap();
        let sections = statistics_sections(&result);
        assert_eq!(sections.len(), 2);

        let titles: Vec<&str> = sections[0].cards.iter().map(|card| card.title).collect();
        assert_eq!(
            titles,
            [
                "Time",
                "CPU",
                "Memory",
                "Disk cache",
                "Network",
                "Result",
                "Scan",
                "General"
            ]
        );
        let card = |title| {
            sections[0]
                .cards
                .iter()
                .find(|card| card.title == title)
                .unwrap()
        };
        assert_eq!(
            figures(card("Time")),
            [
                ("Execution time", "28.820 s"),
                ("Round trip", "28.994 s"),
                ("Started", "2026-10-23T15:51:00.000Z")
            ]
        );
        assert_eq!(figures(card("Memory")), [("Peak per node", "1.0 MB")]);
        assert_eq!(
            figures(card("Result")),
            [("Rows", "3"), ("Size", "54 bytes")]
        );
        assert_eq!(
            figures(card("Scan")),
            [
                ("Extents scanned", "2 of 10"),
                ("Rows scanned", "20 of 100")
            ]
        );
        assert_eq!(
            figures(card("General")),
            [
                ("Client request ID", "Kusto.Zed;b8f5f"),
                ("Query hash", "be3cfb5d5a8bdc89")
            ]
        );

        let other = &sections[1];
        assert_eq!(
            other.heading.as_deref(),
            Some("https://other.kusto.windows.net")
        );
        let titles: Vec<&str> = other.cards.iter().map(|card| card.title).collect();
        assert_eq!(
            titles,
            ["CPU", "Memory"],
            "a card the service sent nothing for is left out"
        );
        assert_eq!(figures(&other.cards[1]), [("Peak per node", "2.3 GB")]);
    }

    #[test]
    fn several_result_tables_are_numbered() {
        let result = ResultSet::from_json(&RESULT.replace(
            r#"[{"table_row_count": 3, "table_size": 54}]"#,
            r#"[{"table_row_count": 3, "table_size": 54}, {"table_row_count": 1, "table_size": 2048}]"#,
        ))
        .unwrap();
        let sections = statistics_sections(&result);
        let result_card = sections[0]
            .cards
            .iter()
            .find(|card| card.title == "Result")
            .unwrap();
        assert_eq!(
            figures(result_card),
            [
                ("Table 1 rows", "3"),
                ("Table 1 size", "54 bytes"),
                ("Table 2 rows", "1"),
                ("Table 2 size", "2.0 KB")
            ]
        );
    }

    #[test]
    fn a_result_without_statistics_has_no_cards() {
        let result = ResultSet::from_json(r#"{"tables":[], "executionDurationMs": 5}"#).unwrap();
        assert!(statistics_sections(&result).is_empty());
    }
}
