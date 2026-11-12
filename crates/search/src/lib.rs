//! Excavate's search logic, independent of the UI and of Windows calls so it can be tested and measured alone:
//!
//! - [`filter`]: the kind filters (Documents, Pictures, Code…) as Everything syntax and as a local test.
//! - [`query`]: what was typed, as terms (or as Everything syntax, passed through untouched).
//! - [`hot`]: the hot tier, an in-memory list of the places people open most (apps, recent files, their folders,
//!   what they opened through Excavate) that answers in about a millisecond on every keystroke.
//! - [`rank`]: one score for every candidate, wherever it came from.
//! - [`describe`]: whether what was typed names a file or describes one.
//! - [`asked`]: a type of file asked for: named in the text ("the pdf of…"), or after `>` and `<` (">v theory").
//! - [`plan`]: the queries a keystroke sends to Everything for the long tail.
//! - [`history`]: what was opened through Excavate, for frecency.

pub mod asked;
pub mod describe;
pub mod filter;
pub mod history;
pub mod hot;
pub mod plan;
pub mod query;
pub mod rank;

pub use filter::Filter;
pub use query::Terms;
pub use rank::{Candidate, Origin, Ranked};
