//! The results grid and the views built around it, on top of the `kusto_results` crate.

mod filter_popover;
mod grid;
mod inspector_text;
mod results_settings;
mod results_viewer;
mod row_details_panel;

pub use filter_popover::{FilterChanged, FilterPopover};
pub use grid::{GridOptions, ResultGrid, ResultGridEvent};
pub use inspector_text::{InspectorPalette, InspectorText};
pub use results_settings::ResultsSettings;
pub use results_viewer::{ResultsFile, ResultsViewer, init};
pub use row_details_panel::{ActiveSelection, RowDetailsPanel, ToggleRowDetails};
