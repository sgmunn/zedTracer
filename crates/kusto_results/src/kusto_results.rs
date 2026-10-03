//! Results of Kusto queries: the data model, the `.ktt` file format, and the logic that
//! turns a table into what a results grid and its inspector show.
//!
//! Nothing here draws anything. The grid, inspector and panels are built on top of this
//! crate, so everything in it can be tested without a window.

pub mod activity;
pub mod activity_tree;
pub mod export;
pub mod failures;
pub mod filter;
pub mod findings;
pub mod inspector;
pub mod result;
pub mod sequence;
pub mod timeline;
pub mod trace_schema;
pub mod trace_text;
pub mod typed;
pub mod view;
pub mod waterfall;

pub use result::{
    Cell, Column, ColumnKind, ColumnLayout, ExtraProperty, NoResultData, ResultSet, Table,
    TableView,
};
