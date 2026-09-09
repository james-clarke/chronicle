//! Redaction of cloud-bound text (m36 chunk 1). Every `Request` a cloud
//! backend sends passes through [`redact`] first: URL query strings and
//! fragments go, path segments that look like tokens go, and the common
//! secret shapes (AWS keys, `sk-` API keys, GitHub tokens, JWTs, URL
//! credentials, bearer tokens) are replaced by a class marker. The classes
//! that fired are reported so the Settings egress line can name them.
//! Shell command lines never need this: only program names reach the
//! digest, by construction of the sessionizer.

use std::sync::LazyLock;

use regex::Regex;

/// One redaction class, in the order the passes run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    Jwt,
    AwsKey,
    ApiKey,
    GithubToken,
    Bearer,
    UrlCredentials,
    UrlQuery,
    PathToken,
}

impl Class {
    pub const ALL: [Class; 8] = [
        Class::Jwt,
        Class::AwsKey,
        Class::ApiKey,
        Class::GithubToken,
        Class::Bearer,
        Class::UrlCredentials,
        Class::UrlQuery,
        Class::PathToken,
    ];

    /// The name shown in Settings and stored in meta.
    pub fn label(self) -> &'static str {
        match self {
            Class::Jwt => "JWTs",
            Class::AwsKey => "AWS keys",
            Class::ApiKey => "API keys",
            Class::GithubToken => "GitHub tokens",
            Class::Bearer => "bearer tokens",
            Class::UrlCredentials => "URL credentials",
            Class::UrlQuery => "URL query strings",
            Class::PathToken => "opaque path tokens",
        }
    }

    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.label() == s)
    }
}

/// The text with every match replaced, and the classes that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub classes: Vec<Class>,
}

static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}").unwrap()
});
static AWS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap());
static API_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}").unwrap());
static GITHUB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{20,}").unwrap());
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}").unwrap());
/// `scheme://user:pass@host` and `scheme://token@host`.
static URL_CREDENTIALS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([a-z][a-z0-9+.-]*://)[^\s/@]+@").unwrap());
/// A URL with a scheme, or a bare `host.tld/path`, followed by `?` or `#`.
static URL_QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\b[a-z][a-z0-9+.-]*://[^\s?#]+|\b[a-z0-9.-]+\.[a-z]{2,}/[^\s?#]*)[?#]\S*")
        .unwrap()
});
/// A path segment of 20+ base64/hex characters; [`looks_like_token`]
/// keeps only the ones that read as an opaque token, not a slug.
static PATH_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/([A-Za-z0-9+_=-]{20,})").unwrap());

/// All hex (a hash), or base64-shaped: mixed case, four or more digits and
/// no hyphen, so `m35-project-first-plan` and `ACME-11533-export` stay.
fn looks_like_token(s: &str) -> bool {
    if s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    let upper = s.chars().any(|c| c.is_ascii_uppercase());
    let lower = s.chars().any(|c| c.is_ascii_lowercase());
    !s.contains('-') && digits >= 4 && upper && lower
}

