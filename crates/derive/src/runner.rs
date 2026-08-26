//! Digest → grammar-constrained task JSON via embedded llama.cpp.
//! The GBNF grammar is the containment layer for untrusted title/MCP text:
//! whatever the digest contains, the model can only emit task-schema JSON.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::{Context, bail};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

const GRAMMAR: &str = include_str!("../../../grammars/task_output.gbnf");
const PROMPT: &str = include_str!("../../../prompts/derive_v1.txt");

const N_CTX: u32 = 4096;
const N_BATCH: u32 = 512;
const MAX_GEN: usize = 900;

#[derive(Debug, serde::Deserialize)]
pub struct DeriveOutput {
    pub tasks: Vec<TaskDraft>,
}

/// Offsets are minutes from the start of the digest window.
#[derive(Debug, serde::Deserialize)]
pub struct TaskDraft {
    pub label: String,
    pub project: Option<String>,
    pub start_offset_min: i64,
    pub end_offset_min: i64,
    pub confidence: f64,
}

pub fn infer_tasks(model_path: &Path, digest: &str) -> anyhow::Result<Vec<TaskDraft>> {
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
    let mut tokens = tokenize_prompt(&model, digest)?;
    if tokens.len() > limit {
        // Digest cap is a chars/4 heuristic; when the real tokenizer disagrees,
        // shrink the digest proportionally and retokenize once.
        let keep = digest.len() * limit / tokens.len();
        let mut cut = keep.min(digest.len());
        while !digest.is_char_boundary(cut) {
            cut -= 1;
        }
        tokens = tokenize_prompt(&model, &digest[..cut])?;
        if tokens.len() > limit {
            bail!(
                "prompt still too long after truncation: {} tokens",
                tokens.len()
            );
        }
    }

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
        .with_context(|| format!("model output is not valid task JSON: {out}"))?;
    Ok(parsed.tasks)
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
