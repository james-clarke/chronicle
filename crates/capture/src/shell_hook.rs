//! Shell hook route (m37 chunk 1): `chronicle shell-init <shell>` prints a
//! snippet that POSTs `{cwd, program, started_ms, ended_ms}` to
//! `/api/chronicle/shell` after every command — cwd, program name and
//! duration only, never the command line. `HookFold` turns the posted
//! stream into `shell` spans exactly like `shell.rs`'s atuin fold, but
//! keyed by place (the git root's basename, or the cwd's basename) rather
//! than a configured repo, so any directory files, not only `git_repos`
//! entries.

use std::collections::HashMap;
use std::path::Path;

use chronicle_core::types::{ActivityEvent, ActivityKind, ms_to_ts};
use serde::Deserialize;

use crate::shell;

/// The place a filesystem path names: the basename of the nearest ancestor
/// (inclusive of `cwd` itself) that holds a `.git` entry, else `cwd`'s own
/// basename. The filesystem root names itself `/`.
pub fn place_of(cwd: &Path) -> String {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return basename(d);
        }
        dir = d.parent();
    }
    basename(cwd)
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_owned())
}

/// One `precmd` post: what left the shell. No command line, by construction.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ShellPost {
    pub cwd: String,
    pub program: String,
    pub started_ms: i64,
    pub ended_ms: i64,
}

/// One open span per place, folded from posted commands the way `shell.rs`'s
/// `Fold` folds atuin rows per configured repo.
#[derive(Default)]
pub struct HookFold {
    open: HashMap<String, Span>,
}

struct Span {
    place: String,
    start_ms: i64,
    last_ms: i64,
    counts: HashMap<String, usize>,
}

impl Span {
    fn event(&self) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(self.start_ms),
            end_ts: Some(ms_to_ts(self.last_ms)),
            repo: self.place.clone(),
            branch: String::new(),
            kind: ActivityKind::Shell,
            ext_id: Some(format!("shellhook:{}:{}", self.place, self.start_ms)),
            summary: Some(shell::summarize_counts(&self.counts)),
            detail: None,
        }
    }
}

impl HookFold {
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one post into its place's open span (starting a new one, and
    /// closing the old one, if the gap since it was last touched is too
    /// wide) and closes any other place idle past the gap. Returns every
    /// event to store: closed spans, then the touched place's span,
    /// refreshed.
    pub fn push(&mut self, post: ShellPost, now_ms: i64) -> Vec<ActivityEvent> {
        let place = place_of(Path::new(&post.cwd));
        let mut out = Vec::new();
        let extend = self
            .open
            .get(&place)
            .is_some_and(|s| post.started_ms - s.last_ms <= shell::GAP_MS);
        if extend {
            let s = self.open.get_mut(&place).expect("checked above");
            s.last_ms = s.last_ms.max(post.ended_ms);
            *s.counts.entry(post.program.clone()).or_default() += 1;
        } else {
            if let Some(prev) = self.open.remove(&place) {
                out.push(prev.event());
            }
            self.open.insert(
                place.clone(),
                Span {
                    place: place.clone(),
                    start_ms: post.started_ms,
                    last_ms: post.ended_ms.max(post.started_ms),
                    counts: HashMap::from([(post.program.clone(), 1)]),
                },
            );
        }
        let stale: Vec<String> = self
            .open
            .iter()
            .filter(|(k, s)| *k != &place && now_ms - s.last_ms > shell::GAP_MS)
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            if let Some(s) = self.open.remove(&k) {
                out.push(s.event());
            }
        }
        if let Some(s) = self.open.get(&place) {
            out.push(s.event());
        }
        out
    }

    /// Closes every span idle past the gap, for a periodic sweep between
    /// posts (a quiet terminal must not hold its span open forever).
    pub fn flush(&mut self, now_ms: i64) -> Vec<ActivityEvent> {
        let stale: Vec<String> = self
            .open
            .iter()
            .filter(|(_, s)| now_ms - s.last_ms > shell::GAP_MS)
            .map(|(k, _)| k.clone())
            .collect();
        stale
            .into_iter()
            .filter_map(|k| self.open.remove(&k).map(|s| s.event()))
            .collect()
    }
}

fn route(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/chronicle/shell")
}

const HEADER: &str =
    "# chronicle shell hook — cwd, program name and duration only; never the command line\n";

