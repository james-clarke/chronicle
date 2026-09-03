//! Digest → grammar-constrained task JSON via embedded llama.cpp.
//! The GBNF grammar is the containment layer for untrusted title/MCP text:
//! whatever the digest contains, the model can only emit task-schema JSON.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::Context;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

const GRAMMAR: &str = include_str!("../../../grammars/task_output_v4.gbnf");
const PROMPT: &str = include_str!("../../../prompts/derive_v4.txt");

const N_CTX: u32 = 4096;
const N_BATCH: u32 = 512;
// 8 intervals × ~35 tokens under the v4 keys, with room for long labels; the 300 tokens freed from v3's 900 went to digest::MAX_TOKENS.
const MAX_GEN: usize = 600;

pub use chronicle_core::types::{DeriveOutput, IntervalDraft};

/// Raw model intervals; callers sanitize + link (chronicle_core::merge).
pub fn infer_intervals(model_path: &Path, digest: &str) -> anyhow::Result<Vec<IntervalDraft>> {
    // llama.cpp/ggml log via a C callback straight to stderr unless redirected;
    // send_logs_to_tracing must only ever run once per process (bench loops).
    static LLAMA_LOGS: std::sync::Once = std::sync::Once::new();
    LLAMA_LOGS.call_once(|| llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default()));
    let backend = LlamaBackend::init()?;
    let model_params = LlamaModelParams::default(); // mmap on by default
    let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
        .with_context(|| format!("loading model {}", model_path.display()))?;

    let threads = num_cpus::get_physical().saturating_sub(1).clamp(1, 8) as i32;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(N_CTX))
        .with_n_batch(N_BATCH)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let mut ctx = model.new_context(&backend, ctx_params)?;

    let limit = N_CTX as usize - MAX_GEN - 64;
    let tokens = crate::fit_prompt(digest, limit, |d| tokenize_prompt(&model, d))?;
    tracing::info!(
        tokens = tokens.len(),
        limit,
        digest_chars = digest.len(),
        "derive prompt"
    );

    // Prompt eval, N_BATCH tokens per decode; logits only for the last token.
    let mut batch = LlamaBatch::new(N_BATCH as usize, 1);
    let last = tokens.len() - 1;
    let mut pos = 0i32;
    for chunk in tokens.chunks(N_BATCH as usize) {
        batch.clear();
        for tok in chunk {
            let is_last = pos as usize == last;
            batch.add(*tok, pos, &[0], is_last)?;
            pos += 1;
        }
        ctx.decode(&mut batch)?;
    }

    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::grammar(&model, GRAMMAR, "root").context("compiling task grammar")?,
        LlamaSampler::greedy(),
    ]);

    let mut out = String::new();
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    for _ in 0..MAX_GEN {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        if model.is_eog_token(token) {
            break;
        }
        out.push_str(&model.token_to_piece(token, &mut decoder, false, None)?);
        batch.clear();
        batch.add(token, pos, &[0], true)?;
        pos += 1;
        ctx.decode(&mut batch)?;
    }

    let parsed: DeriveOutput = serde_json::from_str(&out)
        .with_context(|| format!("model output is not valid interval JSON: {out}"))?;
    Ok(parsed.intervals)
}

fn tokenize_prompt(
    model: &LlamaModel,
    digest: &str,
) -> anyhow::Result<Vec<llama_cpp_2::token::LlamaToken>> {
    let content = PROMPT.replace("{digest}", digest);
    let tmpl = model
        .chat_template(None)
        .context("model has no embedded chat template")?;
    let text = model.apply_chat_template(
        &tmpl,
        &[LlamaChatMessage::new("user".into(), content)?],
        true,
    )?;
    // Never AddBos: the rendered template already carries its special tokens.
    Ok(model.str_to_token(&text, AddBos::Never)?)
}
