use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
use chronicle_core::config::Config;

use crate::daemon::CtrlMsg;
use chronicle_core::types::CaptureEvent;
use crossbeam_channel::Sender;
use jiff::{Timestamp, ToSpan};
use regex::Regex;

#[cfg(target_os = "linux")]
pub(crate) fn spawn_capture(
    config: &Config,
    data_dir: &Path,
    tx: Sender<CaptureEvent>,
    ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use chronicle_capture::x11::{X11AfkProvider, X11FocusProvider};

    let focus = X11FocusProvider::new().map_err(|e| anyhow::anyhow!("X11 focus provider: {e}"))?;
    spawn_focus_thread(focus, tx.clone(), ctrl.clone())?;
    spawn_lock_capture(tx.clone(), ctrl)?;
    let afk = X11AfkProvider::new().map_err(|e| anyhow::anyhow!("X11 afk provider: {e}"))?;
    spawn_afk_thread(config, afk, tx.clone())?;
    spawn_presence_capture(config, tx.clone())?;
    spawn_common(config, data_dir, tx)
}

/// macOS (m38): the same shape as Linux with the polling providers in
/// `chronicle_capture::macos`. Without the Accessibility grant the focus
/// provider still runs (app names, empty titles) and logs once.
#[cfg(target_os = "macos")]
pub(crate) fn spawn_capture(
    config: &Config,
    data_dir: &Path,
    tx: Sender<CaptureEvent>,
    ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use chronicle_capture::macos::focus::MacFocusProvider;
    use chronicle_capture::macos::input::MacAfkProvider;

    let focus =
        MacFocusProvider::new().map_err(|e| anyhow::anyhow!("macOS focus provider: {e}"))?;
    spawn_focus_thread(focus, tx.clone(), ctrl.clone())?;
    spawn_lock_capture(tx.clone(), ctrl)?;
    let afk = MacAfkProvider::new().map_err(|e| anyhow::anyhow!("macOS afk provider: {e}"))?;
    spawn_afk_thread(config, afk, tx.clone())?;
    spawn_presence_capture(config, tx.clone())?;
    spawn_common(config, data_dir, tx)
}

/// The focus thread: the one provider that is load-bearing. Its exit
/// closes the open span (m32 chunk 0).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_focus_thread(
    focus: impl chronicle_capture::FocusProvider + 'static,
    tx: Sender<CaptureEvent>,
    ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("focus".into())
        .spawn(move || {
            let etx = tx.clone();
            if let Err(e) = focus.run(tx) {
                tracing::error!("focus provider exited: {e}");
                // Nothing watches focus any more: the open span closes here
                // instead of growing until the next restart (m32 chunk 0).
                // A hard edge like a lock — idle would only mark it quiet.
                let ts = Timestamp::now();
                let _ = etx.send(CaptureEvent::Lock { locked: true, ts });
                let _ = ctrl.send(CtrlMsg::CaptureLost {
                    reason: "provider exit",
                    ts,
                });
            }
        })?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_afk_thread(
    config: &Config,
    afk: impl chronicle_capture::AfkProvider + 'static,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    let threshold_ms = u64::from(config.afk_close_secs) * 1000;
    std::thread::Builder::new()
        .name("afk".into())
        .spawn(move || afk_loop(afk, tx, threshold_ms))?;
    Ok(())
}

/// The platform-neutral collectors, after the platform's focus, lock, idle
/// and presence threads.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_common(config: &Config, data_dir: &Path, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    spawn_git_capture(config, tx.clone())?;
    spawn_notes_capture(config, tx.clone())?;
    spawn_ai_sessions_capture(config, tx.clone())?;
    // Listener map (m32 chunk 4): which repo each local dev server is.
    spawn_provider_thread(
        "ports",
        "port map provider",
        chronicle_capture::ports::PortMapProvider::default(),
        tx.clone(),
    )?;
    spawn_github_capture(config, tx.clone())?;
    spawn_gitlab_capture(config, tx.clone())?;
    spawn_shell_capture(config, tx.clone())?;
    spawn_mic_capture(config, tx.clone())?;
    spawn_local_cwd_capture(tx.clone())?;
    spawn_browser_capture(config, tx.clone())?;
    spawn_ics_capture(config, tx.clone())?;
    spawn_gcal_capture(config, data_dir, tx)
}