/// The hook script text for a shell's rc file, or `None` for an
/// unsupported shell. Silent, never blocks the prompt, no-ops if `curl` is
/// missing.
pub fn shell_init(shell: &str, port: u16) -> Option<String> {
    match shell {
        "zsh" => Some(zsh_script(port)),
        "bash" => Some(bash_script(port)),
        "fish" => Some(fish_script(port)),
        "pwsh" => Some(pwsh_script(port)),
        _ => None,
    }
}

fn zsh_script(port: u16) -> String {
    const TPL: &str = r#"if (( ${+commands[curl]} )); then
  autoload -Uz add-zsh-hook
  typeset -g _chronicle_shell_ran=0
  typeset -g _chronicle_shell_start=0
  typeset -g _chronicle_shell_prog=""
  chronicle_shell_preexec() {
    _chronicle_shell_ran=1
    _chronicle_shell_start=$(( $(date +%s%N) / 1000000 ))
    _chronicle_shell_prog=${${(z)1}[1]:t}
  }
  chronicle_shell_precmd() {
    if (( _chronicle_shell_ran )); then
      _chronicle_shell_ran=0
      local _chronicle_end=$(( $(date +%s%N) / 1000000 ))
      local _chronicle_cwd=$PWD
      local _chronicle_prog=$_chronicle_shell_prog
      local _chronicle_start=$_chronicle_shell_start
      printf '{"cwd":"%s","program":"%s","started_ms":%s,"ended_ms":%s}' "$_chronicle_cwd" "$_chronicle_prog" "$_chronicle_start" "$_chronicle_end" | curl -s -m 1 -o /dev/null -H 'Content-Type: application/json' --data-binary @- __ROUTE__ &!
    fi
  }
  add-zsh-hook preexec chronicle_shell_preexec
  add-zsh-hook precmd chronicle_shell_precmd
fi
"#;
    format!("{HEADER}{}", TPL.replace("__ROUTE__", &route(port)))
}

fn bash_script(port: u16) -> String {
    const TPL: &str = r#"if command -v curl >/dev/null 2>&1; then
  _chronicle_shell_ran=0
  _chronicle_shell_in=0
  _chronicle_shell_preexec() {
    (( _chronicle_shell_in )) && return
    case "$BASH_COMMAND" in
      _chronicle_shell_precmd*) return ;;
    esac
    _chronicle_shell_in=1
    _chronicle_shell_ran=1
    _chronicle_shell_start=$(( $(date +%s%N) / 1000000 ))
    set -- $BASH_COMMAND
    _chronicle_shell_prog=$(basename -- "$1" 2>/dev/null)
    _chronicle_shell_in=0
  }
  trap '_chronicle_shell_preexec' DEBUG
  _chronicle_shell_precmd() {
    local _chronicle_status=$?
    if (( _chronicle_shell_ran )); then
      _chronicle_shell_ran=0
      local _chronicle_end=$(( $(date +%s%N) / 1000000 ))
      local _chronicle_cwd="$PWD"
      local _chronicle_prog="$_chronicle_shell_prog"
      local _chronicle_start="$_chronicle_shell_start"
      printf '{"cwd":"%s","program":"%s","started_ms":%s,"ended_ms":%s}' "$_chronicle_cwd" "$_chronicle_prog" "$_chronicle_start" "$_chronicle_end" | curl -s -m 1 -o /dev/null -H 'Content-Type: application/json' --data-binary @- __ROUTE__ & disown
    fi
    return $_chronicle_status
  }
  PROMPT_COMMAND="_chronicle_shell_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#;
    format!("{HEADER}{}", TPL.replace("__ROUTE__", &route(port)))
}

fn fish_script(port: u16) -> String {
    const TPL: &str = r#"if type -q curl
    function _chronicle_shell_preexec --on-event fish_preexec
        set -g _chronicle_shell_ran 1
        set -g _chronicle_shell_start (math (date +%s%N) / 1000000)
        set -l _chronicle_words (string split ' ' -- $argv[1])
        set -g _chronicle_shell_prog (basename -- $_chronicle_words[1])
    end
    function _chronicle_shell_postexec --on-event fish_postexec
        if test "$_chronicle_shell_ran" = 1
            set -g _chronicle_shell_ran 0
            set -l _chronicle_end (math (date +%s%N) / 1000000)
            set -l _chronicle_cwd $PWD
            set -l _chronicle_prog $_chronicle_shell_prog
            set -l _chronicle_start $_chronicle_shell_start
            printf '{"cwd":"%s","program":"%s","started_ms":%s,"ended_ms":%s}' "$_chronicle_cwd" "$_chronicle_prog" "$_chronicle_start" "$_chronicle_end" | curl -s -m 1 -o /dev/null -H 'Content-Type: application/json' --data-binary @- __ROUTE__ &; disown
        end
    end
end
"#;
    format!("{HEADER}{}", TPL.replace("__ROUTE__", &route(port)))
}

