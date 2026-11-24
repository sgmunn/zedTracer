#![allow(missing_docs)]

use anyhow::Context as _;
use collections::IndexMap;
use gpui::{HighlightStyle, Hsla};
use palette::FromColor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::IntoGpui;
pub use settings::{
    FontStyleContent, HighlightStyleContent, StatusColorsContent, ThemeColor, ThemeColorsContent,
    ThemeStyleContent,
};
pub use settings::{FontWeightContent, WindowBackgroundContent};

use theme::{StatusColorsRefinement, ThemeColorsRefinement};

const LIGHT_DIFF_HUNK_FILLED_OPACITY: f32 = 0.16;
const LIGHT_DIFF_HUNK_HOLLOW_BACKGROUND_OPACITY: f32 = 0.08;
const LIGHT_DIFF_HUNK_HOLLOW_BORDER_OPACITY: f32 = 0.48;
const DARK_DIFF_HUNK_FILLED_OPACITY: f32 = 0.12;
const DARK_DIFF_HUNK_HOLLOW_BACKGROUND_OPACITY: f32 = 0.06;
const DARK_DIFF_HUNK_HOLLOW_BORDER_OPACITY: f32 = 0.36;

/// The content of a serialized theme family.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThemeFamilyContent {
    pub name: String,
    pub author: String,
    pub themes: Vec<ThemeContent>,
}

/// The content of a serialized theme.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThemeContent {
    pub name: String,
    pub appearance: theme::AppearanceContent,
    pub style: settings::ThemeStyleContent,
}

/// Returns the syntax style overrides in the [`ThemeContent`].
pub fn syntax_overrides(this: &settings::ThemeStyleContent) -> Vec<(String, HighlightStyle)> {
    this.syntax
        .iter()
        .map(|(key, style)| {
            (
                key.clone(),
                HighlightStyle {
                    color: style
                        .color
                        .as_ref()
                        .and_then(|color| resolve_color(color, &this.palette).ok()),
                    background_color: style
                        .background_color
                        .as_ref()
                        .and_then(|color| resolve_color(color, &this.palette).ok()),
                    font_style: style.font_style.map(|s| s.into_gpui()),
                    font_weight: style.font_weight.map(|w| w.into_gpui()),
                    ..Default::default()
                },
            )
        })
        .collect()
}

pub fn status_colors_refinement(
    colors: &settings::StatusColorsContent,
    palette: &IndexMap<String, ThemeColor>,
) -> StatusColorsRefinement {
    StatusColorsRefinement {
        conflict: colors
            .conflict
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        conflict_background: colors
            .conflict_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        conflict_border: colors
            .conflict_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        created: colors
            .created
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        created_background: colors
            .created_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        created_border: colors
            .created_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        deleted: colors
            .deleted
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        deleted_background: colors
            .deleted_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        deleted_border: colors
            .deleted_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        error: colors
            .error
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        error_background: colors
            .error_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        error_border: colors
            .error_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        hidden: colors
            .hidden
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        hidden_background: colors
            .hidden_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        hidden_border: colors
            .hidden_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        hint: colors
            .hint
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        hint_background: colors
            .hint_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        hint_border: colors
            .hint_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ignored: colors
            .ignored
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ignored_background: colors
            .ignored_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ignored_border: colors
            .ignored_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        info: colors
            .info
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        info_background: colors
            .info_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        info_border: colors
            .info_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        modified: colors
            .modified
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        modified_background: colors
            .modified_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        modified_border: colors
            .modified_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        predictive: colors
            .predictive
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        predictive_background: colors
            .predictive_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        predictive_border: colors
            .predictive_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        renamed: colors
            .renamed
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        renamed_background: colors
            .renamed_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        renamed_border: colors
            .renamed_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        success: colors
            .success
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        success_background: colors
            .success_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        success_border: colors
            .success_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        unreachable: colors
            .unreachable
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        unreachable_background: colors
            .unreachable_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        unreachable_border: colors
            .unreachable_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        warning: colors
            .warning
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        warning_background: colors
            .warning_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        warning_border: colors
            .warning_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
    }
}

