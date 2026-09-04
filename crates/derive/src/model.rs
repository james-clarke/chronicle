//! Model manager: download GGUF to the data dir, SHA-256 verify, resume.
//! Expected hash/size come from the Hugging Face LFS pointer file at pull
//! time — nothing hardcoded, so preset bumps are one-line changes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub name: &'static str,
    pub repo: &'static str,
    pub file: &'static str,
}

/// Official Qwen GGUF repos ship no Q4_K_M for these sizes; unsloth's do.
/// First entry is the default; qwen3-1.7b stays as the low-RAM fallback.
pub const PRESETS: &[ModelSpec] = &[
    ModelSpec {
        name: "qwen3-4b",
        repo: "unsloth/Qwen3-4B-Instruct-2507-GGUF",
        file: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
    },
    ModelSpec {
        name: "qwen3-1.7b",
        repo: "unsloth/Qwen3-1.7B-GGUF",
        file: "Qwen3-1.7B-Q4_K_M.gguf",
    },
    // Day-tier candidate (m27 chunk 6): about 5 GB, opt-in via
    // `model_path_heavy`; never the default until the replay eval says so.
    ModelSpec {
        name: "qwen3-8b",
        repo: "unsloth/Qwen3-8B-GGUF",
        file: "Qwen3-8B-Q4_K_M.gguf",
    },
];

/// Embedding models for the soft tier (m30 chunk 6): opt-in through
/// `embed_model`; never a derive candidate. bge-small passed the latency
/// gate on the reference laptop (p95 7 ms per title); EmbeddingGemma-300m
/// did not (p95 29 ms).
pub const EMBED_PRESETS: &[ModelSpec] = &[ModelSpec {
    name: "bge-small",
    repo: "CompendiumLabs/bge-small-en-v1.5-gguf",
    file: "bge-small-en-v1.5-q8_0.gguf",
}];

pub fn default_preset() -> &'static ModelSpec {
    &PRESETS[0]
}

/// The embedding model file from `embed_model`: a path, or a file name
/// (or preset name) inside the models directory. None when unset or
/// missing.
pub fn resolve_embed(embed_model: Option<&str>, data_dir: &Path) -> Option<PathBuf> {
    let name = embed_model?;
    let direct = PathBuf::from(name);
    if direct.is_absolute() && direct.exists() {
        return Some(direct);
    }
    let file = EMBED_PRESETS
        .iter()
        .find(|p| p.name == name)
        .map_or(name, |p| p.file);
    let p = models_dir(data_dir).join(file);
    p.exists().then_some(p)
}

pub fn preset(name: &str) -> Option<&'static ModelSpec> {
    PRESETS
        .iter()
        .chain(EMBED_PRESETS.iter())
        .find(|p| p.name == name)
}

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Active model: explicit config path wins, else the default preset if
/// downloaded. None = derivation cannot run yet.
pub fn resolve(config_model_path: Option<&Path>, data_dir: &Path) -> Option<PathBuf> {
    if let Some(p) = config_model_path {
        return p.exists().then(|| p.to_path_buf());
    }
    let p = models_dir(data_dir).join(default_preset().file);
    p.exists().then_some(p)
}

struct Pointer {
    sha256: String,
    size: u64,
}

fn fetch_pointer(spec: &ModelSpec) -> anyhow::Result<Pointer> {
    let url = format!(
        "https://huggingface.co/{}/raw/main/{}",
        spec.repo, spec.file
    );
    let mut resp = ureq::get(&url)
        .call()
        .with_context(|| format!("fetching LFS pointer {url}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .context("reading LFS pointer body")?;
    let mut sha256 = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(oid) = line.strip_prefix("oid sha256:") {
            sha256 = Some(oid.trim().to_string());
        } else if let Some(s) = line.strip_prefix("size ") {
            size = Some(s.trim().parse::<u64>()?);
        }
    }
    match (sha256, size) {
        (Some(sha256), Some(size)) => Ok(Pointer { sha256, size }),
        _ => bail!("no LFS pointer at {url} (got: {})", text.escape_debug()),
    }
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download (or resume) the model, verify SHA-256, and return the final path.
/// `progress(done_bytes, total_bytes)` fires roughly once per MiB.
pub fn pull(
    data_dir: &Path,
    spec: &ModelSpec,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<PathBuf> {
    let dir = models_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let final_path = dir.join(spec.file);
    let part_path = dir.join(format!("{}.part", spec.file));

    let ptr = fetch_pointer(spec)?;
    if final_path.exists() {
        if std::fs::metadata(&final_path)?.len() == ptr.size
            && sha256_file(&final_path)? == ptr.sha256
        {
            return Ok(final_path);
        }
        // Size or hash mismatch: stale/corrupt file, redownload.
        std::fs::remove_file(&final_path)?;
    }

    let mut done = if part_path.exists() {
        std::fs::metadata(&part_path)?.len()
    } else {
        0
    };
    if done > ptr.size {
        std::fs::remove_file(&part_path)?;
        done = 0;
    }

    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        spec.repo, spec.file
    );
    let mut req = ureq::get(&url);
    if done > 0 {
        req = req.header("Range", format!("bytes={done}-"));
    }
    let resp = req.call().with_context(|| format!("downloading {url}"))?;
    let status = resp.status().as_u16();
    let mut out = match status {
        206 if done > 0 => std::fs::OpenOptions::new().append(true).open(&part_path)?,
        200 => {
            // Server ignored the Range header — restart from zero.
            done = 0;
            std::fs::File::create(&part_path)?
        }
        _ => bail!("unexpected status {status} from {url}"),
    };

    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, ptr.size);
    }
    out.flush()?;
    drop(out);

    let got = std::fs::metadata(&part_path)?.len();
    if got != ptr.size {
        bail!(
            "download incomplete: {got} of {} bytes (rerun to resume)",
            ptr.size
        );
    }
    let hash = sha256_file(&part_path)?;
    if hash != ptr.sha256 {
        std::fs::remove_file(&part_path)?;
        bail!("SHA-256 mismatch: expected {}, got {hash}", ptr.sha256);
    }
    std::fs::rename(&part_path, &final_path)?;
    Ok(final_path)
}
