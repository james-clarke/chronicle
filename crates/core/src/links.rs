//! Project-config link discovery (m37 chunk 2): read the local marker files
//! a repo's deploy/observability tooling leaves behind, so onboarding can
//! propose dashboard links without ever touching a secret. Every reader here
//! extracts names and ids only (never a key, token, or env value) and never
//! fails on a malformed or unreadable file — it just yields nothing for that
//! kind.

use std::path::Path;

/// One discovered project link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoLink {
    pub kind: &'static str,
    pub name: String,
    pub url_pattern: Option<String>,
}

/// Every link a repo's marker files carry. Order: hosting/deploy, infra,
/// CI, then the build system(s).
pub fn read_links(repo_root: &Path) -> Vec<RepoLink> {
    let mut out = Vec::new();
    out.extend(vercel(repo_root));
    out.extend(netlify(repo_root));
    out.extend(fly(repo_root));
    out.extend(render(repo_root));
    out.extend(railway(repo_root));
    out.extend(wrangler(repo_root));
    out.extend(supabase(repo_root));
    out.extend(doppler(repo_root));
    out.extend(sentry(repo_root));
    out.extend(circleci(repo_root));
    out.extend(buildkite(repo_root));
    out.extend(build_systems(repo_root));
    out
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn read_toml(path: &Path) -> Option<toml::Value> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn vercel(root: &Path) -> Vec<RepoLink> {
    let Some(v) = read_json(&root.join(".vercel/project.json")) else {
        return Vec::new();
    };
    let project_name = v.get("projectName").and_then(|x| x.as_str());
    let Some(name) = project_name
        .or_else(|| v.get("projectId").and_then(|x| x.as_str()))
        .map(str::to_owned)
    else {
        return Vec::new();
    };
    // orgId (if present) has no slug we can put in a URL, so the org
    // segment of the pattern stays a wildcard.
    let url_pattern = project_name.map(|n| format!("vercel.com/*/{n}"));
    vec![RepoLink {
        kind: "vercel",
        name,
        url_pattern,
    }]
}

fn netlify(root: &Path) -> Vec<RepoLink> {
    let Some(v) = read_json(&root.join(".netlify/state.json")) else {
        return Vec::new();
    };
    let Some(site_id) = v.get("siteId").and_then(|x| x.as_str()) else {
        return Vec::new();
    };
    // Netlify's dashboard URL is keyed by site name, not id; only emit a
    // pattern when we have evidence the site actually has one.
    let url_pattern = v
        .get("siteName")
        .and_then(|x| x.as_str())
        .map(|_| format!("app.netlify.com/sites/{site_id}"));
    vec![RepoLink {
        kind: "netlify",
        name: site_id.to_owned(),
        url_pattern,
    }]
}

fn fly(root: &Path) -> Vec<RepoLink> {
    let Some(v) = read_toml(&root.join("fly.toml")) else {
        return Vec::new();
    };
    let Some(name) = v.get("app").and_then(|x| x.as_str()) else {
        return Vec::new();
    };
    vec![RepoLink {
        kind: "fly",
        name: name.to_owned(),
        url_pattern: Some(format!("fly.io/apps/{name}")),
    }]
}

/// Light YAML: every `name:` line (list-item or bare) inside a top-level
/// `services:` block.
fn render(root: &Path) -> Vec<RepoLink> {
    let Some(text) = std::fs::read_to_string(root.join("render.yaml")).ok() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_services = false;
    for line in text.lines() {
        if line.trim_end() == "services:" {
            in_services = true;
            continue;
        }
        if !in_services {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_services = false;
            continue;
        }
        if let Some(name) = parse_name_line(trimmed) {
            out.push(RepoLink {
                kind: "render",
                name,
                url_pattern: None,
            });
        }
    }
    out
}

fn parse_name_line(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix('-').unwrap_or(trimmed).trim_start();
    let val = rest.strip_prefix("name:")?.trim();
    let val = val.trim_matches(['"', '\'']);
    (!val.is_empty()).then(|| val.to_owned())
}

fn railway(root: &Path) -> Vec<RepoLink> {
    if !root.join("railway.json").is_file() {
        return Vec::new();
    }
    vec![RepoLink {
        kind: "railway",
        name: folder_name(root),
        url_pattern: None,
    }]
}

fn wrangler(root: &Path) -> Vec<RepoLink> {
    let Some(v) = read_toml(&root.join("wrangler.toml")) else {
        return Vec::new();
    };
    let Some(name) = v.get("name").and_then(|x| x.as_str()) else {
        return Vec::new();
    };
    vec![RepoLink {
        kind: "wrangler",
        name: name.to_owned(),
        url_pattern: Some(format!(
            "dash.cloudflare.com/*/workers/services/view/{name}"
        )),
    }]
}

fn supabase(root: &Path) -> Vec<RepoLink> {
    let Some(v) = read_toml(&root.join("supabase/config.toml")) else {
        return Vec::new();
    };
    let Some(id) = v.get("project_id").and_then(|x| x.as_str()) else {
        return Vec::new();
    };
    vec![RepoLink {
        kind: "supabase",
        name: id.to_owned(),
        url_pattern: Some(format!("supabase.com/dashboard/project/{id}")),
    }]
}

fn doppler(root: &Path) -> Vec<RepoLink> {
    let Some(text) = std::fs::read_to_string(root.join(".doppler.yaml")).ok() else {
        return Vec::new();
    };
    let project = text.lines().find_map(|l| {
        let val = l.trim().strip_prefix("project:")?.trim();
        let val = val.trim_matches(['"', '\'']);
        (!val.is_empty()).then(|| val.to_owned())
    });
    let Some(project) = project else {
        return Vec::new();
    };
    vec![RepoLink {
        kind: "doppler",
        url_pattern: Some(format!(
            "dashboard.doppler.com/workplace/*/projects/{project}"
        )),
        name: project,
    }]
}

/// `.sentryclirc` is an ini file: `[defaults]` section, `org =` / `project =`.
fn sentry(root: &Path) -> Vec<RepoLink> {
    let Some(text) = std::fs::read_to_string(root.join(".sentryclirc")).ok() else {
        return Vec::new();
    };
    let mut section = None;
    let mut org = None;
    let mut project = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = Some(name.to_owned());
            continue;
        }
        if section.as_deref() != Some("defaults") {
            continue;
        }
        if let Some((key, val)) = l.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            if key == "org" && !val.is_empty() {
                org = Some(val.to_owned());
            } else if key == "project" && !val.is_empty() {
                project = Some(val.to_owned());
            }
        }
    }
    let Some(org) = org else {
        return Vec::new();
    };
    let name = match project {
        Some(p) => format!("{org}/{p}"),
        None => org.clone(),
    };
    vec![RepoLink {
        kind: "sentry",
        name,
        url_pattern: Some(format!("{org}.sentry.io")),
    }]
}

