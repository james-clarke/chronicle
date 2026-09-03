//! Digest → grammar-constrained task JSON via embedded llama.cpp.
//! The GBNF grammar is the containment layer for untrusted title/MCP text:
//! whatever the digest contains, the model can only emit task-schema JSON.
//!
//! m27 chunk 3: `DeriveModel` loads once per process and `DeriveSession`
//! keeps one context whose KV cache holds the tokens of the last prompt and
//! answer. Each request decodes only what differs from the cached tokens —
//! in practice the digest and the template tail, since the instruction
//! prefix is identical every time. Chunk 4 adds the live prompt; a worker
//! holds one session per prompt so the two prefixes do not evict each other.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::Context;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;

use crate::backend::{self, N_BATCH};

const BATCH_GRAMMAR: &str = include_str!("../../../grammars/task_output_v4.gbnf");
const BATCH_PROMPT: &str = include_str!("../../../prompts/derive_v5.txt");
const LIVE_GRAMMAR: &str = include_str!("../../../grammars/live_output_v1.gbnf");
const LIVE_PROMPT: &str = include_str!("../../../prompts/live_v1.txt");
const CONSOLIDATE_GRAMMAR: &str = include_str!("../../../grammars/consolidate_v1.gbnf");
const CONSOLIDATE_PROMPT: &str = include_str!("../../../prompts/consolidate_v1.txt");

/// Batch tier context. The live tier gets a smaller one (`LIVE_N_CTX`) so
/// its KV cache stays cheap to hold; it still clears `digest::MAX_TOKENS`
/// plus the live prompt's overhead so a busy window is never truncated.
pub const N_CTX: u32 = 4096;
pub const LIVE_N_CTX: u32 = 3072;

pub use chronicle_core::types::{DeriveOutput, IntervalDraft, LiveDraft};

/// Which instruction/grammar pair a request uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Batch,
    Live,
    /// Day-tier consolidation (chunk 6): merges and renames over the day.
    Consolidate,
}

impl Prompt {
    fn text(self) -> &'static str {
        match self {
            Prompt::Batch => BATCH_PROMPT,
            Prompt::Live => LIVE_PROMPT,
            Prompt::Consolidate => CONSOLIDATE_PROMPT,
        }
    }
    fn grammar(self) -> &'static str {
        match self {
            Prompt::Batch => BATCH_GRAMMAR,
            Prompt::Live => LIVE_GRAMMAR,
            Prompt::Consolidate => CONSOLIDATE_GRAMMAR,
        }
    }
    /// 8 intervals × ~35 tokens under the v4 keys, with room for long
    /// labels; the 300 tokens freed from v3's 900 went to digest::MAX_TOKENS.
    /// The live object is one interval's worth.
    fn max_gen(self) -> usize {
        match self {
            Prompt::Batch => 600,
            Prompt::Live => 80,
            Prompt::Consolidate => 400,
        }
    }
}

/// What one request cost (m27 chunk 2 instrumentation).
#[derive(Debug, Clone)]
pub struct RunStats {
    pub prompt_tokens: usize,
    /// Prompt tokens already in the KV cache from the previous request (the
    /// instruction prefix, ~1000 tokens, on every call after the first).
    pub cached_prefix_tokens: usize,
    pub gen_tokens: usize,
    pub prompt_eval_ms: u64,
    pub gen_ms: u64,
}

/// One batch derive: the parsed intervals plus the raw text (the inspector's
/// "last output") and stats.
#[derive(Debug)]
pub struct DeriveRun {
    pub intervals: Vec<IntervalDraft>,
    pub raw: String,
    pub prompt_tokens: usize,
    pub cached_prefix_tokens: usize,
    pub gen_tokens: usize,
    pub prompt_eval_ms: u64,
    pub gen_ms: u64,
}

/// One live pass: the single draft plus stats.
#[derive(Debug)]
pub struct LiveRun {
    pub draft: LiveDraft,
    pub raw: String,
    pub stats: RunStats,
}

/// One consolidation pass: the model's plan (unguarded) plus stats.
#[derive(Debug)]
pub struct ConsolidateRun {
    pub plan: chronicle_core::consolidate::ModelPlan,
    pub raw: String,
    pub stats: RunStats,
}

/// A loaded model (mmap) plus the backend it lives on.
pub struct DeriveModel {
    backend: LlamaBackend,
    model: LlamaModel,
}

impl DeriveModel {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        let (backend, model) = backend::load_model(model_path)?;
        Ok(Self { backend, model })
    }

    /// A batch-tier context that keeps its KV cache between requests.
    pub fn session(&self) -> anyhow::Result<DeriveSession<'_>> {
        self.session_with(Prompt::Batch, N_CTX)
    }

    /// A context for `prompt` with `n_ctx` tokens of KV cache.
    pub fn session_with(&self, prompt: Prompt, n_ctx: u32) -> anyhow::Result<DeriveSession<'_>> {
        let threads = backend::threads();
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            .with_n_batch(N_BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let ctx = self.model.new_context(&self.backend, ctx_params)?;
        Ok(DeriveSession {
            model: &self.model,
            ctx,
            prompt,
            n_ctx,
            batch: LlamaBatch::new(N_BATCH as usize, 1),
            cached: Vec::new(),
        })
    }
}

