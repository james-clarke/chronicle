//! Cloud text backends (m31): the same prompts the local model sees, sent
//! to a provider the user chose and keyed. Nothing here runs unless a
//! `models.toml` route names a backend.

pub mod anthropic;
pub mod claude_code;
pub mod sse;

use chronicle_core::models_config::{BackendCfg, BackendKind};

use crate::redact::redact;
use crate::text::{Completion, JobKind, Request, TextBackend};

/// The backend a `[backends.<name>]` entry describes, behind the
/// redaction pass: nothing built here sends a request unredacted.
pub fn build(name: &str, cfg: &BackendCfg) -> anyhow::Result<Box<dyn TextBackend>> {
    let inner: Box<dyn TextBackend> = match cfg.kind {
        BackendKind::Anthropic => Box::new(anthropic::AnthropicBackend::new(
            name,
            &cfg.model,
            &cfg.api_key,
            cfg.base_url.as_deref(),
        )),
        BackendKind::OpenAiCompat => {
            anyhow::bail!("backend {name}: openai_compat arrives in m31 chunk 3")
        }
        BackendKind::ClaudeCode => Box::new(claude_code::ClaudeCodeBackend::new(
            name,
            &cfg.model,
            cfg.command.as_deref(),
        )),
    };
    Ok(Box::new(Redacting(inner)))
}

/// Runs `redact::redact` over every text field of a request (system, user,
/// both sides of the history) before the wrapped backend sees it, and
/// reports the classes that fired on the completion (m36 chunk 1).
pub struct Redacting(pub Box<dyn TextBackend>);

impl TextBackend for Redacting {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn model(&self) -> &str {
        self.0.model()
    }

    fn context_tokens(&self) -> usize {
        self.0.context_tokens()
    }

    fn complete(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<Completion> {
        let mut classes = std::collections::BTreeSet::new();
        let mut take = |text: &str| {
            let r = redact(text);
            classes.extend(r.classes);
            r.text
        };
        let system = req.system.map(&mut take);
        let user = take(req.user);
        let history: Vec<(String, String)> = req
            .history
            .iter()
            .map(|(q, a)| (take(q), take(a)))
            .collect();
        let clean = Request {
            job: req.job,
            system: system.as_deref(),
            user: &user,
            history: &history,
            schema: req.schema,
            max_output: req.max_output,
        };
        let mut c = self.0.complete(&clean, on_token)?;
        c.redactions = classes.into_iter().collect();
        Ok(c)
    }

    fn batch(&self, reqs: &[Request<'_>]) -> anyhow::Result<Vec<anyhow::Result<Completion>>> {
        let mut classes = std::collections::BTreeSet::new();
        let mut take = |text: &str| {
            let r = redact(text);
            classes.extend(r.classes);
            r.text
        };
        let owned: Vec<(Option<String>, String, Vec<(String, String)>)> = reqs
            .iter()
            .map(|req| {
                (
                    req.system.map(&mut take),
                    take(req.user),
                    req.history
                        .iter()
                        .map(|(q, a)| (take(q), take(a)))
                        .collect(),
                )
            })
            .collect();
        let clean: Vec<Request<'_>> = reqs
            .iter()
            .zip(&owned)
            .map(|(req, (system, user, history))| Request {
                job: req.job,
                system: system.as_deref(),
                user,
                history,
                schema: req.schema,
                max_output: req.max_output,
            })
            .collect();
        let classes: Vec<crate::redact::Class> = classes.into_iter().collect();
        Ok(self
            .0
            .batch(&clean)?
            .into_iter()
            .map(|r| {
                r.map(|mut c| {
                    c.redactions = classes.clone();
                    c
                })
            })
            .collect())
    }
}

/// Why a cloud call failed, classified so the caller can decide between
/// retry, fall back to local, and give up. Messages carry the provider's
/// `error.message` at most; never the request or the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    /// 401/403: the key is wrong or lacks permission. Never retried.
    Auth(u16, String),
    /// 400/404/413: our request shape. Never retried.
    BadRequest(u16, String),
    /// 429 after retries.
    RateLimited(String),
    /// 5xx/529 after retries.
    Server(u16, String),
    /// DNS, connect, timeout, mid-stream disconnect, after retries.
    Transport(String),
    /// The model stopped for a reason other than `end_turn`
    /// (`max_tokens`, `refusal`), or streamed an error event.
    Stopped(String),
}

impl CloudError {
    /// A transient failure: a later job may succeed without any change.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            CloudError::RateLimited(_) | CloudError::Server(..) | CloudError::Transport(_)
        )
    }

    /// The one-line reason stored on the job (`cloud: …`) and shown in
    /// Settings.
    pub fn brief(&self) -> String {
        match self {
            CloudError::Auth(code, m) => format!("auth {code}: {m}"),
            CloudError::BadRequest(code, m) => format!("request rejected {code}: {m}"),
            CloudError::RateLimited(m) => format!("rate limited: {m}"),
            CloudError::Server(code, m) => format!("provider error {code}: {m}"),
            CloudError::Transport(m) => format!("unreachable: {m}"),
            CloudError::Stopped(m) => format!("stopped: {m}"),
        }
    }
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cloud: {}", self.brief())
    }
}