pub fn theme_colors_refinement(
    this: &settings::ThemeColorsContent,
    status_colors: &StatusColorsRefinement,
    is_light: bool,
    palette: &IndexMap<String, ThemeColor>,
) -> ThemeColorsRefinement {
    let border = this
        .border
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let editor_document_highlight_read_background = this
        .editor_document_highlight_read_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let scrollbar_thumb_background = this
        .scrollbar_thumb_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok())
        .or_else(|| {
            this.deprecated_scrollbar_thumb_background
                .as_ref()
                .and_then(|color| resolve_color(color, palette).ok())
        });
    let scrollbar_thumb_hover_background = this
        .scrollbar_thumb_hover_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let scrollbar_thumb_active_background = this
        .scrollbar_thumb_active_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok())
        .or(scrollbar_thumb_background);
    let scrollbar_thumb_border = this
        .scrollbar_thumb_border
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let element_hover = this
        .element_hover
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let panel_background = this
        .panel_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let search_match_background = this
        .search_match_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok());
    let search_active_match_background = this
        .search_active_match_background
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok())
        .or(search_match_background);
    let version_control_added = this
        .version_control_added
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok())
        .or(status_colors.created);
    let version_control_deleted = this
        .version_control_deleted
        .as_ref()
        .and_then(|color| resolve_color(color, palette).ok())
        .or(status_colors.deleted);
    let (hunk_fill, hunk_hollow_bg, hunk_hollow_border) = if is_light {
        (
            LIGHT_DIFF_HUNK_FILLED_OPACITY,
            LIGHT_DIFF_HUNK_HOLLOW_BACKGROUND_OPACITY,
            LIGHT_DIFF_HUNK_HOLLOW_BORDER_OPACITY,
        )
    } else {
        (
            DARK_DIFF_HUNK_FILLED_OPACITY,
            DARK_DIFF_HUNK_HOLLOW_BACKGROUND_OPACITY,
            DARK_DIFF_HUNK_HOLLOW_BORDER_OPACITY,
        )
    };
    ThemeColorsRefinement {
        border,
        border_variant: this
            .border_variant
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        border_focused: this
            .border_focused
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        border_selected: this
            .border_selected
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        border_transparent: this
            .border_transparent
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        border_disabled: this
            .border_disabled
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        elevated_surface_background: this
            .elevated_surface_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        surface_background: this
            .surface_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        background: this
            .background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        element_background: this
            .element_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        element_hover,
        element_active: this
            .element_active
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        element_selected: this
            .element_selected
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        element_disabled: this
            .element_disabled
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        element_selection_background: this
            .element_selection_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        drop_target_background: this
            .drop_target_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        drop_target_border: this
            .drop_target_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ghost_element_background: this
            .ghost_element_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ghost_element_hover: this
            .ghost_element_hover
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ghost_element_active: this
            .ghost_element_active
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ghost_element_selected: this
            .ghost_element_selected
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        ghost_element_disabled: this
            .ghost_element_disabled
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        text: this
            .text
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        text_muted: this
            .text_muted
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        text_placeholder: this
            .text_placeholder
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        text_disabled: this
            .text_disabled
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        text_accent: this
            .text_accent
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        icon: this
            .icon
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        icon_muted: this
            .icon_muted
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        icon_disabled: this
            .icon_disabled
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        icon_placeholder: this
            .icon_placeholder
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        icon_accent: this
            .icon_accent
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        debugger_accent: this
            .debugger_accent
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        status_bar_background: this
            .status_bar_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        title_bar_background: this
            .title_bar_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        title_bar_inactive_background: this
            .title_bar_inactive_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        toolbar_background: this
            .toolbar_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        tab_bar_background: this
            .tab_bar_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        tab_inactive_background: this
            .tab_inactive_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        tab_active_background: this
            .tab_active_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        search_match_background,
        search_active_match_background,
        panel_background,
        panel_focused_border: this
            .panel_focused_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        panel_indent_guide: this
            .panel_indent_guide
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        panel_indent_guide_hover: this
            .panel_indent_guide_hover
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        panel_indent_guide_active: this
            .panel_indent_guide_active
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        panel_overlay_background: this
            .panel_overlay_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(panel_background.map(ensure_opaque)),
        panel_overlay_hover: this
            .panel_overlay_hover
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(panel_background
                .zip(element_hover)
                .map(|(panel_bg, hover_bg)| panel_bg.blend(hover_bg))
                .map(ensure_opaque)),
        pane_focused_border: this
            .pane_focused_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        pane_group_border: this
            .pane_group_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(border),
        scrollbar_thumb_background,
        scrollbar_thumb_hover_background,
        scrollbar_thumb_active_background,
        scrollbar_thumb_border,
        scrollbar_track_background: this
            .scrollbar_track_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        scrollbar_track_border: this
            .scrollbar_track_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        minimap_thumb_background: this
            .minimap_thumb_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(scrollbar_thumb_background.map(ensure_non_opaque)),
        minimap_thumb_hover_background: this
            .minimap_thumb_hover_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(scrollbar_thumb_hover_background.map(ensure_non_opaque)),
        minimap_thumb_active_background: this
            .minimap_thumb_active_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(scrollbar_thumb_active_background.map(ensure_non_opaque)),
        minimap_thumb_border: this
            .minimap_thumb_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(scrollbar_thumb_border),
        editor_foreground: this
            .editor_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_code_lens_foreground: this
            .editor_code_lens_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_background: this
            .editor_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_gutter_background: this
            .editor_gutter_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_subheader_background: this
            .editor_subheader_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_active_line_background: this
            .editor_active_line_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_highlighted_line_background: this
            .editor_highlighted_line_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_debugger_active_line_background: this
            .editor_debugger_active_line_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_line_number: this
            .editor_line_number
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_hover_line_number: this
            .editor_hover_line_number
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_active_line_number: this
            .editor_active_line_number
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_invisible: this
            .editor_invisible
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_wrap_guide: this
            .editor_wrap_guide
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_active_wrap_guide: this
            .editor_active_wrap_guide
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_indent_guide: this
            .editor_indent_guide
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_indent_guide_active: this
            .editor_indent_guide_active
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_document_highlight_read_background,
        editor_document_highlight_write_background: this
            .editor_document_highlight_write_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        editor_document_highlight_bracket_background: this
            .editor_document_highlight_bracket_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(editor_document_highlight_read_background),
        editor_diff_hunk_added_background: this
            .editor_diff_hunk_added_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or_else(|| version_control_added.map(|c| c.opacity(hunk_fill))),
        editor_diff_hunk_added_hollow_background: this
            .editor_diff_hunk_added_hollow_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or_else(|| version_control_added.map(|c| c.opacity(hunk_hollow_bg))),
        editor_diff_hunk_added_hollow_border: this
            .editor_diff_hunk_added_hollow_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or_else(|| version_control_added.map(|c| c.opacity(hunk_hollow_border))),
        editor_diff_hunk_deleted_background: this
            .editor_diff_hunk_deleted_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or_else(|| version_control_deleted.map(|c| c.opacity(hunk_fill))),
        editor_diff_hunk_deleted_hollow_background: this
            .editor_diff_hunk_deleted_hollow_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or_else(|| version_control_deleted.map(|c| c.opacity(hunk_hollow_bg))),
        editor_diff_hunk_deleted_hollow_border: this
            .editor_diff_hunk_deleted_hollow_border
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or_else(|| version_control_deleted.map(|c| c.opacity(hunk_hollow_border))),
        terminal_background: this
            .terminal_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_background: this
            .terminal_ansi_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_foreground: this
            .terminal_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_bright_foreground: this
            .terminal_bright_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_dim_foreground: this
            .terminal_dim_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_black: this
            .terminal_ansi_black
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_black: this
            .terminal_ansi_bright_black
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_black: this
            .terminal_ansi_dim_black
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_red: this
            .terminal_ansi_red
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_red: this
            .terminal_ansi_bright_red
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_red: this
            .terminal_ansi_dim_red
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_green: this
            .terminal_ansi_green
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_green: this
            .terminal_ansi_bright_green
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_green: this
            .terminal_ansi_dim_green
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_yellow: this
            .terminal_ansi_yellow
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_yellow: this
            .terminal_ansi_bright_yellow
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_yellow: this
            .terminal_ansi_dim_yellow
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_blue: this
            .terminal_ansi_blue
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_blue: this
            .terminal_ansi_bright_blue
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_blue: this
            .terminal_ansi_dim_blue
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_magenta: this
            .terminal_ansi_magenta
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_magenta: this
            .terminal_ansi_bright_magenta
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_magenta: this
            .terminal_ansi_dim_magenta
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_cyan: this
            .terminal_ansi_cyan
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_cyan: this
            .terminal_ansi_bright_cyan
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_cyan: this
            .terminal_ansi_dim_cyan
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_white: this
            .terminal_ansi_white
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_bright_white: this
            .terminal_ansi_bright_white
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        terminal_ansi_dim_white: this
            .terminal_ansi_dim_white
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        link_text_hover: this
            .link_text_hover
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        version_control_added,
        version_control_deleted,
        version_control_modified: this
            .version_control_modified
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(status_colors.modified),
        version_control_renamed: this
            .version_control_renamed
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(status_colors.modified),
        version_control_conflict: this
            .version_control_conflict
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(status_colors.ignored),
        version_control_ignored: this
            .version_control_ignored
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(status_colors.ignored),
        version_control_word_added: this
            .version_control_word_added
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        version_control_word_deleted: this
            .version_control_word_deleted
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        #[allow(deprecated)]
        version_control_conflict_marker_ours: this
            .version_control_conflict_marker_ours
            .as_ref()
            .or(this.version_control_conflict_ours_background.as_ref())
            .and_then(|color| resolve_color(color, palette).ok()),
        #[allow(deprecated)]
        version_control_conflict_marker_theirs: this
            .version_control_conflict_marker_theirs
            .as_ref()
            .or(this.version_control_conflict_theirs_background.as_ref())
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_normal_background: this
            .vim_normal_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_insert_background: this
            .vim_insert_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_replace_background: this
            .vim_replace_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_visual_background: this
            .vim_visual_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_visual_line_background: this
            .vim_visual_line_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_visual_block_background: this
            .vim_visual_block_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_yank_background: this
            .vim_yank_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(editor_document_highlight_read_background),
        vim_helix_jump_label_foreground: this
            .vim_helix_jump_label_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok())
            .or(status_colors.error),
        vim_helix_normal_background: this
            .vim_helix_normal_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_helix_select_background: this
            .vim_helix_select_background
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_normal_foreground: this
            .vim_normal_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_insert_foreground: this
            .vim_insert_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_replace_foreground: this
            .vim_replace_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_visual_foreground: this
            .vim_visual_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_visual_line_foreground: this
            .vim_visual_line_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_visual_block_foreground: this
            .vim_visual_block_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_helix_normal_foreground: this
            .vim_helix_normal_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
        vim_helix_select_foreground: this
            .vim_helix_select_foreground
            .as_ref()
            .and_then(|color| resolve_color(color, palette).ok()),
    }
}

