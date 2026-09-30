use gpui::{Hsla, Rgba};
use settings::{RegisterSetting, Settings};

/// The settings for the Kusto results grid.
#[derive(Clone, Debug, Default, RegisterSetting)]
pub struct ResultsSettings {
    /// The row tint for severity levels 1 (critical) to 5 (verbose). `None` leaves a level
    /// without a tint.
    pub severity_tints: [Option<Hsla>; 5],
}

impl ResultsSettings {
    /// The tint for a level from 1 to 5.
    pub fn severity_tint(&self, level: u8) -> Option<Hsla> {
        let index = usize::from(level).checked_sub(1)?;
        self.severity_tints.get(index).copied().flatten()
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
    use gpui::TestAppContext;
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
            severity_colors: Some(KustoSeverityColorsContent {
                critical: Some("#ff000080".into()),
                error: Some(String::new()),
                warning: Some("not a colour".into()),
                normal: None,
                verbose: Some("  #00ff0040  ".into()),
            }),
        });
        let settings = ResultsSettings::from_settings(&content);
        assert!(settings.severity_tint(1).is_some());
        assert_eq!(settings.severity_tint(2), None);
        assert_eq!(settings.severity_tint(3), None);
        assert_eq!(settings.severity_tint(4), None);
        assert!(settings.severity_tint(5).is_some());
    }
}
