//! Digest→prompt, llama runner, GBNF grammar, model manager.
//! Corrections retrieval lands in M5.

pub mod model;
pub mod runner;

pub use runner::{TaskDraft, infer_tasks};
