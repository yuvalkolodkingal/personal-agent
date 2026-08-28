//! Privacy-preserving, generation-bound desktop context and control contracts.
//!
//! Native screen and accessibility APIs live behind [`DesktopBackend`]. The
//! coordinator in this crate validates privacy, handle freshness, authorization,
//! and observable postconditions before reporting an action as successful.

mod action;
mod backend;
mod coordinator;
mod model;
mod privacy;

pub use action::*;
pub use backend::*;
pub use coordinator::*;
pub use model::*;
pub use privacy::*;
