//! Finds the code behind a trace event in local repositories, on demand.
//!
//! Nothing is indexed ahead of time. The keys an event offers (its marker, a `Class.Method` at
//! the start of its message, the frames of a call stack) are looked up with `git grep` when the
//! event is looked at, and what is found is kept in memory for the session. Nothing here draws
//! anything or knows about a results table.

mod keys;
mod resolve;
mod search;
mod source;

pub use keys::{CodeKeys, EventText, SymbolPath};
pub use resolve::{CodeRef, CodeRefKind, CodeResolver, KeyOrigin};
pub use search::{GitSearch, Repository, discover_repositories};