/// Browser history reader (m37 chunk 4): the real URL behind a tab title,
/// from the browsers' own history databases; on by default, off when no
/// profile exists.
pub(crate) fn spawn_browser_capture(
    config: &Config,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::browser::{BrowserProvider, profiles};

    if !config.browser_history {
        return Ok(());
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(());
    };
    if profiles(&home).is_empty() {
        tracing::info!("browser_history = true but no history database found");
        return Ok(());
    }
    spawn_provider_thread(
        "browser",
        "browser history provider",
        BrowserProvider::new(&home),
        tx,
    )
}

/// ICS calendar feeds (m37 chunk 4): `calendars` URLs or paths, no OAuth.
pub(crate) fn spawn_ics_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::ics::IcsProvider;

    if config.calendars.is_empty() {
        return Ok(());
    }
    let sources: Vec<String> = config
        .calendars
        .iter()
        .map(|c| {
            if c.starts_with("http://") || c.starts_with("https://") {
                c.clone()
            } else {
                chronicle_core::config::expand_home(c).display().to_string()
            }
        })
        .collect();
    let provider = IcsProvider::new(sources, jiff::tz::TimeZone::system());
    spawn_provider_thread("ics", "ics calendar provider", provider, tx)
}

/// tmux panes and Docker Compose stacks (m37 chunk 1): a live cwd per
/// attached pane and per running stack, when the tool is on PATH.
pub(crate) fn spawn_local_cwd_capture(tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    if let Some(p) = chronicle_capture::tmux::TmuxProvider::detect() {
        spawn_provider_thread("tmux", "tmux provider", p, tx.clone())?;
    }
    if let Some(p) = chronicle_capture::docker::DockerProvider::detect() {
        spawn_provider_thread("docker", "docker provider", p, tx)?;
    }
    Ok(())
}

/// MR poller via the user's `glab` (m37 chunk 2): opt-in, never
/// load-bearing.
pub(crate) fn spawn_gitlab_capture(
    config: &Config,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::gitlab::GitlabProvider;

    if !config.gitlab_mrs {
        return Ok(());
    }
    let Some(glab) = GitlabProvider::detect() else {
        tracing::warn!("gitlab_mrs = true but `glab` is not on PATH");
        return Ok(());
    };
    let repos: Vec<PathBuf> = config
        .git_repos
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let provider = GitlabProvider::new(glab, &repos);
    if provider.is_empty() {
        tracing::warn!("gitlab_mrs = true but no configured repo has a GitLab remote");
        return Ok(());
    }
    spawn_provider_thread("gitlab", "gitlab provider", provider, tx)
}

/// logind lock edges (m32 chunk 0): a lock is AFK from that moment and ends
/// the capture ledger row; an unlock resumes both. Optional, never
/// load-bearing.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_lock_capture(
    tx: Sender<CaptureEvent>,
    ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use chronicle_capture::lock::LogindLock;

    match LogindLock::new() {
        Ok(lock) => spawn_lock_thread(lock, tx, ctrl),
        Err(e) => {
            tracing::warn!("lock signal unavailable: {e}");
            Ok(())
        }
    }
}