pub struct DeriveSession<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    prompt: Prompt,
    n_ctx: u32,
    batch: LlamaBatch<'m>,
    /// Tokens whose KV entries are resident at positions `0..len`: the last
    /// prompt followed by what the model generated for it.
    cached: Vec<LlamaToken>,
}

impl DeriveSession<'_> {
    /// Derive one batch digest, streaming generated text through `on_token`.
    /// Raw model intervals; callers sanitize + link (chronicle_core::merge).
    pub fn infer(
        &mut self,
        digest: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<DeriveRun> {
        anyhow::ensure!(
            self.prompt == Prompt::Batch,
            "batch derive on a {:?} session",
            self.prompt
        );
        let (raw, stats) = self.generate(digest, on_token)?;
        let parsed: DeriveOutput = serde_json::from_str(&raw)
            .with_context(|| format!("model output is not valid interval JSON: {raw}"))?;
        Ok(DeriveRun {
            intervals: parsed.intervals,
            raw,
            prompt_tokens: stats.prompt_tokens,
            cached_prefix_tokens: stats.cached_prefix_tokens,
            gen_tokens: stats.gen_tokens,
            prompt_eval_ms: stats.prompt_eval_ms,
            gen_ms: stats.gen_ms,
        })
    }

    /// Label the current stretch (live tier).
    pub fn infer_live(
        &mut self,
        digest: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LiveRun> {
        anyhow::ensure!(
            self.prompt == Prompt::Live,
            "live derive on a {:?} session",
            self.prompt
        );
        let (raw, stats) = self.generate(digest, on_token)?;
        let draft: LiveDraft = serde_json::from_str(&raw)
            .with_context(|| format!("model output is not valid live JSON: {raw}"))?;
        Ok(LiveRun { draft, raw, stats })
    }

    /// Merges and renames over the day's tasks (day tier).
    pub fn infer_consolidate(
        &mut self,
        input: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<ConsolidateRun> {
        anyhow::ensure!(
            self.prompt == Prompt::Consolidate,
            "consolidate on a {:?} session",
            self.prompt
        );
        let (raw, stats) = self.generate(input, on_token)?;
        let plan = serde_json::from_str(&raw)
            .with_context(|| format!("model output is not valid consolidation JSON: {raw}"))?;
        Ok(ConsolidateRun { plan, raw, stats })
    }

    fn generate(
        &mut self,
        digest: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<(String, RunStats)> {
        let max_gen = self.prompt.max_gen();
        let limit = self.n_ctx as usize - max_gen - 64;
        let prompt = self.prompt;
        let model = self.model;
        let tokens = crate::fit_prompt(digest, limit, |d| tokenize_prompt(model, prompt, d))?;

        // Reuse the cached prefix; the last prompt token is always decoded
        // again so its logits exist for the first sample.
        let keep = backend::common_prefix(&self.cached, &tokens).min(tokens.len() - 1);
        self.ctx
            .clear_kv_cache_seq(Some(0), Some(keep as u32), None)
            .context("clearing kv cache")?;
        self.cached.truncate(keep);

        let t_prompt = std::time::Instant::now();
        let start_pos =
            backend::decode_prompt(&mut self.ctx, &mut self.batch, &tokens[keep..], keep as i32)?;
        self.cached.extend_from_slice(&tokens[keep..]);
        let prompt_eval_ms = t_prompt.elapsed().as_millis() as u64;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::grammar(self.model, prompt.grammar(), "root")
                .context("compiling task grammar")?,
            LlamaSampler::greedy(),
        ]);

        let t_gen = std::time::Instant::now();
        let mut out = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut gen_tokens = 0usize;
        for pos in (start_pos..).take(max_gen) {
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
            self.ctx.decode(&mut self.batch)?;
            self.cached.push(token);
        }
        let gen_ms = t_gen.elapsed().as_millis() as u64;
        tracing::info!(
            ?prompt,
            prompt_tokens = tokens.len(),
            cached_prefix_tokens = keep,
            limit,
            digest_chars = digest.len(),
            gen_tokens,
            prompt_eval_ms,
            gen_ms,
            "derive run"
        );
        Ok((
            out,
            RunStats {
                prompt_tokens: tokens.len(),
                cached_prefix_tokens: keep,
                gen_tokens,
                prompt_eval_ms,
                gen_ms,
            },
        ))
    }
}

fn tokenize_prompt(
    model: &LlamaModel,
    prompt: Prompt,
    digest: &str,
) -> anyhow::Result<Vec<LlamaToken>> {
    let content = prompt.text().replace("{digest}", digest);
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
    fn prompts_carry_the_digest_marker() {
        for p in [
            super::Prompt::Batch,
            super::Prompt::Live,
            super::Prompt::Consolidate,
        ] {
            assert!(p.text().contains("{digest}"), "{p:?}");
            assert!(p.grammar().contains("root ::="), "{p:?}");
        }
        let live: super::LiveDraft = serde_json::from_str(
            r#"{"ref": 2, "label": null, "project": null, "confidence": 0.7}"#,
        )
        .unwrap();
        assert_eq!(live.task_ref, Some(2));
    }
}