fn ensure_non_opaque(color: Hsla) -> Hsla {
    const MAXIMUM_OPACITY: f32 = 0.7;
    if color.a <= MAXIMUM_OPACITY {
        color
    } else {
        Hsla {
            a: MAXIMUM_OPACITY,
            ..color
        }
    }
}

fn ensure_opaque(color: Hsla) -> Hsla {
    Hsla { a: 1.0, ..color }
}

/// Converts a gradient from a theme or settings override into a GPUI background.
///
/// Returns `None` when either color fails to parse, so an invalid gradient never replaces a
/// valid one.
pub fn window_gradient_from_content(
    content: &settings::WindowGradientContent,
    palette: &IndexMap<String, ThemeColor>,
) -> Option<gpui::Background> {
    let from = resolve_color(&content.from, palette).ok()?;
    let to = resolve_color(&content.to, palette).ok()?;
    Some(gpui::linear_gradient(
        content.angle,
        gpui::linear_color_stop(from, 0.),
        gpui::linear_color_stop(to, 1.),
    ))
}

/// Parses a theme color, looking up `@name` references in the theme style's palette.
pub fn resolve_color(color: &str, palette: &IndexMap<String, ThemeColor>) -> anyhow::Result<Hsla> {
    match color.strip_prefix('@') {
        Some(name) => {
            let palette_color = palette
                .get(name)
                .with_context(|| format!("theme palette has no color named {name:?}"))?;
            try_parse_color(palette_color)
                .with_context(|| format!("invalid color for theme palette entry {name:?}"))
        }
        None => try_parse_color(color),
    }
}