/// Session lock flag (m38): polled, never load-bearing.
#[cfg(target_os = "macos")]
pub(crate) fn spawn_lock_capture(
    tx: Sender<CaptureEvent>,
    ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use chronicle_capture::macos::lock::MacLock;

    match MacLock::new() {
        Ok(lock) => spawn_lock_thread(lock, tx, ctrl),
        Err(e) => {
            tracing::warn!("lock signal unavailable: {e}");
            Ok(())
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_lock_thread(
    lock: impl chronicle_capture::LockSignal + 'static,
    tx: Sender<CaptureEvent>,
    ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("lock".into())
        .spawn(move || {
            let mut locked = false;
            let result = lock.run(&mut |now_locked| {
                if now_locked == locked {
                    return;
                }
                locked = now_locked;
                let ts = Timestamp::now();
                let _ = tx.send(CaptureEvent::Lock {
                    locked: now_locked,
                    ts,
                });
                let _ = ctrl.send(if now_locked {
                    CtrlMsg::CaptureLost { reason: "lock", ts }
                } else {
                    CtrlMsg::CaptureBack(ts)
                });
            });
            if let Err(e) = result {
                tracing::error!("lock signal exited: {e}");
            }
        })?;
    Ok(())
}

/// XI2 raw-event counts per minute (m32 chunk 1): keys, buttons, motion,
/// scroll — never which. Off by config, or when the server lacks XInput 2;
/// never load-bearing.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_presence_capture(
    config: &chronicle_core::config::Config,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::presence::X11PresenceProvider;

    if !config.capture_presence {
        return Ok(());
    }
    match X11PresenceProvider::new() {
        Ok(provider) => spawn_presence_thread(provider, tx),
        Err(e) => {
            tracing::warn!("presence counts unavailable: {e}");
            Ok(())
        }
    }
}

/// CoreGraphics event counters per minute (m38): the same four counts.
#[cfg(target_os = "macos")]
pub(crate) fn spawn_presence_capture(
    config: &chronicle_core::config::Config,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::macos::input::MacPresenceProvider;

    if !config.capture_presence {
        return Ok(());
    }
    match MacPresenceProvider::new() {
        Ok(provider) => spawn_presence_thread(provider, tx),
        Err(e) => {
            tracing::warn!("presence counts unavailable: {e}");
            Ok(())
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_presence_thread(
    provider: impl chronicle_capture::PresenceProvider + 'static,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("presence".into())
        .spawn(move || {
            let result =
                provider.run(&mut |minute| tx.send(CaptureEvent::Presence(minute)).is_ok());
            if let Err(e) = result {
                tracing::error!("presence provider exited: {e}");
            }
        })?;
    Ok(())
}

/// Spawn a named thread running `provider.run(tx)`; a run failure is logged,
/// not propagated — capture providers are never load-bearing.
pub(crate) fn spawn_provider_thread(
    name: &'static str,
    what: &'static str,
    provider: impl chronicle_capture::FocusProvider + 'static,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if let Err(e) = provider.run(tx) {
                tracing::error!("{what} exited: {e}");
            }
        })?;
    Ok(())
}

/// Mic-in-use watcher via `pw-dump`: optional, never load-bearing.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_mic_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::mic::MicProvider;

    if !config.mic_capture {
        return Ok(());
    }
    let pw_dump = chronicle_core::config::resolve_command("pw-dump");
    if !pw_dump.contains('/') {
        tracing::warn!("mic_capture = true but `pw-dump` is not on PATH");
        return Ok(());
    }
    let provider = MicProvider::new(PathBuf::from(pw_dump));
    spawn_provider_thread("mic", "mic provider", provider, tx)
}

/// Git poller: optional, never load-bearing — a dead thread loses git
/// evidence, not capture.
pub(crate) fn spawn_git_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::git::GitProvider;

    let repos: Vec<PathBuf> = config
        .git_repos
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let git = GitProvider::new(&repos);
    if git.is_empty() {
        if !repos.is_empty() {
            tracing::warn!("git_repos configured but none resolved to a git dir");
        }
        return Ok(());
    }
    // Reflog backfill (m37 chunk 2): the branch switches of the last 30
    // days the poll never saw, once per start; repeats dedupe on
    // `reflog:<sha>@<ts>`.
    let now_ms = Timestamp::now().as_millisecond();
    for repo in &repos {
        for e in chronicle_capture::reflog::reflog_checkouts(repo, now_ms - 30 * 86_400_000, now_ms)
        {
            if tx.send(CaptureEvent::Activity(e)).is_err() {
                return Ok(());
            }
        }
    }
    spawn_provider_thread("git", "git provider", git, tx)
}

/// Notes reader (m32 chunk 5): every `git_repos` entry's
/// `.remember/today-*.md`; optional, never load-bearing — same contract
/// as git.
pub(crate) fn spawn_notes_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::notes::NotesProvider;

    let repos: Vec<PathBuf> = config
        .git_repos
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let notes = NotesProvider::new(&repos);
    if notes.is_empty() {
        return Ok(());
    }
    spawn_provider_thread("notes", "notes provider", notes, tx)
}

