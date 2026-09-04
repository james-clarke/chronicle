//! Warm chat session: model loaded once per worker process. `ChatSession`
//! keeps one context whose KV cache holds the tokens of the last prompt and
//! answer, mirroring `runner::DeriveSession` — each question decodes only
//! what differs from the cached tokens. Free-text output (no grammar) —
//! chat answers are only ever displayed, never acted on.

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

pub const SYSTEM_PROMPT: &str = include_str!("../../../prompts/chat_v1.txt");

const N_CTX: u32 = 4096;
const MAX_GEN: usize = 1024;
/// Prior turns re-sent with each question (answers clipped to keep room).
const HISTORY_TURNS: usize = 3;
const HISTORY_ANSWER_CHARS: usize = 800;

pub struct ChatModel {
    backend: LlamaBackend,
    model: LlamaModel,
}

impl ChatModel {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        let (backend, model) = backend::load_model(model_path)?;
        Ok(Self { backend, model })
    }

    /// A context that keeps its KV cache between turns; see `ChatSession`.
    pub fn session(&self) -> anyhow::Result<ChatSession<'_>> {
        let threads = backend::threads();
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let ctx = self.model.new_context(&self.backend, ctx_params)?;
        Ok(ChatSession {
            model: &self.model,
            ctx,
            batch: LlamaBatch::new(N_BATCH as usize, 1),
            cached: Vec::new(),
        })
    }

    /// Answer one question grounded in `context`, streaming filtered pieces
    /// through `on_token`. Returns the full filtered answer. Fresh context
    /// per call — callers asking more than one question should hold a
    /// `ChatSession` instead so the KV cache carries across turns.
    pub fn answer(
        &self,
        history: &[(String, String)],
        context: &str,
        question: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        self.session()?.answer(history, context, question, on_token)
    }
}

pub struct ChatSession<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    batch: LlamaBatch<'m>,
    /// Tokens whose KV entries are resident at positions `0..len`: the last
    /// prompt followed by what the model generated for it.
    cached: Vec<LlamaToken>,
}

impl ChatSession<'_> {
    /// Answer one question grounded in `context`, streaming filtered pieces
    /// through `on_token`. Returns the full filtered answer. Reuses the KV
    /// cache across turns: only the suffix of the new prompt after the
    /// longest common prefix with the last prompt+answer gets decoded.
    pub fn answer(
        &mut self,
        history: &[(String, String)],
        context: &str,
        question: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        let limit = N_CTX as usize - MAX_GEN - 64;
        let model = self.model;
        let tokens = crate::fit_prompt(context, limit, |c| tokenize(model, history, c, question))?;

        // fit_prompt caps tokens well under N_CTX; this is a defensive
        // fallback in case that ever doesn't hold, forfeiting cache reuse
        // rather than decoding past the end of the KV cache.
        let keep = if tokens.len() >= N_CTX as usize {
            self.cached.clear();
            0
        } else {
            backend::common_prefix(&self.cached, &tokens).min(tokens.len() - 1)
        };
        self.ctx
            .clear_kv_cache_seq(Some(0), Some(keep as u32), None)
            .context("clearing kv cache")?;
        self.cached.truncate(keep);

        let start_pos =
            backend::decode_prompt(&mut self.ctx, &mut self.batch, &tokens[keep..], keep as i32)?;
        self.cached.extend_from_slice(&tokens[keep..]);

        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);
        let mut filter = ThinkFilter::default();
        let mut out = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        for pos in (start_pos..).take(MAX_GEN) {
            let token = sampler.sample(&self.ctx, self.batch.n_tokens() - 1);
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
            self.batch.clear();
            self.batch.add(token, pos, &[0], true)?;
            self.ctx.decode(&mut self.batch)?;
            self.cached.push(token);
        }
        filter.finish(&mut |s| {
            out.push_str(s);
            on_token(s);
        });
        Ok(out)
    }
}

fn tokenize(
    model: &LlamaModel,
    history: &[(String, String)],
    context: &str,
    question: &str,
) -> anyhow::Result<Vec<LlamaToken>> {
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
    let tmpl = model
        .chat_template(None)
        .context("model has no embedded chat template")?;
    let text = model.apply_chat_template(&tmpl, &messages, true)?;
    // Never AddBos: the rendered template already carries its special tokens.
    Ok(model.str_to_token(&text, AddBos::Never)?)
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
pub(crate) enum ThinkFilter {
    #[default]
    Start,
    Pending(String),
    Swallow(String),
    Pass,
}

impl ThinkFilter {
    pub(crate) fn push(&mut self, piece: &str, emit: &mut impl FnMut(&str)) {
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
    pub(crate) fn finish(&mut self, emit: &mut impl FnMut(&str)) {
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