fn try_parse_color(color: &str) -> anyhow::Result<Hsla> {
    let rgba = gpui::Rgba::try_from(color)?;
    let rgba = palette::rgb::Srgba::from_components((rgba.r, rgba.g, rgba.b, rgba.a));
    let hsla = palette::Hsla::from_color(rgba);

    let hsla = gpui::hsla(
        hsla.hue.into_positive_degrees() / 360.,
        hsla.saturation,
        hsla.lightness,
        hsla.alpha,
    );

    Ok(hsla)
}

#[cfg(test)]
mod tests {
    use collections::IndexMap;
    use theme::StatusColorsRefinement;

    use super::{
        StatusColorsContent, ThemeColor, ThemeColorsContent, ThemeStyleContent, resolve_color,
        status_colors_refinement, syntax_overrides, theme_colors_refinement, try_parse_color,
        window_gradient_from_content,
    };

    #[test]
    fn explicit_diff_hunk_colors_take_precedence_over_fallbacks() {
        let mut colors = ThemeColorsContent::default();
        colors.editor_diff_hunk_added_background = Some("#112233".into());
        colors.editor_diff_hunk_added_hollow_background = Some("#223344".into());
        colors.editor_diff_hunk_added_hollow_border = Some("#334455".into());
        colors.editor_diff_hunk_deleted_background = Some("#445566".into());
        colors.editor_diff_hunk_deleted_hollow_background = Some("#556677".into());
        colors.editor_diff_hunk_deleted_hollow_border = Some("#667788".into());
        colors.version_control_added = Some("#00ff00".into());
        colors.version_control_deleted = Some("#ff0000".into());

        let refinement = theme_colors_refinement(
            &colors,
            &status_colors_refinement(&StatusColorsContent::default(), &IndexMap::default()),
            true,
            &IndexMap::default(),
        );

        assert_eq!(
            refinement.editor_diff_hunk_added_background,
            Some(parse_color("#112233"))
        );
        assert_eq!(
            refinement.editor_diff_hunk_added_hollow_background,
            Some(parse_color("#223344"))
        );
        assert_eq!(
            refinement.editor_diff_hunk_added_hollow_border,
            Some(parse_color("#334455"))
        );
        assert_eq!(
            refinement.editor_diff_hunk_deleted_background,
            Some(parse_color("#445566"))
        );
        assert_eq!(
            refinement.editor_diff_hunk_deleted_hollow_background,
            Some(parse_color("#556677"))
        );
        assert_eq!(
            refinement.editor_diff_hunk_deleted_hollow_border,
            Some(parse_color("#667788"))
        );
    }