/// AI session watcher: optional, never load-bearing — same contract as git.
pub(crate) fn spawn_ai_sessions_capture(
    config: &Config,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::ai_sessions::AiSessionProvider;
    use chronicle_capture::sessions::{self, SessionFormat};

    let dirs: Vec<PathBuf> = config
        .ai_session_dirs
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(());
    };
    // Every format with a directory on this machine (m37 chunk 0), or the
    // configured names; Claude Code's own dirs come from `ai_session_dirs`.
    let formats: Vec<Box<dyn SessionFormat>> = if config.ai_session_formats.is_empty() {
        sessions::all()
            .into_iter()
            .filter(|f| f.name() != "claude" && !f.roots(&home).is_empty())
            .collect()
    } else {
        config
            .ai_session_formats
            .iter()
            .filter(|n| n.as_str() != "claude")
            .filter_map(|n| {
                let f = sessions::by_name(n);
                if f.is_none() {
                    tracing::warn!("ai_session_formats: unknown format {n:?}");
                }
                f
            })
            .collect()
    };
    // Repos and worktrees: what Gemini's hashed cwd and Aider's
    // repo-root history are matched against.
    let candidates: Vec<PathBuf> = chronicle_core::project::Matcher::from_config(config)
        .projects
        .iter()
        .flat_map(|p| p.paths.clone())
        .collect();
    let names: Vec<&str> = formats.iter().map(|f| f.name()).collect();
    let watcher = AiSessionProvider::new(dirs.clone(), formats, home, candidates);
    if watcher.is_empty() {
        if !dirs.is_empty() {
            tracing::warn!("ai_session_dirs configured but none is a directory");
        }
        return Ok(());
    }
    if !names.is_empty() {
        tracing::info!("session formats besides claude: {}", names.join(", "));
    }
    spawn_provider_thread("ai-sessions", "ai session provider", watcher, tx)
}

/// PR poller via the user's `gh`: opt-in, never load-bearing.
pub(crate) fn spawn_github_capture(
    config: &Config,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::github::GitHubProvider;

    if !config.github_prs {
        return Ok(());
    }
    let gh = chronicle_core::config::resolve_command("gh");
    if !gh.contains('/') {
        tracing::warn!("github_prs = true but `gh` is not on PATH");
        return Ok(());
    }
    let provider = GitHubProvider::new(PathBuf::from(gh));
    spawn_provider_thread("github", "github provider", provider, tx)
}

/// atuin history poller: opt-in, never load-bearing.
pub(crate) fn spawn_shell_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::shell::{ShellProvider, default_db_path};

    if !config.shell_history {
        return Ok(());
    }
    let db = default_db_path();
    if !db.is_file() {
        tracing::warn!("shell_history = true but {} is missing", db.display());
        return Ok(());
    }
    let repos: Vec<PathBuf> = config
        .git_repos
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let provider = ShellProvider::new(db, &repos);
    spawn_provider_thread("shell", "shell provider", provider, tx)
}

/// Google Calendar poller: opt-in and only once `chronicle gcal-login` has
/// written the token file; never load-bearing.
pub(crate) fn spawn_gcal_capture(
    config: &Config,
    data_dir: &Path,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::gcal::{GcalProvider, Tokens, token_path};

    if !config.google_calendar {
        return Ok(());
    }
    let path = token_path(data_dir);
    if !path.exists() {
        tracing::warn!(
            "google_calendar = true but no {}: run `chronicle gcal-login`",
            path.display()
        );
        return Ok(());
    }
    let tokens = match Tokens::load(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("google_calendar: {} unreadable: {e}", path.display());
            return Ok(());
        }
    };
    let provider = GcalProvider::new(tokens);
    std::thread::Builder::new()
        .name("gcal".into())
        .spawn(move || {
            if let Err(e) = provider.run(tx) {
                tracing::error!("google calendar provider exited: {e}");
            }
        })?;
    Ok(())
}

