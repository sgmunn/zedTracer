//! The results grid and the views built around it, on top of the `kusto_results` crate.

mod activity_tree;
mod filter_popover;
mod grid;
mod history;
mod inspector_text;
mod query_parameters;
mod query_view;
mod results_panel;
mod results_settings;
mod results_viewer;
mod row_details_panel;
mod run_query;
mod save_result;
mod sequence_view;
mod structured_view;

pub use activity_tree::{ActivityTree, ActivityTreeEvent};
pub use filter_popover::{FilterChanged, FilterPopover};
pub use grid::{GridOptions, ResultGrid, ResultGridEvent};
pub use inspector_text::{InspectorPalette, InspectorText};
pub use results_panel::{ResultsPanel, ToggleResults};
pub use results_settings::ResultsSettings;
pub use results_viewer::{ResultsFile, ResultsViewer, init};
pub use row_details_panel::{ActiveSelection, RowDetailsPanel, ToggleRowDetails};
pub use run_query::{CancelQuery, CopyClientRequestId, KustoSettings, RunQuery, ShowResult};
pub use structured_view::StructuredView;