fn pwsh_script(port: u16) -> String {
    const TPL: &str = r#"if (Get-Command curl -ErrorAction SilentlyContinue) {
    $global:_chronicleShellRan = $false
    $global:_chronicleShellStart = 0
    $global:_chronicleShellProg = ""
    function global:_ChronicleShellPreexec {
        param([string]$Line)
        $global:_chronicleShellRan = $true
        $global:_chronicleShellStart = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $first = ($Line.Trim() -split '\s+')[0]
        $global:_chronicleShellProg = Split-Path -Leaf $first
    }
    if (Get-Module -Name PSReadLine -ErrorAction SilentlyContinue) {
        Set-PSReadLineOption -AddToHistoryHandler {
            param($line)
            _ChronicleShellPreexec $line
            return $true
        }
    }
    function global:prompt {
        if ($global:_chronicleShellRan) {
            $global:_chronicleShellRan = $false
            $end = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            $cwd = (Get-Location).Path
            $prog = $global:_chronicleShellProg
            $start = $global:_chronicleShellStart
            $body = '{"cwd":"' + $cwd.Replace('\','\\').Replace('"','\"') + '","program":"' + $prog + '","started_ms":' + $start + ',"ended_ms":' + $end + '}'
            Start-Job -ScriptBlock {
                param($b)
                curl -s -m 1 -o NUL -H 'Content-Type: application/json' --data-binary $b __ROUTE__ 2>$null
            } -ArgumentList $body | Out-Null
        }
        "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
    }
}
"#;
    format!("{HEADER}{}", TPL.replace("__ROUTE__", &route(port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_init_none_for_unknown_shell() {
        assert!(shell_init("nu", 4177).is_none());
        assert!(shell_init("csh", 4177).is_none());
    }

    /// Every script's JSON payload line names only the derived cwd/program/
    /// timing variables — never the raw command text the shell handed the
    /// hook (`$1`, `$BASH_COMMAND`, `$argv`, `$Line`).
    fn payload_line(script: &str) -> &str {
        script
            .lines()
            .find(|l| l.contains("--data-binary"))
            .expect("a data-binary line")
    }

    #[test]
    fn zsh_script_posts_route_and_never_the_raw_command() {
        let s = shell_init("zsh", 4177).unwrap();
        assert!(s.starts_with(
            "# chronicle shell hook — cwd, program name and duration only; never the command line"
        ));
        assert!(s.contains("http://127.0.0.1:4177/api/chronicle/shell"));
        assert!(s.contains("add-zsh-hook preexec"));
        assert!(s.contains("add-zsh-hook precmd"));
        assert!(s.contains("&!"), "zsh detaches with &!");
        let payload = payload_line(&s);
        assert!(!payload.contains("$1"));
        assert!(!payload.contains("BASH_COMMAND"));
    }

    #[test]
    fn bash_script_posts_route_and_never_the_raw_command() {
        let s = shell_init("bash", 4177).unwrap();
        assert!(s.contains("http://127.0.0.1:4177/api/chronicle/shell"));
        assert!(s.contains("trap '_chronicle_shell_preexec' DEBUG"));
        assert!(s.contains("PROMPT_COMMAND="));
        assert!(s.contains("& disown"), "bash detaches with & disown");
        let payload = payload_line(&s);
        assert!(!payload.contains("$1"));
        assert!(!payload.contains("BASH_COMMAND"));
    }

    #[test]
    fn fish_script_posts_route_and_never_the_raw_command() {
        let s = shell_init("fish", 4177).unwrap();
        assert!(s.contains("http://127.0.0.1:4177/api/chronicle/shell"));
        assert!(s.contains("--on-event fish_preexec"));
        assert!(s.contains("--on-event fish_postexec"));
        assert!(s.contains("&; disown"), "fish detaches with &; disown");
        let payload = payload_line(&s);
        assert!(!payload.contains("$argv"));
    }

    #[test]
    fn pwsh_script_posts_route_and_never_the_raw_command() {
        let s = shell_init("pwsh", 4177).unwrap();
        assert!(s.contains("http://127.0.0.1:4177/api/chronicle/shell"));
        assert!(s.contains("Start-Job"));
        let payload = payload_line(&s);
        assert!(!payload.contains("$Line"));
    }

    #[test]
    fn place_of_finds_the_git_root_basename() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("dev").join("chronicle");
        let crates_dir = repo.join("crates").join("capture");
        std::fs::create_dir_all(&crates_dir).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(place_of(&repo), "chronicle");
        assert_eq!(place_of(&crates_dir), "chronicle");

        let outside = tmp.path().join("scratch");
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(place_of(&outside), "scratch");

        assert_eq!(place_of(Path::new("/")), "/");
    }

    fn post(cwd: &str, program: &str, started_ms: i64, ended_ms: i64) -> ShellPost {
        ShellPost {
            cwd: cwd.to_owned(),
            program: program.to_owned(),
            started_ms,
            ended_ms,
        }
    }

    #[test]
    fn hook_fold_groups_two_cwds_in_one_repo_into_one_place_span() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("chronicle");
        let crates_dir = repo.join("crates");
        std::fs::create_dir_all(&crates_dir).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let mut fold = HookFold::new();
        let t = 1_788_451_000_000;
        let repo_s = repo.to_string_lossy().into_owned();
        let crates_s = crates_dir.to_string_lossy().into_owned();

        fold.push(post(&repo_s, "cargo", t, t + 500), t + 500);
        let events = fold.push(post(&crates_s, "git", t + 60_000, t + 60_500), t + 60_500);

        assert_eq!(events.len(), 1, "same place, still one open span");
        let ev = &events[0];
        assert_eq!(ev.repo, "chronicle");
        assert_eq!(ev.kind, ActivityKind::Shell);
        assert_eq!(
            ev.ext_id.as_deref(),
            Some(format!("shellhook:chronicle:{t}").as_str())
        );
        assert_eq!(
            ev.summary.as_deref(),
            Some("cargo \u{d7}1 \u{b7} git \u{d7}1")
        );
        assert_eq!(ev.ts, ms_to_ts(t));
        assert_eq!(ev.end_ts, Some(ms_to_ts(t + 60_500)));
    }

    #[test]
    fn hook_fold_splits_on_a_gap() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("chronicle");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let repo_s = repo.to_string_lossy().into_owned();

        let mut fold = HookFold::new();
        let t = 1_788_451_000_000;
        fold.push(post(&repo_s, "cargo", t, t), t);

        let late = t + shell::GAP_MS + 60_000;
        let events = fold.push(post(&repo_s, "git", late, late), late);

        assert_eq!(
            events.len(),
            2,
            "the closed cargo span, then the open git span"
        );
        assert_eq!(events[0].summary.as_deref(), Some("cargo \u{d7}1"));
        assert_eq!(
            events[0].ext_id.as_deref(),
            Some(format!("shellhook:chronicle:{t}").as_str())
        );
        assert_eq!(events[1].summary.as_deref(), Some("git \u{d7}1"));
        assert_eq!(
            events[1].ext_id.as_deref(),
            Some(format!("shellhook:chronicle:{late}").as_str())
        );
    }

    #[test]
    fn hook_fold_uses_the_basename_outside_any_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let scratch_s = scratch.to_string_lossy().into_owned();

        let mut fold = HookFold::new();
        let t = 1_788_451_000_000;
        let events = fold.push(post(&scratch_s, "ls", t, t), t);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].repo, "scratch");
    }

    #[test]
    fn flush_closes_idle_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("chronicle");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let repo_s = repo.to_string_lossy().into_owned();

        let mut fold = HookFold::new();
        let t = 1_788_451_000_000;
        fold.push(post(&repo_s, "cargo", t, t), t);

        let closed = fold.flush(t + shell::GAP_MS + 1);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].summary.as_deref(), Some("cargo \u{d7}1"));
        assert!(
            fold.flush(t + shell::GAP_MS + 2).is_empty(),
            "already closed"
        );
    }
}