/// `chronicle gcal-login`: OAuth desktop flow. Opens the consent screen in
/// the browser, takes the code off an ephemeral loopback port, exchanges it
/// and writes `<data dir>/google.toml` (mode 0600).
pub(crate) fn gcal_login(
    data_dir: &Path,
    client_id: Option<String>,
    client_secret: Option<String>,
) -> anyhow::Result<()> {
    use chronicle_capture::gcal;

    let client_id = flag_or_env(client_id, "CHRONICLE_GOOGLE_CLIENT_ID")
        .context("no OAuth client id: pass --client-id or set CHRONICLE_GOOGLE_CLIENT_ID")?;
    let client_secret = flag_or_env(client_secret, "CHRONICLE_GOOGLE_CLIENT_SECRET").context(
        "no OAuth client secret: pass --client-secret or set CHRONICLE_GOOGLE_CLIENT_SECRET",
    )?;

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let redirect_uri = format!("http://{}", listener.local_addr()?);
    let state = random_hex(16)?;
    let endpoints = gcal::Endpoints::default();
    let url = gcal::auth_url(&endpoints, &client_id, &redirect_uri, &state);
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    println!("waiting for Google on {redirect_uri}; if no browser opened, visit:\n{url}");

    let code = wait_for_oauth_code(&listener, &state)?;
    let grant = gcal::exchange_code(&endpoints, &client_id, &client_secret, &code, &redirect_uri)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let refresh_token = grant.refresh_token.context(
        "Google returned no refresh token: remove Chronicle under \
         myaccount.google.com/permissions and sign in again",
    )?;
    let email = gcal::account_email(&endpoints, &grant.access_token).unwrap_or_default();
    std::fs::create_dir_all(data_dir)?;
    let path = gcal::token_path(data_dir);
    gcal::Tokens {
        client_id,
        client_secret,
        refresh_token,
        email: email.clone(),
    }
    .save(&path)
    .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    let who = if email.is_empty() {
        "the primary calendar".to_owned()
    } else {
        email
    };
    println!(
        "signed in as {who} \u{b7} token in {} \u{b7} turn Google Calendar on in \
         Settings \u{203a} Connections and restart the daemon",
        path.display()
    );
    Ok(())
}

