//! One-shot short-text generation: task descriptions, declare suggestions,
//! range narratives. The model loads once per [`Describer`] — one job for an
//! ephemeral `ai-job` worker, many for the backfill command.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::Context;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use chronicle_core::types::SuggestedTask;

use crate::backend::{self, N_BATCH};
use crate::chat::ThinkFilter;
use crate::prompts::{self, CHECKPOINT_GRAMMAR, SUGGEST_GRAMMAR, non_empty};
use crate::text::JobKind;

const N_CTX: u32 = 4096;
/// Descriptions and narratives are a few sentences; suggestions one JSON object.
const MAX_GEN: usize = 256;

pub struct Describer {
    backend: LlamaBackend,
    model: LlamaModel,
}

impl Describer {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        let (backend, model) = backend::load_model(model_path)?;
        Ok(Self { backend, model })
    }

    /// 1-2 sentence description of a finished task from its span evidence.
    pub fn describe_task(
        &self,
        label: &str,
        project: Option<&str>,
        evidence: &str,
    ) -> anyhow::Result<String> {
        let prompt = prompts::render_description(label, project, evidence);
        non_empty(self.generate(&prompt, None)?, "description")
    }

    /// 1-3 sentence journal entry for one batch's slice of a task. `context`
    /// is the task's external ticket context (may be empty), `git` the
    /// session's vcs lines, `evidence` the session's span lines.
    pub fn journal_entry(
        &self,
        label: &str,
        project: Option<&str>,
        context: &str,
        git: &str,
        evidence: &str,
    ) -> anyhow::Result<String> {
        let prompt = prompts::render_journal(label, project, context, git, evidence);
        non_empty(self.generate(&prompt, None)?, "journal entry")
    }

    /// 2-4 sentence narrative over a pre-rendered stats digest.
    pub fn narrative(&self, digest: &str) -> anyhow::Result<String> {
        let prompt = prompts::render_narrative(digest);
        non_empty(self.generate(&prompt, None)?, "narrative")
    }

    /// Morning standup draft (one short paragraph per task) over a
    /// pre-rendered digest of a day's journal entries and checkpoints.
    pub fn standup(&self, digest: &str) -> anyhow::Result<String> {
        let prompt = prompts::render_standup(digest);
        non_empty(self.generate(&prompt, None)?, "standup draft")
    }

    /// Grammar-constrained checkpoint: (state, next_steps) from the task's
    /// journal tail and optional external ticket context.
    pub fn checkpoint(
        &self,
        label: &str,
        project: Option<&str>,
        context: &str,
        journal: &str,
    ) -> anyhow::Result<(String, String)> {
        let prompt = prompts::render_checkpoint(label, project, context, journal);
        let out = self.generate(&prompt, Some(CHECKPOINT_GRAMMAR))?;
        prompts::parse_checkpoint(&out)
    }

    /// Grammar-constrained declare suggestion from a recent-activity digest.
    pub fn suggest_task(&self, digest: &str) -> anyhow::Result<SuggestedTask> {
        let prompt = prompts::render_suggest(digest);
        let out = self.generate(&prompt, Some(SUGGEST_GRAMMAR))?;
        prompts::parse_suggest(&out)
    }

    /// The [`crate::text::TextBackend`] shape over the local model: the job
    /// picks its grammar; the caller has already rendered the prompt.
    pub fn complete(&self, job: JobKind, prompt: &str) -> anyhow::Result<String> {
        self.generate(prompt, Self::grammar_for(job))
    }

    /// GBNF for a JSON job on the local engine; prose jobs run free.
    pub fn grammar_for(job: JobKind) -> Option<&'static str> {
        match job {
            JobKind::Checkpoint => Some(CHECKPOINT_GRAMMAR),
            JobKind::SuggestTask | JobKind::NameTask => Some(SUGGEST_GRAMMAR),
            _ => None,
        }
    }

    /// One prompt in, one bounded completion out; fresh context per call.
    /// With a grammar the raw text is returned (the grammar's first token
    /// forecloses think blocks); without one the ThinkFilter strips Qwen3's
    /// empty `<think>` preamble.
    fn generate(&self, content: &str, grammar: Option<&str>) -> anyhow::Result<String> {
        let threads = backend::threads();
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let mut ctx = self.model.new_context(&self.backend, ctx_params)?;

        let limit = N_CTX as usize - MAX_GEN - 64;
        let tokens = crate::fit_prompt(content, limit, |c| self.tokenize(c))?;

        let mut batch = LlamaBatch::new(N_BATCH as usize, 1);
        let start_pos = backend::decode_prompt(&mut ctx, &mut batch, &tokens, 0)?;

        let mut sampler = match grammar {
            Some(g) => LlamaSampler::chain_simple([
                LlamaSampler::grammar(&self.model, g, "root").context("compiling grammar")?,
                LlamaSampler::greedy(),
            ]),
            None => LlamaSampler::chain_simple([LlamaSampler::greedy()]),
        };

        let mut filter = ThinkFilter::default();
        let mut out = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        for pos in (start_pos..).take(MAX_GEN) {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            if self.model.is_eog_token(token) {
                break;
            }
            let piece = self
                .model
                .token_to_piece(token, &mut decoder, false, None)?;
            if grammar.is_some() {
                out.push_str(&piece);
            } else {
                filter.push(&piece, &mut |s| out.push_str(s));
            }
            batch.clear();
            batch.add(token, pos, &[0], true)?;
            ctx.decode(&mut batch)?;
        }
        if grammar.is_none() {
            filter.finish(&mut |s| out.push_str(s));
        }
        Ok(out)
    }

    fn tokenize(&self, content: &str) -> anyhow::Result<Vec<llama_cpp_2::token::LlamaToken>> {
        let tmpl = self
            .model
            .chat_template(None)
            .context("model has no embedded chat template")?;
        let text = self.model.apply_chat_template(
            &tmpl,
            &[LlamaChatMessage::new("user".into(), content.into())?],
            true,
        )?;
        // Never AddBos: the rendered template already carries its special tokens.
        Ok(self.model.str_to_token(&text, AddBos::Never)?)
    }
}