    #[test]
    fn diff_hunk_colors_fallback_to_version_control_colors() {
        let mut colors = ThemeColorsContent::default();
        colors.version_control_added = Some("#00ff00".into());
        colors.version_control_deleted = Some("#ff0000".into());

        let refinement = theme_colors_refinement(
            &colors,
            &status_colors_refinement(&StatusColorsContent::default(), &IndexMap::default()),
            true,
            &IndexMap::default(),
        );

        let added = parse_color("#00ff00");
        let deleted = parse_color("#ff0000");

        assert_eq!(
            refinement.editor_diff_hunk_added_background,
            Some(added.opacity(0.16))
        );
        assert_eq!(
            refinement.editor_diff_hunk_added_hollow_background,
            Some(added.opacity(0.08))
        );
        assert_eq!(
            refinement.editor_diff_hunk_added_hollow_border,
            Some(added.opacity(0.48))
        );
        assert_eq!(
            refinement.editor_diff_hunk_deleted_background,
            Some(deleted.opacity(0.16))
        );
        assert_eq!(
            refinement.editor_diff_hunk_deleted_hollow_background,
            Some(deleted.opacity(0.08))
        );
        assert_eq!(
            refinement.editor_diff_hunk_deleted_hollow_border,
            Some(deleted.opacity(0.48))
        );
    }

