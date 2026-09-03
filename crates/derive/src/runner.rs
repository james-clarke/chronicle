//! Digest → grammar-constrained task JSON via embedded llama.cpp.
//! The GBNF grammar is the containment layer for untrusted title/MCP text:
//! whatever the digest contains, the model can only emit task-schema JSON.
//!
//! m27 chunk 3: `DeriveModel` loads once per process and `DeriveSession`
//! keeps one context whose KV cache holds the tokens of the last prompt and
//! answer. Each request decodes only what differs from the cached tokens —
//! in practice the digest and the template tail, since the instruction
//! prefix is identical every time.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::Context;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;

const GRAMMAR: &str = include_str!("../../../grammars/task_output_v4.gbnf");
const PROMPT: &str = include_str!("../../../prompts/derive_v4.txt");

const N_CTX: u32 = 4096;
const N_BATCH: u32 = 512;
// 8 intervals × ~35 tokens under the v4 keys, with room for long labels; the 300 tokens freed from v3's 900 went to digest::MAX_TOKENS.
const MAX_GEN: usize = 600;

pub use chronicle_core::types::{DeriveOutput, IntervalDraft};

/// One derive call: the parsed intervals plus what it cost (m27 chunk 2
/// instrumentation; `raw` feeds the inspector's "last output").
#[derive(Debug)]
pub struct DeriveRun {
    pub intervals: Vec<IntervalDraft>,
    pub raw: String,
    pub prompt_tokens: usize,
    /// Prompt tokens already in the KV cache from the previous request (the
    /// instruction prefix, ~1000 tokens, on every call after the first).
    pub cached_prefix_tokens: usize,
    pub gen_tokens: usize,
    pub prompt_eval_ms: u64,
    pub gen_ms: u64,
}

/// A loaded model (mmap) plus the backend it lives on.
pub struct DeriveModel {
    backend: LlamaBackend,
    model: LlamaModel,
}

impl DeriveModel {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        // llama.cpp/ggml log via a C callback straight to stderr unless
        // redirected; send_logs_to_tracing must only ever run once per process.
        static LLAMA_LOGS: std::sync::Once = std::sync::Once::new();
        LLAMA_LOGS
            .call_once(|| llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default()));
        let backend = LlamaBackend::init()?;
        let model_params = LlamaModelParams::default(); // mmap on by default
        let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
            .with_context(|| format!("loading model {}", model_path.display()))?;
        Ok(Self { backend, model })
    }

    /// A context that keeps its KV cache between requests.
    pub fn session(&self) -> anyhow::Result<DeriveSession<'_>> {
        let threads = num_cpus::get_physical().saturating_sub(1).clamp(1, 8) as i32;
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let ctx = self.model.new_context(&self.backend, ctx_params)?;
        Ok(DeriveSession {
            model: &self.model,
            ctx,
            batch: LlamaBatch::new(N_BATCH as usize, 1),
            cached: Vec::new(),
        })
    }
}

pub struct DeriveSession<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    batch: LlamaBatch<'m>,
    /// Tokens whose KV entries are resident at positions `0..len`: the last
    /// prompt followed by what the model generated for it.
    cached: Vec<LlamaToken>,
}

impl DeriveSession<'_> {
    /// Derive one digest, streaming generated text through `on_token`.
    /// Raw model intervals; callers sanitize + link (chronicle_core::merge).
    pub fn infer(
        &mut self,
        digest: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<DeriveRun> {
        let limit = N_CTX as usize - MAX_GEN - 64;
        let tokens = crate::fit_prompt(digest, limit, |d| tokenize_prompt(self.model, d))?;

        // Reuse the cached prefix; the last prompt token is always decoded
        // again so its logits exist for the first sample.
        let keep = common_prefix(&self.cached, &tokens).min(tokens.len() - 1);
        self.ctx
            .clear_kv_cache_seq(Some(0), Some(keep as u32), None)
            .context("clearing kv cache")?;
        self.cached.truncate(keep);

        let t_prompt = std::time::Instant::now();
        let last = tokens.len() - 1;
        let mut pos = keep as i32;
        for chunk in tokens[keep..].chunks(N_BATCH as usize) {
            self.batch.clear();
            for tok in chunk {
                let is_last = pos as usize == last;
                self.batch.add(*tok, pos, &[0], is_last)?;
                pos += 1;
            }
            self.ctx.decode(&mut self.batch)?;
        }
        self.cached.extend_from_slice(&tokens[keep..]);
        let prompt_eval_ms = t_prompt.elapsed().as_millis() as u64;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::grammar(self.model, GRAMMAR, "root").context("compiling task grammar")?,
            LlamaSampler::greedy(),
        ]);

        let t_gen = std::time::Instant::now();
        let mut out = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut gen_tokens = 0usize;
        for _ in 0..MAX_GEN {
            let token = sampler.sample(&self.ctx, self.batch.n_tokens() - 1);
            if self.model.is_eog_token(token) {
                break;
            }
            gen_tokens += 1;
            let piece = self
                .model
                .token_to_piece(token, &mut decoder, false, None)?;
            on_token(&piece);
            out.push_str(&piece);
            self.batch.clear();
            self.batch.add(token, pos, &[0], true)?;
            pos += 1;
            self.ctx.decode(&mut self.batch)?;
            self.cached.push(token);
        }
        let gen_ms = t_gen.elapsed().as_millis() as u64;
        tracing::info!(
            prompt_tokens = tokens.len(),
            cached_prefix_tokens = keep,
            limit,
            digest_chars = digest.len(),
            gen_tokens,
            prompt_eval_ms,
            gen_ms,
            "derive run"
        );

        let parsed: DeriveOutput = serde_json::from_str(&out)
            .with_context(|| format!("model output is not valid interval JSON: {out}"))?;
        Ok(DeriveRun {
            intervals: parsed.intervals,
            raw: out,
            prompt_tokens: tokens.len(),
            cached_prefix_tokens: keep,
            gen_tokens,
            prompt_eval_ms,
            gen_ms,
        })
    }
}

/// One-shot derive: load, one session, one request. Bench and replay hold a
/// session per model instead so the prefix cache carries across cases.
pub fn infer_intervals(model_path: &Path, digest: &str) -> anyhow::Result<DeriveRun> {
    let model = DeriveModel::load(model_path)?;
    let mut session = model.session()?;
    session.infer(digest, &mut |_| {})
}

fn common_prefix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn tokenize_prompt(model: &LlamaModel, digest: &str) -> anyhow::Result<Vec<LlamaToken>> {
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
