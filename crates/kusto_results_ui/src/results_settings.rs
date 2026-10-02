use gpui::{Hsla, Rgba};
use kusto_results::Table;
use kusto_results::trace_schema::{TraceColumns, TraceSchema};
use settings::{DockSide, RegisterSetting, Settings};

/// The settings for the Kusto results grid.
#[derive(Clone, Debug, RegisterSetting)]
pub struct ResultsSettings {
    /// The row tint for severity levels 1 (critical) to 5 (verbose). `None` leaves a level
    /// without a tint.
    pub severity_tints: [Option<Hsla>; 5],
    /// The side the Row Details panel docks on.
    pub dock: DockSide,
    /// The user's trace schemas, tried in order before the built-in column names.
    pub trace_schemas: Vec<TraceSchema>,
}

impl ResultsSettings {
    /// The tint for a level from 1 to 5.
    pub fn severity_tint(&self, level: u8) -> Option<Hsla> {
        let index = usize::from(level).checked_sub(1)?;
        self.severity_tints.get(index).copied().flatten()
    }

    /// The columns of `table` that play each part of a trace.
    pub fn trace_columns(&self, table: &Table) -> TraceColumns {
        TraceColumns::resolve(table, &self.trace_schemas)
    }
}

fn trace_schema(content: &settings::KustoTraceSchemaContent) -> TraceSchema {
    TraceSchema {
        name: content.name.clone().unwrap_or_default(),
        requires: content.requires.clone().unwrap_or_default(),
        activity_id: content.activity_id.clone(),
        parent_activity_id: content.parent_activity_id.clone(),
        marker: content.marker.clone(),
        actor: content.actor.clone(),
        timestamp: content.timestamp.clone(),
        severity: content.severity.clone(),
        message: content.message.clone(),
    }
}

/// A hex colour such as `#f14c4c40`, or nothing for an empty or unreadable one.
fn parse_tint(text: &str) -> Option<Hsla> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match Rgba::try_from(text) {
        Ok(colour) => Some(colour.into()),
        Err(error) => {
            log::warn!("kusto_results severity colour {text:?} is not a colour: {error}");
            None
        }
    }
}

impl Settings for ResultsSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let colours = content
            .kusto_results
            .as_ref()
            .and_then(|results| results.severity_colors.as_ref());
        let tint = |pick: fn(&settings::KustoSeverityColorsContent) -> &Option<String>| {
            colours
                .and_then(|colours| pick(colours).as_deref())
                .and_then(parse_tint)
        };
        Self {
            dock: content
                .kusto_results
                .as_ref()
                .and_then(|results| results.dock)
                .unwrap_or(DockSide::Right),
            trace_schemas: content
                .kusto_results
                .as_ref()
                .and_then(|results| results.trace_schemas.as_deref())
                .unwrap_or_default()
                .iter()
                .map(trace_schema)
                .collect(),
            severity_tints: [
                tint(|colours| &colours.critical),
                tint(|colours| &colours.error),
                tint(|colours| &colours.warning),
                tint(|colours| &colours.normal),
                tint(|colours| &colours.verbose),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{BorrowAppContext as _, TestAppContext};
    use settings::{KustoSeverityColorsContent, SettingsStore};

    use super::*;

    #[gpui::test]
    fn the_default_settings_tint_all_five_levels(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            let settings = ResultsSettings::get_global(cx);
            for level in 1..=5 {
                assert!(settings.severity_tint(level).is_some(), "level {level}");
            }
            assert_eq!(settings.severity_tint(0), None);
            assert_eq!(settings.severity_tint(6), None);
        });
    }

    #[test]
    fn an_empty_or_unreadable_colour_leaves_a_level_untinted() {
        let mut content = settings::SettingsContent::default();
        content.kusto_results = Some(settings::KustoResultsSettingsContent {
            dock: None,
            severity_colors: Some(KustoSeverityColorsContent {
                critical: Some("#ff000080".into()),
                error: Some(String::new()),
                warning: Some("not a colour".into()),
                normal: None,
                verbose: Some("  #00ff0040  ".into()),
            }),
            trace_schemas: None,
        });
        let settings = ResultsSettings::from_settings(&content);
        assert!(settings.severity_tint(1).is_some());
        assert_eq!(settings.severity_tint(2), None);
        assert_eq!(settings.severity_tint(3), None);
        assert_eq!(settings.severity_tint(4), None);
        assert!(settings.severity_tint(5).is_some());
    }

    #[gpui::test]
    fn trace_schemas_in_the_user_settings_rename_the_columns_of_a_trace(cx: &mut TestAppContext) {
        use kusto_results::Column;
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            let table = Table {
                name: "t".into(),
                columns: ["SpanId", "ParentSpanId", "Service", "TIMESTAMP"]
                    .into_iter()
                    .map(|name| Column::new(name, "string"))
                    .collect(),
                rows: Vec::new(),
            };
            assert!(!ResultsSettings::get_global(cx).trace_columns(&table).supports_activity());

            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r#"{ "kusto_results": { "trace_schemas": [
                            { "name": "spans", "activity_id": "SpanId",
                              "parent_activity_id": "ParentSpanId", "actor": "Service" }
                        ] } }"#,
                        cx,
                    )
                    .expect("the user settings parse");
            });
            let columns = ResultsSettings::get_global(cx).trace_columns(&table);
            assert!(columns.supports_sequence());
            assert_eq!(columns.activity_id, Some(0));
            assert_eq!(columns.actor, Some(2));
        });
    }

    /// A user's `""` has to win over the default colour when the settings are merged, not only
    /// when one section is read on its own.
    #[gpui::test]
    fn an_empty_colour_in_the_user_settings_removes_that_tint(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            assert!(ResultsSettings::get_global(cx).severity_tint(2).is_some());

            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r##"{ "kusto_results": { "severity_colors": { "error": "", "warning": "#ff0000" } } }"##,
                        cx,
                    )
                    .expect("the user settings parse");
            });
            let settings = ResultsSettings::get_global(cx);
            assert_eq!(settings.severity_tint(2), None, "the empty colour removes the tint");
            assert!(settings.severity_tint(3).is_some(), "a colour replaces the default");
            assert!(
                settings.severity_tint(1).is_some(),
                "a level the user did not mention keeps its default"
            );
        });
    }
}
