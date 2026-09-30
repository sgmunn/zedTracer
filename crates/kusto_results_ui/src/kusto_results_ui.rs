//! The results grid and the views built around it, on top of the `kusto_results` crate.

mod filter_popover;
mod grid;
mod inspector_text;
mod row_details_panel;

pub use filter_popover::{FilterChanged, FilterPopover};
pub use grid::{ResultGrid, ResultGridEvent};
pub use inspector_text::{InspectorPalette, InspectorText};
pub use row_details_panel::{ActiveSelection, RowDetailsPanel, ToggleRowDetails};