fn circleci(root: &Path) -> Vec<RepoLink> {
    if !root.join(".circleci/config.yml").is_file() {
        return Vec::new();
    }
    vec![RepoLink {
        kind: "circleci",
        name: folder_name(root),
        url_pattern: None,
    }]
}

fn buildkite(root: &Path) -> Vec<RepoLink> {
    if !root.join(".buildkite/pipeline.yml").is_file() {
        return Vec::new();
    }
    vec![RepoLink {
        kind: "buildkite",
        name: folder_name(root),
        url_pattern: None,
    }]
}

/// The build system(s) in play: one language/package-manager marker plus
/// any monorepo tool markers, each independent of the others.
fn build_systems(root: &Path) -> Vec<RepoLink> {
    let exists = |p: &str| root.join(p).is_file();
    let mut names = Vec::new();
    if exists("Cargo.toml") {
        names.push("cargo");
    }
    if exists("package.json") {
        names.push(if exists("pnpm-lock.yaml") {
            "pnpm"
        } else if exists("yarn.lock") {
            "yarn"
        } else if exists("bun.lockb") {
            "bun"
        } else {
            "npm"
        });
    }
    if exists("pom.xml") {
        names.push("maven");
    }
    if exists("build.gradle") || exists("build.gradle.kts") {
        names.push("gradle");
    }
    if exists("pyproject.toml") {
        names.push("python");
    }
    if exists("go.mod") {
        names.push("go");
    }
    if exists("Gemfile") {
        names.push("bundler");
    }
    if exists("mix.exs") {
        names.push("mix");
    }
    if exists("nx.json") {
        names.push("nx");
    }
    if exists("turbo.json") {
        names.push("turbo");
    }
    names
        .into_iter()
        .map(|name| RepoLink {
            kind: "build",
            name: name.to_owned(),
            url_pattern: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn reads_vercel_project() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vercel/project.json",
            r#"{"projectId":"prj_1","projectName":"my-app","orgId":"team_1"}"#,
        );
        let links = read_links(tmp.path());
        assert_eq!(
            links,
            vec![RepoLink {
                kind: "vercel",
                name: "my-app".into(),
                url_pattern: Some("vercel.com/*/my-app".into()),
            }]
        );
    }

    #[test]
    fn vercel_garbage_json_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), ".vercel/project.json", "{not json");
        assert!(read_links(tmp.path()).is_empty());
    }

    #[test]
    fn reads_fly_toml() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "fly.toml", "app = \"my-fly-app\"\n");
        let links = read_links(tmp.path());
        assert!(links.contains(&RepoLink {
            kind: "fly",
            name: "my-fly-app".into(),
            url_pattern: Some("fly.io/apps/my-fly-app".into()),
        }));
    }

    #[test]
    fn reads_render_services() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "render.yaml",
            "services:\n  - type: web\n    name: api\n  - type: worker\n    name: jobs\nenvVarGroups: []\n",
        );
        let links = read_links(tmp.path());
        let render: Vec<_> = links.iter().filter(|l| l.kind == "render").collect();
        assert_eq!(render.len(), 2);
        assert_eq!(render[0].name, "api");
        assert_eq!(render[1].name, "jobs");
        assert!(render[0].url_pattern.is_none());
    }

    #[test]
    fn reads_supabase_config() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "supabase/config.toml",
            "project_id = \"abcxyz\"\n",
        );
        let links = read_links(tmp.path());
        assert!(links.contains(&RepoLink {
            kind: "supabase",
            name: "abcxyz".into(),
            url_pattern: Some("supabase.com/dashboard/project/abcxyz".into()),
        }));
    }

    #[test]
    fn reads_sentry_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".sentryclirc",
            "[defaults]\norg=my-org\nproject=my-project\n\n[auth]\ntoken=SECRET\n",
        );
        let links = read_links(tmp.path());
        assert!(links.contains(&RepoLink {
            kind: "sentry",
            name: "my-org/my-project".into(),
            url_pattern: Some("my-org.sentry.io".into()),
        }));
        // Never a secret in the result.
        assert!(!links.iter().any(|l| l.name.contains("SECRET")));
    }

    #[test]
    fn detects_build_systems() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "Cargo.toml", "[package]\n");
        write(tmp.path(), "package.json", "{}");
        write(tmp.path(), "pnpm-lock.yaml", "");
        write(tmp.path(), "turbo.json", "{}");
        let links = read_links(tmp.path());
        let build: Vec<_> = links
            .iter()
            .filter(|l| l.kind == "build")
            .map(|l| l.name.as_str())
            .collect();
        assert!(build.contains(&"cargo"));
        assert!(build.contains(&"pnpm"));
        assert!(build.contains(&"turbo"));
        assert!(!build.contains(&"npm"));
    }

    #[test]
    fn no_marker_files_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_links(tmp.path()).is_empty());
    }
}
