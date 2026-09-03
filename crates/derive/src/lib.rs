//! Digest→prompt, llama runner, GBNF grammar, model manager.
//! Corrections retrieval lands in M5.

pub mod chat;
pub mod describe;
pub mod model;
pub mod runner;

pub use chat::ChatModel;
pub use runner::{
    ConsolidateRun, DeriveModel, DeriveRun, DeriveSession, IntervalDraft, LIVE_N_CTX, LiveDraft,
    LiveRun, N_CTX, Prompt, RunStats, infer_intervals,
};

/// Tokenize `text` inside its prompt, shrinking the text until the whole
/// prompt fits `limit` tokens. The digest cap is a chars/4 heuristic and the
/// rendered prompt wraps the text in fixed template and instruction tokens,
/// so one proportional cut of the text alone can still land over the limit;
/// each pass scales the text by the ratio it missed by, with a little
/// headroom, and converges in a few steps.
pub(crate) fn fit_prompt<T>(
    text: &str,
    limit: usize,
    mut tokenize: impl FnMut(&str) -> anyhow::Result<Vec<T>>,
) -> anyhow::Result<Vec<T>> {
    let mut cut = text.len();
    let mut tokens = tokenize(text)?;
    for _ in 0..4 {
        if tokens.len() <= limit {
            return Ok(tokens);
        }
        cut = (cut * limit * 96 / (tokens.len() * 100)).min(cut.saturating_sub(1));
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        tokens = tokenize(&text[..cut])?;
    }
    if tokens.len() > limit {
        anyhow::bail!(
            "prompt still too long after truncation: {} tokens",
            tokens.len()
        );
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    /// A tokenizer denser than chars/4 with 1200 tokens of fixed overhead:
    /// the proportional cut must iterate to land under the limit.
    #[test]
    fn fit_prompt_converges_past_fixed_overhead() {
        let text = "x".repeat(12_000);
        let tok = |t: &str| Ok((0..1200 + t.len() / 3).collect::<Vec<usize>>());
        let tokens = super::fit_prompt(&text, 3132, tok).unwrap();
        assert!(tokens.len() <= 3132, "{}", tokens.len());
        assert!(tokens.len() > 2900, "over-cut: {}", tokens.len());
        let short = super::fit_prompt("short", 3132, tok).unwrap();
        assert_eq!(short.len(), 1200 + 1);
        assert!(
            super::fit_prompt(&text, 1000, tok).is_err(),
            "overhead alone exceeds"
        );
    }
}
