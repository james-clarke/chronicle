//! One-shot short-text generation: task descriptions, declare suggestions,
//! range narratives. The model loads once per [`Describer`] — one job for an
//! ephemeral `ai-job` worker, many for the backfill command.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::{Context, bail};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use chronicle_core::types::SuggestedTask;

use crate::chat::ThinkFilter;

const DESCRIPTION_PROMPT: &str = include_str!("../../../prompts/task_description_v1.txt");
const SUGGEST_PROMPT: &str = include_str!("../../../prompts/suggest_task_v1.txt");
const NARRATIVE_PROMPT: &str = include_str!("../../../prompts/narrative_v1.txt");
const SUGGEST_GRAMMAR: &str = include_str!("../../../grammars/suggest_task_v1.gbnf");

const N_CTX: u32 = 4096;
const N_BATCH: u32 = 512;
/// Descriptions and narratives are a few sentences; suggestions one JSON object.
const MAX_GEN: usize = 256;

pub struct Describer {
    backend: LlamaBackend,
    model: LlamaModel,
}

impl Describer {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        static LLAMA_LOGS: std::sync::Once = std::sync::Once::new();
        LLAMA_LOGS
            .call_once(|| llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default()));
        let backend = LlamaBackend::init()?;
        let model = LlamaModel::load_from_file(&backend, model_path, &LlamaModelParams::default())
            .with_context(|| format!("loading model {}", model_path.display()))?;
        Ok(Self { backend, model })
    }

    /// 1-2 sentence description of a finished task from its span evidence.
    pub fn describe_task(
        &self,
        label: &str,
        project: Option<&str>,
        evidence: &str,
    ) -> anyhow::Result<String> {
        let project_line = project.map(|p| format!(" [{p}]")).unwrap_or_default();
        let prompt = DESCRIPTION_PROMPT
            .replace("{label}", label)
            .replace("{project_line}", &project_line)
            .replace("{evidence}", evidence);
        let out = self.generate(&prompt, None)?;
        if out.trim().is_empty() {
            bail!("model produced an empty description");
        }
        Ok(out.trim().to_owned())
    }

    /// 2-4 sentence narrative over a pre-rendered stats digest.
    pub fn narrative(&self, digest: &str) -> anyhow::Result<String> {
        let prompt = NARRATIVE_PROMPT.replace("{digest}", digest);
        let out = self.generate(&prompt, None)?;
        if out.trim().is_empty() {
            bail!("model produced an empty narrative");
        }
        Ok(out.trim().to_owned())
    }

    /// Grammar-constrained declare suggestion from a recent-activity digest.
    pub fn suggest_task(&self, digest: &str) -> anyhow::Result<SuggestedTask> {
        let prompt = SUGGEST_PROMPT.replace("{digest}", digest);
        let out = self.generate(&prompt, Some(SUGGEST_GRAMMAR))?;
        let parsed: SuggestedTask = serde_json::from_str(&out)
            .with_context(|| format!("model output is not valid suggestion JSON: {out}"))?;
        Ok(parsed)
    }

    /// One prompt in, one bounded completion out; fresh context per call.
    /// With a grammar the raw text is returned (the grammar's first token
    /// forecloses think blocks); without one the ThinkFilter strips Qwen3's
    /// empty `<think>` preamble.
    fn generate(&self, content: &str, grammar: Option<&str>) -> anyhow::Result<String> {
        let threads = num_cpus::get_physical().saturating_sub(1).clamp(1, 8) as i32;
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let mut ctx = self.model.new_context(&self.backend, ctx_params)?;

        let limit = N_CTX as usize - MAX_GEN - 64;
        let mut tokens = self.tokenize(content)?;
        if tokens.len() > limit {
            // Same chars/4-vs-tokenizer mismatch handling as the derive
            // runner: shrink proportionally and retokenize once.
            let keep = content.len() * limit / tokens.len();
            let mut cut = keep.min(content.len());
            while !content.is_char_boundary(cut) {
                cut -= 1;
            }
            tokens = self.tokenize(&content[..cut])?;
            if tokens.len() > limit {
                bail!(
                    "prompt still too long after truncation: {} tokens",
                    tokens.len()
                );
            }
        }

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
        for _ in 0..MAX_GEN {
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
            pos += 1;
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
