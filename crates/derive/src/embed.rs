//! Sentence embeddings through llama.cpp (m30 chunk 6). Only the bench
//! lives here until the latency gate says the soft tier gets a model: load
//! an embedding GGUF, embed a batch of stored titles one at a time (the
//! per-span cost the daemon would pay), report the percentiles.

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

use anyhow::Context;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::AddBos;

use crate::backend;

/// Context size for one title; titles are short and long ones are clipped.
const N_CTX: u32 = 512;

#[derive(Debug, Clone, PartialEq)]
pub struct EmbedStats {
    pub dim: usize,
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub mean_ms: f64,
    /// Wall time to load the model and build the context.
    pub load_ms: f64,
}

/// A loaded embedding model. One context per `embed` call; a call embeds
/// its texts one at a time (mean pooling, L2-normalised).
pub struct Embedder {
    backend: llama_cpp_2::llama_backend::LlamaBackend,
    model: llama_cpp_2::model::LlamaModel,
}

impl Embedder {
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        let (backend, model) = backend::load_model(model_path)?;
        Ok(Self { backend, model })
    }

    pub fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let threads = backend::threads();
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_CTX)
            .with_n_ubatch(N_CTX)
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_embeddings(true)
            .with_pooling_type(LlamaPoolingType::Mean);
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .context("embedding context")?;
        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            let mut tokens = self.model.str_to_token(text, AddBos::Always)?;
            tokens.truncate(N_CTX as usize - 2);
            batch.clear();
            batch.add_sequence(&tokens, 0, false)?;
            ctx.clear_kv_cache();
            ctx.decode(&mut batch).context("embedding decode")?;
            let emb = ctx.embeddings_seq_ith(0).context("embedding read")?;
            let norm = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            out.push(if norm > 0.0 {
                emb.iter().map(|x| x / norm).collect()
            } else {
                emb.to_vec()
            });
        }
        Ok(out)
    }
}

/// Embed each of `texts` on its own and time it.
pub fn bench(model_path: &Path, texts: &[String]) -> anyhow::Result<EmbedStats> {
    let started = Instant::now();
    let embedder = Embedder::load(model_path)?;
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut times: Vec<f64> = Vec::with_capacity(texts.len());
    let mut dim = 0;
    for text in texts {
        let t0 = Instant::now();
        let v = embedder.embed(&[text.as_str()])?;
        dim = v.first().map_or(0, |v| v.len());
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if times.is_empty() {
            return 0.0;
        }
        let i = ((times.len() as f64 - 1.0) * p).round() as usize;
        times[i.min(times.len() - 1)]
    };
    Ok(EmbedStats {
        dim,
        n: times.len(),
        p50_ms: pct(0.5),
        p95_ms: pct(0.95),
        mean_ms: if times.is_empty() {
            0.0
        } else {
            times.iter().sum::<f64>() / times.len() as f64
        },
        load_ms,
    })
}
