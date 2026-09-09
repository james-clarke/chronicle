//! Corrections as memory (m36 chunk 2): the few past corrections nearest
//! the work at hand, for a naming, consolidate or advisor prompt. Nearest
//! by cosine over the bge-small vectors in `correction_embeddings` when
//! the model is on disk and rows are embedded; by the FTS match of
//! `storage::correction_hints` otherwise, so the section renders the same
//! either way.

use std::path::Path;

use chronicle_core::storage;
use chronicle_core::types::Correction;
use rusqlite::Connection;

use crate::embed::Embedder;
use crate::text::JobKind;

pub struct Examples {
    embedder: Option<Embedder>,
}

impl Examples {
    /// Load the embedding model when one resolves (`embed_model`, else the
    /// downloaded preset); a load failure logs and falls back to FTS.
    pub fn open(embed_model: Option<&str>, data_dir: &Path) -> Self {
        let embedder =
            crate::model::resolve_embed_or_default(embed_model, data_dir).and_then(|p| {
                match Embedder::load(&p) {
                    Ok(e) => Some(e),
                    Err(e) => {
                        tracing::warn!("embedding model failed to load, examples use FTS: {e}");
                        None
                    }
                }
            });
        Self { embedder }
    }

    pub fn none() -> Self {
        Self { embedder: None }
    }

    /// The correction kinds a job learns from, as a SQL list literal.
    pub fn kinds_for(job: JobKind) -> &'static str {
        match job {
            JobKind::NameTask | JobKind::SuggestTask => "('rename', 'assign', 'reassign', 'merge')",
            JobKind::Consolidate => "('rename', 'merge')",
            _ => storage::EXAMPLE_KINDS,
        }
    }

    /// Up to `k` corrections nearest `text` (the "app: title" lines of the
    /// work), positives first as the digest renders them.
    pub fn nearest(
        &self,
        conn: &Connection,
        job: JobKind,
        text: &str,
        k: usize,
    ) -> anyhow::Result<Vec<Correction>> {
        let kinds = Self::kinds_for(job);
        if let Some(e) = &self.embedder
            && storage::correction_embeddings_count(conn)? > 0
        {
            let query: String = text.chars().take(1500).collect();
            let vec = e.embed(&[query.as_str()])?.pop().unwrap_or_default();
            let near = storage::nearest_corrections(conn, &vec, kinds, k)?;
            return Ok(near
                .into_iter()
                .filter(|(_, sim)| *sim >= MIN_SIMILARITY)
                .map(|(c, _)| c)
                .collect());
        }
        let allowed: Vec<&str> = kinds
            .trim_matches(|c| c == '(' || c == ')')
            .split(',')
            .map(|s| s.trim().trim_matches('\''))
            .collect();
        Ok(storage::correction_hints(conn, text)?
            .into_iter()
            .filter(|c| allowed.contains(&c.kind.as_str()))
            .take(k)
            .collect())
    }
}

/// Below this cosine the nearest correction is about other work; a
/// section of unrelated examples pulls a label toward them.
pub const MIN_SIMILARITY: f32 = 0.6;

/// The "evidence → wrong → right" lines for a prompt that has no digest
/// (consolidate); the naming digest renders the same corrections through
/// `digest::build_digest` instead.
pub fn render_section(corrections: &[Correction]) -> String {
    if corrections.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Past corrections (how the user named similar work)\n");
    for c in corrections {
        let evidence = c
            .ctx
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| {
                chronicle_core::evidence::strip_glyphs(l.trim())
                    .chars()
                    .take(80)
                    .collect::<String>()
            })
            .unwrap_or_default();
        let old = chronicle_core::evidence::strip_glyphs(&c.old_label);
        let new = chronicle_core::evidence::strip_glyphs(&c.new_label);
        if c.kind == "eject" {
            out.push_str(&format!("- \"{evidence}\" \u{2717} \"{old}\"\n"));
            continue;
        }
        // An assign has no wrong label: the work it was made over is the
        // left-hand side, as the digest renders it.
        let unassigned = c.kind == "assign" || old == "(unassigned)";
        out.push_str("- ");
        if !evidence.is_empty() {
            out.push_str(&format!("\"{evidence}\" \u{2192} "));
        }
        if !unassigned {
            out.push_str(&format!("\"{old}\" \u{2192} "));
        }
        out.push_str(&format!("\"{new}\""));
        if let Some(p) = &c.new_project
            && c.old_project != c.new_project
        {
            out.push_str(&format!(" [{p}]"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_renders_evidence_wrong_right() {
        let cs = vec![
            Correction {
                old_label: "shop work".into(),
                new_label: "fixing checkout".into(),
                old_project: None,
                new_project: Some("shop".into()),
                kind: "rename".into(),
                ctx: "Code cart.py \u{2014} shop\nTerminal pytest\n".into(),
            },
            Correction {
                old_label: "fixing checkout".into(),
                new_label: "(unassigned)".into(),
                old_project: Some("shop".into()),
                new_project: None,
                kind: "eject".into(),
                ctx: "Slack #payments\n".into(),
            },
            Correction {
                old_label: "old".into(),
                new_label: "new".into(),
                old_project: None,
                new_project: None,
                kind: "merge".into(),
                ctx: String::new(),
            },
            Correction {
                old_label: "(unassigned)".into(),
                new_label: "ACME-11533 export modal".into(),
                old_project: None,
                new_project: None,
                kind: "assign".into(),
                ctx: "Google-chrome Jam\n".into(),
            },
        ];
        assert_eq!(
            render_section(&cs),
            "\n## Past corrections (how the user named similar work)\n- \"Code cart.py \u{2014} shop\" \u{2192} \"shop work\" \u{2192} \"fixing checkout\" [shop]\n- \"Slack #payments\" \u{2717} \"fixing checkout\"\n- \"old\" \u{2192} \"new\"\n- \"Google-chrome Jam\" \u{2192} \"ACME-11533 export modal\"\n"
        );
        assert_eq!(render_section(&[]), "");
        assert_eq!(
            Examples::kinds_for(JobKind::Consolidate),
            "('rename', 'merge')"
        );
    }

    /// No model, no embedded rows: the FTS path answers, filtered by kind.
    #[test]
    fn falls_back_to_fts_without_a_model() {
        let path =
            std::env::temp_dir().join(format!("chronicle-examples-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = storage::open(&path).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, label, status, source, created_ts) VALUES (1, 'a', 'open', 'derived', 1)",
            [],
        )
        .unwrap();
        for (id, kind, ctx, old, new) in [
            (
                1,
                "rename",
                "Code cart.py checkout\n",
                "shop work",
                "fixing checkout",
            ),
            (
                2,
                "eject",
                "Code cart.py checkout\n",
                "fixing checkout",
                "(unassigned)",
            ),
        ] {
            conn.execute(
                "INSERT INTO corrections (id, ts, task_id, old_label, new_label, ctx, kind) VALUES (?1, 1, 1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, old, new, ctx, kind],
            )
            .unwrap();
        }
        let ex = Examples::none();
        let near = ex
            .nearest(&conn, JobKind::NameTask, "Code cart.py", 4)
            .unwrap();
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].new_label, "fixing checkout");
        let near = ex
            .nearest(&conn, JobKind::Derive, "Code cart.py", 4)
            .unwrap();
        assert_eq!(near.len(), 2);
    }
}