pub fn redact(text: &str) -> Redacted {
    let mut classes = Vec::new();
    let mut out = std::borrow::Cow::Borrowed(text);
    let passes: [(Class, &Regex, &str); 7] = [
        (Class::Jwt, &JWT, "[jwt]"),
        (Class::AwsKey, &AWS, "[aws key]"),
        (Class::ApiKey, &API_KEY, "[api key]"),
        (Class::GithubToken, &GITHUB, "[github token]"),
        (Class::Bearer, &BEARER, "$1 [token]"),
        (Class::UrlCredentials, &URL_CREDENTIALS, "$1[credentials]@"),
        (Class::UrlQuery, &URL_QUERY, "$1"),
    ];
    for (class, re, rep) in passes {
        if re.is_match(&out) {
            classes.push(class);
            out = std::borrow::Cow::Owned(re.replace_all(&out, rep).into_owned());
        }
    }
    let mut hit = false;
    let replaced = PATH_TOKEN.replace_all(&out, |caps: &regex::Captures<'_>| {
        if looks_like_token(&caps[1]) {
            hit = true;
            "/[token]".to_owned()
        } else {
            caps[0].to_owned()
        }
    });
    if hit {
        classes.push(Class::PathToken);
        out = std::borrow::Cow::Owned(replaced.into_owned());
    }
    Redacted {
        text: out.into_owned(),
        classes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> (String, Vec<Class>) {
        let r = redact(text);
        (r.text, r.classes)
    }

    #[test]
    fn clean_text_is_untouched() {
        let digest = "- 0\u{2013}3m Code: cart.py \u{2014} shop\n- 3\u{2013}4m Firefox: Stripe API reference \u{2014} Mozilla Firefox\n- 4\u{2013}9m Terminal: cargo test: 42 passed\nWhat now? See #payments and internationalization/README.md\n";
        assert_eq!(one(digest), (digest.to_owned(), vec![]));
    }

    #[test]
    fn secrets_by_class() {
        let (t, c) = one("key sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv here");
        assert_eq!(t, "key [api key] here");
        assert_eq!(c, [Class::ApiKey]);
        let (t, c) = one("AKIAIOSFODNN7EXAMPLE and AKIAIOSFODNN7EXAMPL");
        assert_eq!(t, "[aws key] and AKIAIOSFODNN7EXAMPL");
        assert_eq!(c, [Class::AwsKey]);
        let (t, c) = one("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123 ghs_short");
        assert_eq!(t, "[github token] ghs_short");
        assert_eq!(c, [Class::GithubToken]);
        let (t, c) = one("Authorization: Bearer abc.def-ghi_123 done");
        assert_eq!(t, "Authorization: Bearer [token] done");
        assert_eq!(c, [Class::Bearer]);
        let (t, c) = one(
            "tok eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c end",
        );
        assert_eq!(t, "tok [jwt] end");
        assert_eq!(c, [Class::Jwt]);
    }

    #[test]
    fn urls_lose_credentials_queries_and_tokens() {
        let (t, c) = one("DATABASE_URL=postgres://app:s3cret@db.internal:5432/prod");
        assert_eq!(
            t,
            "DATABASE_URL=postgres://[credentials]@db.internal:5432/prod"
        );
        assert_eq!(c, [Class::UrlCredentials]);
        let (t, c) = one("open https://example.com/a/b?token=abc&x=1#frag then");
        assert_eq!(t, "open https://example.com/a/b then");
        assert_eq!(c, [Class::UrlQuery]);
        let (t, c) = one("github.com/org/repo/pull/12?diff=split \u{2014} Firefox");
        assert_eq!(t, "github.com/org/repo/pull/12 \u{2014} Firefox");
        assert_eq!(c, [Class::UrlQuery]);
        let (t, c) = one("https://x.dev/reset/9f8e7d6c5b4a39281706f5e4d3c2b1a0/confirm");
        assert_eq!(t, "https://x.dev/reset/[token]/confirm");
        assert_eq!(c, [Class::PathToken]);
        let (t, c) = one("app.dev/s/AbCdEfGhIjKlMnOpQrStUvWxYz0123/edit");
        assert_eq!(t, "app.dev/s/[token]/edit");
        assert_eq!(c, [Class::PathToken]);
        // Long words, slugs and branch names in paths are not tokens.
        for keep in [
            "/src/internationalization/index.ts",
            "docs/plans/m35-project-first-plan.md",
            "github.com/o/r/tree/ACME-11533-export-selected-modal",
            "/home/james/.local/share/chronicle_2026_backup_copy/",
        ] {
            assert_eq!(one(keep), (keep.to_owned(), vec![]), "{keep}");
        }
    }

    #[test]
    fn classes_are_ordered_and_named() {
        let (t, c) = one("https://u:p@h.io/x?y=1 sk-abcdefghijklmnopqrst");
        assert_eq!(t, "https://[credentials]@h.io/x [api key]");
        assert_eq!(c, [Class::ApiKey, Class::UrlCredentials, Class::UrlQuery]);
        for k in Class::ALL {
            assert_eq!(Class::parse(k.label()), Some(k));
        }
    }
}