impl std::error::Error for CloudError {}

/// List prices per million tokens, `(input, output)`, checked 2026-09-04.
/// Cache reads bill at a tenth of input. Unknown models cost `None`, and
/// the daily cap cannot apply to them (Settings says so).
pub fn price_per_mtok(model: &str) -> Option<(f64, f64)> {
    let m = model.trim().to_ascii_lowercase();
    let table: &[(&str, (f64, f64))] = &[
        ("claude-fable-5", (10.0, 50.0)),
        ("claude-opus-5", (5.0, 25.0)),
        ("claude-opus-4", (5.0, 25.0)),
        ("claude-sonnet-5", (2.0, 10.0)),
        ("claude-sonnet-4", (3.0, 15.0)),
        ("claude-haiku-4", (1.0, 5.0)),
    ];
    table
        .iter()
        .find(|(prefix, _)| m.starts_with(prefix))
        .map(|(_, p)| *p)
}

pub fn cost_usd(model: &str, c: &Completion) -> Option<f64> {
    if let Some(reported) = c.cost_usd {
        return Some(reported);
    }
    let (inp, out) = price_per_mtok(model)?;
    let fresh = c.input_tokens.saturating_sub(c.cache_read_tokens) as f64;
    let cached = c.cache_read_tokens as f64;
    let generated = c.output_tokens as f64;
    Some((fresh * inp + cached * inp * 0.1 + generated * out) / 1_000_000.0)
}

/// Output headroom per job on a cloud model. The local `MAX_GEN` values are
/// 4B-sized; a frontier model writes a little longer and JSON never needs
/// to be squeezed.
pub fn max_output_for(job: JobKind) -> u32 {
    match job {
        JobKind::Chat => 4096,
        JobKind::Narrative | JobKind::Standup | JobKind::Journal | JobKind::TaskDescription => 1024,
        JobKind::Derive | JobKind::Consolidate => 2048,
        JobKind::Checkpoint | JobKind::SuggestTask | JobKind::NameTask | JobKind::Live => 600,
        JobKind::Advise => 300,
    }
}

/// Thinking depth per job: naming and descriptions are lookups over a
/// short digest; chat and narrative are where a frontier model earns its
/// cost.
pub fn effort_for(job: JobKind) -> &'static str {
    match job {
        JobKind::Chat | JobKind::Narrative | JobKind::Standup => "high",
        JobKind::Derive | JobKind::Consolidate | JobKind::Journal => "medium",
        _ => "low",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_and_cost() {
        assert_eq!(price_per_mtok("claude-opus-5"), Some((5.0, 25.0)));
        assert_eq!(price_per_mtok("claude-haiku-4-5"), Some((1.0, 5.0)));
        assert_eq!(price_per_mtok("gpt-5"), None);
        let c = Completion {
            input_tokens: 1_000_000,
            cache_read_tokens: 500_000,
            output_tokens: 100_000,
            ..Default::default()
        };
        // 0.5M fresh at $5 + 0.5M cached at $0.5 + 0.1M out at $25.
        let usd = cost_usd("claude-opus-5", &c).unwrap();
        assert!((usd - (2.5 + 0.25 + 2.5)).abs() < 1e-9, "{usd}");
        assert_eq!(cost_usd("mystery", &c), None);
    }

    /// A backend that records what it was asked.
    struct Echo(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl TextBackend for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn model(&self) -> &str {
            "claude-sonnet-5"
        }
        fn context_tokens(&self) -> usize {
            1000
        }
        fn complete(
            &self,
            req: &Request<'_>,
            _on_token: &mut dyn FnMut(&str),
        ) -> anyhow::Result<Completion> {
            let mut seen = self.0.lock().unwrap();
            seen.push(req.system.unwrap_or("").to_owned());
            seen.push(req.user.to_owned());
            seen.extend(req.history.iter().flat_map(|(q, a)| [q.clone(), a.clone()]));
            Ok(Completion {
                text: "ok".into(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn redacting_wrapper_cleans_every_field_and_reports_classes() {
        use crate::redact::Class;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let echo = Redacting(Box::new(Echo(seen.clone())));
        let history = vec![(
            "was it https://a.io/x?k=1".to_owned(),
            "token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123".to_owned(),
        )];
        let req = Request {
            job: JobKind::Chat,
            system: Some("rules sk-abcdefghijklmnopqrst"),
            user: "AKIAIOSFODNN7EXAMPLE now",
            history: &history,
            schema: None,
            max_output: 0,
        };
        let c = echo.complete(&req, &mut |_| {}).unwrap();
        assert_eq!(
            c.redactions,
            [
                Class::AwsKey,
                Class::ApiKey,
                Class::GithubToken,
                Class::UrlQuery
            ]
        );
        assert_eq!(c.text, "ok");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "rules [api key]",
                "[aws key] now",
                "was it https://a.io/x",
                "token [github token]",
            ]
        );
    }

    #[test]
    fn transient_classification() {
        assert!(CloudError::RateLimited("x".into()).is_transient());
        assert!(!CloudError::Auth(401, "x".into()).is_transient());
        assert_eq!(
            CloudError::Auth(401, "bad key".into()).to_string(),
            "cloud: auth 401: bad key"
        );
    }
}
