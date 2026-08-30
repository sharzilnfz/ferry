//! Self-healing `FolderEngine` encapsulating store opening, polynomial validation,
//! background worker tasks, exponential backoff crash recovery, and direct
//! `UiEvent` broadcast streaming.

pub use crate::supervisor::engine::*;