    #[test]
    fn diff_hunk_opacity_fallbacks_use_correct_values_for_light_and_dark_themes() {
        let mut colors = ThemeColorsContent::default();
        colors.version_control_added = Some("#00ff00".into());

        let light_refinement = theme_colors_refinement(
            &colors,
            &status_colors_refinement(&StatusColorsContent::default(), &IndexMap::default()),
            true,
            &IndexMap::default(),
        );
        let dark_refinement = theme_colors_refinement(
            &colors,
            &status_colors_refinement(&StatusColorsContent::default(), &IndexMap::default()),
            false,
            &IndexMap::default(),
        );

        let added = parse_color("#00ff00");

        assert_eq!(
            light_refinement.editor_diff_hunk_added_background,
            Some(added.opacity(0.16))
        );
        assert_eq!(
            light_refinement.editor_diff_hunk_added_hollow_background,
            Some(added.opacity(0.08))
        );
        assert_eq!(
            light_refinement.editor_diff_hunk_added_hollow_border,
            Some(added.opacity(0.48))
        );

        assert_eq!(
            dark_refinement.editor_diff_hunk_added_background,
            Some(added.opacity(0.12))
        );
        assert_eq!(
            dark_refinement.editor_diff_hunk_added_hollow_background,
            Some(added.opacity(0.06))
        );
        assert_eq!(
            dark_refinement.editor_diff_hunk_added_hollow_border,
            Some(added.opacity(0.36))
        );
    }

    #[test]
    fn diff_hunk_fallbacks_are_absent_when_status_and_version_control_colors_are_missing() {
        let refinement = theme_colors_refinement(
            &ThemeColorsContent::default(),
            &status_colors_refinement(&StatusColorsContent::default(), &IndexMap::default()),
            true,
            &IndexMap::default(),
        );

        assert_eq!(refinement.editor_diff_hunk_added_background, None);
        assert_eq!(refinement.editor_diff_hunk_added_hollow_background, None);
        assert_eq!(refinement.editor_diff_hunk_added_hollow_border, None);
        assert_eq!(refinement.editor_diff_hunk_deleted_background, None);
        assert_eq!(refinement.editor_diff_hunk_deleted_hollow_background, None);
        assert_eq!(refinement.editor_diff_hunk_deleted_hollow_border, None);
    }

    fn parse_color(color: &str) -> gpui::Hsla {
        match try_parse_color(color) {
            Ok(color) => color,
            Err(error) => panic!("failed to parse color {color}: {error}"),
        }
    }

    #[test]
    fn helix_jump_label_color_uses_theme_color_or_status_error() {
        let status_error = try_parse_color("#e63333").expect("valid color");
        let override_color = try_parse_color("#00ff00").expect("valid color");
        let status_colors = StatusColorsRefinement {
            error: Some(status_error),
            ..Default::default()
        };

        let fallback_refinement = theme_colors_refinement(
            &ThemeColorsContent::default(),
            &status_colors,
            Default::default(),
            &IndexMap::default(),
        );

        assert_eq!(
            fallback_refinement.vim_helix_jump_label_foreground,
            Some(status_error)
        );

        let override_refinement = theme_colors_refinement(
            &ThemeColorsContent {
                vim_helix_jump_label_foreground: Some("#00ff00".into()),
                ..Default::default()
            },
            &status_colors,
            Default::default(),
            &IndexMap::default(),
        );

        assert_eq!(
            override_refinement.vim_helix_jump_label_foreground,
            Some(override_color)
        );
    }

    #[test]
    fn window_gradient_is_built_from_parsed_colors() {
        let content = settings::WindowGradientContent {
            angle: 135.,
            from: "#0a0f2e".into(),
            to: "#2a1050cc".into(),
        };

        let expected = gpui::linear_gradient(
            135.,
            gpui::linear_color_stop(try_parse_color("#0a0f2e").unwrap(), 0.),
            gpui::linear_color_stop(try_parse_color("#2a1050cc").unwrap(), 1.),
        );

        assert_eq!(
            window_gradient_from_content(&content, &IndexMap::default()),
            Some(expected)
        );
    }