pub(crate) fn flag_or_env(flag: Option<String>, var: &str) -> Option<String> {
    flag.or_else(|| std::env::var(var).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// `n` bytes from the OS CSPRNG, hex-encoded.
pub(crate) fn random_hex(n: usize) -> anyhow::Result<String> {
    use std::io::Read;

    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The browser's redirect carries the code; anything else on the port (a
/// favicon probe, a stray hit) is answered and ignored.
pub(crate) fn wait_for_oauth_code(
    listener: &std::net::TcpListener,
    state: &str,
) -> anyhow::Result<String> {
    use chronicle_capture::gcal::redirect_param;
    use std::io::{BufRead, BufReader, Read, Write};

    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut line = String::new();
        // Bounded: the request line is all we read, and the peer is not
        // necessarily the browser we opened.
        BufReader::new(&stream)
            .take(8 * 1024)
            .read_line(&mut line)?;
        let code = redirect_param(&line, "code");
        let error = redirect_param(&line, "error");
        let body = match (&code, &error) {
            (Some(_), _) => "Chronicle is signed in. You can close this tab.",
            (_, Some(_)) => "Google refused the sign-in; check the terminal.",
            _ => "Waiting for Google.",
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let _ = stream.flush();
        if let Some(err) = error {
            bail!("Google returned {err}");
        }
        if let Some(code) = code {
            if redirect_param(&line, "state").as_deref() != Some(state) {
                bail!("OAuth state mismatch \u{2014} ignoring the redirect");
            }
            return Ok(code);
        }
    }
    bail!("the loopback listener closed before the code arrived")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn spawn_capture(
    _config: &Config,
    _data_dir: &Path,
    _tx: Sender<CaptureEvent>,
    _ctrl: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    bail!("capture on this platform lands in M40")
}

pub(crate) const AFK_POLL: Duration = Duration::from_secs(30);
pub(crate) const GAP_MARKER_SECS: i64 = 60;

pub(crate) fn afk_loop(
    afk: impl chronicle_capture::AfkProvider,
    tx: Sender<CaptureEvent>,
    threshold_ms: u64,
) {
    // Announce the starting state so a dangling AFK span (gap marker, or a
    // restart while idle) gets closed.
    let mut was_idle = match afk.idle_ms() {
        Ok(ms) => {
            let idle = ms >= threshold_ms;
            if tx.send(afk_event(idle, ms)).is_err() {
                return;
            }
            idle
        }
        Err(e) => {
            tracing::warn!("afk poll failed: {e}");
            false
        }
    };
    let mut last_poll = Timestamp::now();
    loop {
        std::thread::sleep(AFK_POLL);
        let now = Timestamp::now();
        // sleep() counts monotonic time, which stands still through a
        // suspend, and the first input after resume resets X's idle counter
        // before this poll sees it: a wall-clock jump is the only trace, so
        // the AFK starts at the last poll (m32 chunk 0).
        if slept_through(last_poll, now, threshold_ms) && !was_idle {
            was_idle = true;
            if tx
                .send(CaptureEvent::Afk {
                    idle: true,
                    ts: last_poll,
                })
                .is_err()
            {
                return;
            }
        }
        last_poll = now;
        let ms = match afk.idle_ms() {
            Ok(ms) => ms,
            Err(e) => {
                tracing::warn!("afk poll failed: {e}");
                continue;
            }
        };
        let idle = ms >= threshold_ms;
        if idle == was_idle {
            continue;
        }
        was_idle = idle;
        if tx.send(afk_event(idle, ms)).is_err() {
            return;
        }
    }
}

/// More wall-clock time passed between two polls than the poll interval plus
/// the idle threshold: the machine slept, and any idle stretch it would have
/// reported is gone.
pub(crate) fn slept_through(last_poll: Timestamp, now: Timestamp, threshold_ms: u64) -> bool {
    now.as_millisecond() - last_poll.as_millisecond()
        > AFK_POLL.as_millis() as i64 + threshold_ms as i64
}

/// Idle transitions are backdated to when input actually stopped.
pub(crate) fn afk_event(idle: bool, idle_ms: u64) -> CaptureEvent {
    let now = Timestamp::now();
    let ts = if idle {
        now.checked_sub((idle_ms as i64).milliseconds())
            .unwrap_or(now)
    } else {
        now
    };
    CaptureEvent::Afk { idle, ts }
}

pub(crate) struct Filters {
    apps: Vec<Regex>,
    titles: Vec<Regex>,
}

impl Filters {
    pub(crate) fn new(config: &Config) -> anyhow::Result<Self> {
        let compile = |patterns: &[String]| -> anyhow::Result<Vec<Regex>> {
            patterns
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("bad exclusion regex {p:?}")))
                .collect()
        };
        Ok(Self {
            apps: compile(&config.excluded_apps)?,
            titles: compile(&config.excluded_titles)?,
        })
    }

    /// Excluded events are dropped before storage — never written at all.
    pub(crate) fn excluded(&self, event: &CaptureEvent) -> bool {
        let (app, title, url) = match event {
            CaptureEvent::Focus(e) | CaptureEvent::TitleChanged(e) => (&e.app, &e.title, None),
            CaptureEvent::Url(e) => (&e.app, &e.title, Some(&e.url)),
            CaptureEvent::Activity(_)
            | CaptureEvent::Afk { .. }
            | CaptureEvent::Lock { .. }
            | CaptureEvent::Presence(_) => return false,
        };
        self.apps.iter().any(|r| r.is_match(app))
            || self
                .titles
                .iter()
                .any(|r| r.is_match(title) || url.is_some_and(|u| r.is_match(u)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // m32 chunk 0: a poll that lands more than the interval plus the idle
    // threshold late on the wall clock is a suspend; a slow poll is not.
    #[test]
    fn slept_through_needs_a_wall_clock_jump() {
        let t0 = Timestamp::UNIX_EPOCH;
        let late = |secs: i64| t0 + secs.seconds();
        assert!(!slept_through(t0, late(30), 120_000));
        assert!(!slept_through(t0, late(150), 120_000));
        assert!(slept_through(t0, late(151), 120_000));
        assert!(slept_through(t0, late(15 * 3600), 120_000));
    }
}
