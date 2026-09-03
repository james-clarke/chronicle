//! Shared llama.cpp plumbing: model load, thread count, prompt-decode
//! chunking. Used by `runner` (batch/live/consolidate), `chat`, and
//! `describe` — kept private so each module's public shape is unchanged.

use std::path::Path;

use anyhow::Context;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::token::LlamaToken;

pub(crate) const N_BATCH: u32 = 512;

/// Load a model (mmap) plus the backend it lives on. `send_logs_to_tracing`
/// must only ever run once per process.
pub(crate) fn load_model(model_path: &Path) -> anyhow::Result<(LlamaBackend, LlamaModel)> {
    static LLAMA_LOGS: std::sync::Once = std::sync::Once::new();
    LLAMA_LOGS.call_once(|| llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default()));
    let backend = LlamaBackend::init()?;
    let model_params = LlamaModelParams::default(); // mmap on by default
    let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
        .with_context(|| format!("loading model {}", model_path.display()))?;
    Ok((backend, model))
}

/// Physical cores minus one, clamped to `[1, 8]`.
pub(crate) fn threads() -> i32 {
    num_cpus::get_physical().saturating_sub(1).clamp(1, 8) as i32
}

/// Longest shared prefix of `a` and `b`.
pub(crate) fn common_prefix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Decode `tokens` into `ctx` in `N_BATCH`-sized chunks, starting at
/// `start_pos`. Returns the position after the last token, i.e. where the
/// first generated token goes.
pub(crate) fn decode_prompt(
    ctx: &mut LlamaContext,
    batch: &mut LlamaBatch,
    tokens: &[LlamaToken],
    start_pos: i32,
) -> anyhow::Result<i32> {
    let last = tokens.len() - 1;
    let mut pos = start_pos;
    let mut idx = 0usize;
    for chunk in tokens.chunks(N_BATCH as usize) {
        batch.clear();
        for tok in chunk {
            let is_last = idx == last;
            batch.add(*tok, pos, &[0], is_last)?;
            pos += 1;
            idx += 1;
        }
        ctx.decode(batch)?;
    }
    Ok(pos)
}

#[cfg(test)]
mod tests {
    #[test]
    fn common_prefix_counts_shared_head_only() {
        assert_eq!(super::common_prefix(&[1, 2, 3], &[1, 2, 4, 5]), 2);
        assert_eq!(super::common_prefix(&[1, 2], &[1, 2, 3]), 2);
        assert_eq!(super::common_prefix::<i32>(&[], &[1]), 0);
        assert_eq!(super::common_prefix(&[9], &[1]), 0);
    }
}