    #[test]
    fn window_gradient_with_an_invalid_color_is_rejected() {
        let invalid_from = settings::WindowGradientContent {
            angle: 0.,
            from: "not a color".into(),
            to: "#2a1050".into(),
        };
        let invalid_to = settings::WindowGradientContent {
            angle: 0.,
            from: "#0a0f2e".into(),
            to: "not a color".into(),
        };

        assert_eq!(
            window_gradient_from_content(&invalid_from, &IndexMap::default()),
            None
        );
        assert_eq!(
            window_gradient_from_content(&invalid_to, &IndexMap::default()),
            None
        );
    }

    #[test]
    fn theme_style_reads_window_gradient_from_json() {
        let style: ThemeStyleContent = serde_json::from_value(serde_json::json!({
            "background.gradient": { "angle": 160, "from": "#0a0f2e", "to": "#2a1050" },
        }))
        .unwrap();

        assert_eq!(
            style.window_gradient,
            Some(settings::WindowGradientContent {
                angle: 160.,
                from: "#0a0f2e".into(),
                to: "#2a1050".into(),
            })
        );
        assert_eq!(
            ThemeStyleContent::default().window_gradient,
            None,
            "a theme without the key has no gradient"
        );
    }

    fn palette(entries: &[(&str, &str)]) -> IndexMap<String, ThemeColor> {
        entries
            .iter()
            .map(|(name, color)| (name.to_string(), ThemeColor::from(*color)))
            .collect()
    }

    #[test]
    fn palette_references_resolve_to_palette_colors() {
        let palette = palette(&[("sky", "#8cd7ff"), ("shade", "#00000050")]);

        assert_eq!(
            resolve_color("@sky", &palette).ok(),
            Some(parse_color("#8cd7ff"))
        );
        assert_eq!(
            resolve_color("@shade", &palette).ok(),
            Some(parse_color("#00000050"))
        );
        assert_eq!(
            resolve_color("#123456", &palette).ok(),
            Some(parse_color("#123456"))
        );
    }

    #[test]
    fn unresolvable_palette_references_are_rejected() {
        let palette = palette(&[("sky", "#8cd7ff"), ("alias", "@sky"), ("broken", "blue")]);

        assert!(resolve_color("@missing", &palette).is_err());
        assert!(
            resolve_color("@alias", &palette).is_err(),
            "palette entries cannot reference other palette entries"
        );
        assert!(resolve_color("@broken", &palette).is_err());
    }

    #[test]
    fn theme_style_palette_applies_to_every_color_kind() {
        let style: ThemeStyleContent = serde_json::from_value(serde_json::json!({
            "palette": { "sky": "#8cd7ff", "night": "#19212a", "coral": "#d1545a" },
            "background.gradient": { "angle": 130, "from": "@sky", "to": "@night" },
            "border": "@sky",
            "error": "@coral",
            "syntax": { "keyword": { "color": "@coral", "background_color": "@night" } },
        }))
        .unwrap();

        let status = status_colors_refinement(&style.status, &style.palette);
        assert_eq!(status.error, Some(parse_color("#d1545a")));

        let colors = theme_colors_refinement(&style.colors, &status, false, &style.palette);
        assert_eq!(colors.border, Some(parse_color("#8cd7ff")));

        let gradient = style
            .window_gradient
            .as_ref()
            .and_then(|gradient| window_gradient_from_content(gradient, &style.palette));
        let expected_gradient = gpui::linear_gradient(
            130.,
            gpui::linear_color_stop(parse_color("#8cd7ff"), 0.),
            gpui::linear_color_stop(parse_color("#19212a"), 1.),
        );
        assert_eq!(gradient, Some(expected_gradient));

        let syntax = syntax_overrides(&style);
        let (name, keyword) = &syntax[0];
        assert_eq!(name, "keyword");
        assert_eq!(keyword.color, Some(parse_color("#d1545a")));
        assert_eq!(keyword.background_color, Some(parse_color("#19212a")));
    }
}
