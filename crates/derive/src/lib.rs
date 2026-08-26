//! Digest→prompt, llama runner, GBNF grammar, model manager.
//! Corrections retrieval lands in M5.

pub mod chat;
pub mod model;
pub mod runner;

pub use chat::ChatModel;
pub use runner::{TaskDraft, infer_tasks};
