//! Warm chat session: model loaded once per worker process, one fresh
//! context per question. Free-text output (no grammar) — chat answers are
//! only ever displayed, never acted on.

use std::num::NonZeroU32;
use std::path::Path;

use anyhow::{Context, bail};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

const SYSTEM_PROMPT: &str = include_str!("../../../prompts/chat_v1.txt");

const N_CTX: u32 = 4096;
const N_BATCH: u32 = 512;
const MAX_GEN: usize = 512;
/// Prior turns re-sent with each question (answers clipped to keep room).
const HISTORY_TURNS: usize = 3;
const HISTORY_ANSWER_CHARS: usize = 800;

pub struct ChatModel {
    backend: LlamaBackend,
    model: LlamaModel,
}

impl ChatModel {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        static LLAMA_LOGS: std::sync::Once = std::sync::Once::new();
        LLAMA_LOGS
            .call_once(|| llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default()));
        let backend = LlamaBackend::init()?;
        let model = LlamaModel::load_from_file(&backend, model_path, &LlamaModelParams::default())
            .with_context(|| format!("loading model {}", model_path.display()))?;
        Ok(Self { backend, model })
    }

    /// Answer one question grounded in `context`, streaming filtered pieces
    /// through `on_token`. Returns the full filtered answer.
    pub fn answer(
        &self,
        history: &[(String, String)],
        context: &str,
        question: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        let limit = N_CTX as usize - MAX_GEN - 64;
        let mut tokens = self.tokenize(history, context, question)?;
        if tokens.len() > limit {
            // Same chars/4-vs-tokenizer mismatch handling as the derive runner:
            // shrink the context proportionally and retokenize once.
            let keep = context.len() * limit / tokens.len();
            let mut cut = keep.min(context.len());
            while !context.is_char_boundary(cut) {
                cut -= 1;
            }
            tokens = self.tokenize(history, &context[..cut], question)?;
            if tokens.len() > limit {
                bail!(
                    "chat prompt still too long after truncation: {} tokens",
                    tokens.len()
                );
            }
        }

        let threads = num_cpus::get_physical().saturating_sub(1).clamp(1, 8) as i32;
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let mut ctx = self.model.new_context(&self.backend, ctx_params)?;

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

        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);
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
            filter.push(&piece, &mut |s| {
                out.push_str(s);
                on_token(s);
            });
            batch.clear();
            batch.add(token, pos, &[0], true)?;
            pos += 1;
            ctx.decode(&mut batch)?;
        }
        filter.finish(&mut |s| {
            out.push_str(s);
            on_token(s);
        });
        Ok(out)
    }

    fn tokenize(
        &self,
        history: &[(String, String)],
        context: &str,
        question: &str,
    ) -> anyhow::Result<Vec<llama_cpp_2::token::LlamaToken>> {
        let mut messages = vec![LlamaChatMessage::new(
            "system".into(),
            SYSTEM_PROMPT.trim().into(),
        )?];
        for (q, a) in history.iter().rev().take(HISTORY_TURNS).rev() {
            // Prior questions go in bare (their DATA sections would blow the
            // window); answers are clipped for the same reason.
            messages.push(LlamaChatMessage::new(
                "user".into(),
                format!("/no_think\n{q}"),
            )?);
            messages.push(LlamaChatMessage::new(
                "assistant".into(),
                clip_chars(a, HISTORY_ANSWER_CHARS),
            )?);
        }
        messages.push(LlamaChatMessage::new(
            "user".into(),
            format!("/no_think\nDATA:\n{context}\n\nQuestion: {question}"),
        )?);
        let tmpl = self
            .model
            .chat_template(None)
            .context("model has no embedded chat template")?;
        let text = self.model.apply_chat_template(&tmpl, &messages, true)?;
        // Never AddBos: the rendered template already carries its special tokens.
        Ok(self.model.str_to_token(&text, AddBos::Never)?)
    }
}

fn clip_chars(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    }
}

/// Strips a leading `<think>...</think>` block (Qwen3 emits an empty one even
/// under /no_think) from a streamed sequence of pieces, plus leading
/// whitespace, passing everything else through unchanged.
#[derive(Default)]
enum ThinkFilter {
    #[default]
    Start,
    Pending(String),
    Swallow(String),
    Pass,
}

impl ThinkFilter {
    fn push(&mut self, piece: &str, emit: &mut impl FnMut(&str)) {
        match self {
            ThinkFilter::Pass => emit(piece),
            ThinkFilter::Start | ThinkFilter::Pending(_) => {
                let mut buf = match std::mem::replace(self, ThinkFilter::Start) {
                    ThinkFilter::Pending(b) => b,
                    _ => String::new(),
                };
                buf.push_str(piece);
                let trimmed = buf.trim_start();
                if trimmed.is_empty() {
                    // Leading whitespace: hold (and ultimately drop) it.
                    *self = ThinkFilter::Pending(buf);
                } else if let Some(rest) = trimmed.strip_prefix("<think>") {
                    let rest = rest.to_owned();
                    *self = ThinkFilter::Swallow(String::new());
                    self.push(&rest, emit);
                } else if "<think>".starts_with(trimmed) {
                    *self = ThinkFilter::Pending(buf);
                } else {
                    emit(trimmed);
                    *self = ThinkFilter::Pass;
                }
            }
            ThinkFilter::Swallow(buf) => {
                buf.push_str(piece);
                if let Some(pos) = buf.find("</think>") {
                    let rest = buf[pos + "</think>".len()..].trim_start().to_owned();
                    if rest.is_empty() {
                        // Still trim the whitespace between the block and the
                        // real answer.
                        *self = ThinkFilter::Pending(String::new());
                    } else {
                        *self = ThinkFilter::Pass;
                        emit(&rest);
                    }
                } else {
                    // Keep only a tail long enough to hold a split "</think>".
                    let excess = buf.len().saturating_sub("</think>".len());
                    if excess > 0 {
                        let mut cut = excess;
                        while !buf.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        buf.drain(..cut);
                    }
                }
            }
        }
    }

    /// Flush anything still held: a partial non-think prefix is real output;
    /// an unclosed think block is not.
    fn finish(&mut self, emit: &mut impl FnMut(&str)) {
        if let ThinkFilter::Pending(buf) = self {
            let trimmed = buf.trim_start();
            if !trimmed.is_empty() {
                emit(trimmed);
            }
        }
        *self = ThinkFilter::Pass;
    }
}

#[cfg(test)]
mod tests {
    use super::ThinkFilter;

    fn run(pieces: &[&str]) -> String {
        let mut f = ThinkFilter::default();
        let mut out = String::new();
        for p in pieces {
            f.push(p, &mut |s| out.push_str(s));
        }
        f.finish(&mut |s| out.push_str(s));
        out
    }

    #[test]
    fn strips_empty_think_block() {
        assert_eq!(
            run(&["<think>", "\n\n", "</think>", "\n\n", "Hi ", "there"]),
            "Hi there"
        );
    }

    #[test]
    fn strips_think_split_across_pieces() {
        assert_eq!(run(&["<th", "ink>reasoning</th", "ink>\nanswer"]), "answer");
    }

    #[test]
    fn passes_plain_output() {
        assert_eq!(run(&["\n", "Plain", " answer"]), "Plain answer");
    }

    #[test]
    fn partial_lookalike_is_flushed() {
        assert_eq!(run(&["<thing>", " not a tag"]), "<thing> not a tag");
        assert_eq!(run(&["<th"]), "<th");
    }

    #[test]
    fn unclosed_think_yields_nothing() {
        assert_eq!(run(&["<think>", "endless reasoning"]), "");
    }
}
