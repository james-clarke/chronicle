//! `chronicle service {install,remove,status}` (m38 chunk 4): a systemd user
//! unit on Linux, a `launchd` LaunchAgent on macOS. Both the CLI and the
//! onboarding "run at login" card call this module.

use std::path::Path;

#[cfg(target_os = "linux")]
mod linux_impl {
    use std::path::PathBuf;

    fn user_unit_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/systemd/user/chronicle.service"))
    }

    pub(crate) fn available() -> bool {
        std::process::Command::new("systemctl")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    pub(crate) fn installed() -> bool {
        user_unit_path().is_some_and(|p| p.exists())
    }

    /// Write the user unit (`ExecStart` pointed at this binary) and enable
    /// it. Deliberately `enable` without `--now`: the daemon is already
    /// running as this UI's parent and holds the single-instance socket;
    /// `--now` would start a second instance that just toggles and exits,
    /// leaving a confusing stopped unit.
    pub(crate) fn install() -> Result<String, String> {
        let exe = crate::own_exe().map_err(|e| e.to_string())?;
        let unit_path = user_unit_path().ok_or("HOME not set")?;
        let template = include_str!("../../../packaging/chronicle.service");
        let unit = template.replace(
            "ExecStart=%h/.cargo/bin/chronicle run",
            &format!("ExecStart={} run", exe.display()),
        );
        if let Some(dir) = unit_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&unit_path, unit).map_err(|e| e.to_string())?;
        for args in [["daemon-reload", ""], ["enable", "chronicle"]] {
            let args: Vec<&str> = args.iter().filter(|a| !a.is_empty()).copied().collect();
            let out = std::process::Command::new("systemctl")
                .arg("--user")
                .args(&args)
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!(
                    "systemctl --user {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        Ok("installed \u{2014} Chronicle will start at your next login".into())
    }

    pub(crate) fn remove() -> Result<String, String> {
        let unit_path = user_unit_path().ok_or("HOME not set")?;
        if unit_path.exists() {
            let out = std::process::Command::new("systemctl")
                .args(["--user", "disable", "chronicle"])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.contains("does not exist") {
                    return Err(format!(
                        "systemctl --user disable failed: {}",
                        stderr.trim()
                    ));
                }
            }
            std::fs::remove_file(&unit_path).map_err(|e| e.to_string())?;
        }
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        Ok("removed".into())
    }

    pub(crate) fn status() -> Result<String, String> {
        let run = |arg: &str| -> String {
            std::process::Command::new("systemctl")
                .args(["--user", arg, "chronicle"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|_| "unknown".into())
        };
        Ok(format!("{} / {}", run("is-enabled"), run("is-active")))
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    const LABEL: &str = "dev.chronicled.chronicle";

    fn home() -> Result<PathBuf, String> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME not set".to_string())
    }

    fn plist_path() -> Result<PathBuf, String> {
        Ok(home()?
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    /// The GUI domain launchd addresses services in is `gui/<uid>`; read it
    /// off the home dir's owner rather than linking libc for `getuid`.
    fn uid() -> Result<u32, String> {
        let home = home()?;
        std::fs::metadata(&home)
            .map(|m| m.uid())
            .map_err(|e| e.to_string())
    }

    pub(crate) fn available() -> bool {
        true
    }

    pub(crate) fn installed() -> bool {
        plist_path().is_ok_and(|p| p.exists())
    }

    /// Write the plist and `launchctl bootstrap` it into the user's GUI
    /// domain. The single-instance note from the systemd path applies here
    /// too: `bootstrap` starts a second `chronicle run` immediately, which
    /// sees the running instance's socket, toggles the UI and exits 0;
    /// `KeepAlive.SuccessfulExit = false` tells launchd not to treat that
    /// clean exit as a crash to relaunch, so it sits idle until next login.
    pub(crate) fn install() -> Result<String, String> {
        let exe = crate::own_exe().map_err(|e| e.to_string())?;
        let data_dir = chronicle_core::data_dir().ok_or("no data dir")?;
        let log = data_dir.join("logs").join("launchd.log");
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let home = home()?;
        let plist = super::render_plist(&exe, &log, &home);
        let path = plist_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, plist).map_err(|e| e.to_string())?;
        let uid = uid()?;
        // A previous `remove` left the label disabled (persistent flag,
        // survives across boots); re-enable before bootstrapping so a
        // reinstall isn't silently refused.
        let _ = std::process::Command::new("launchctl")
            .args(["enable", &format!("gui/{uid}/{LABEL}")])
            .output();
        let out = std::process::Command::new("launchctl")
            .args([
                "bootstrap",
                &format!("gui/{uid}"),
                &path.display().to_string(),
            ])
            .output()
            .map_err(|e| e.to_string())?;
        // launchd's exit code 5 covers every generic bootstrap failure (bad
        // plist, permissions, no GUI session) as well as "already
        // bootstrapped" — it isn't diagnostic on its own. Ask launchd
        // directly instead of pattern-matching the exit code.
        let print = std::process::Command::new("launchctl")
            .args(["print", &format!("gui/{uid}/{LABEL}")])
            .output()
            .map_err(|e| e.to_string())?;
        if !print.status.success() {
            return Err(format!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok("installed \u{2014} Chronicle will start at your next login".into())
    }

    /// `bootout` SIGTERMs the whole job tree — the UI child and the daemon
    /// it runs under, not just the LaunchAgent registration — so a "run at
    /// login" toggle would kill the running app out from under the user.
    /// `disable` only flips launchd's persistent load flag (mirrors the
    /// Linux `systemctl --user disable` path, which is also without
    /// `--now`); the running process is left alone and the plist removal
    /// below is what actually stops it starting again next login.
    pub(crate) fn remove() -> Result<String, String> {
        let uid = uid()?;
        let _ = std::process::Command::new("launchctl")
            .args(["disable", &format!("gui/{uid}/{LABEL}")])
            .output();
        if let Ok(path) = plist_path() {
            let _ = std::fs::remove_file(path);
        }
        Ok("removed".into())
    }

    pub(crate) fn status() -> Result<String, String> {
        let uid = uid()?;
        let out = std::process::Command::new("launchctl")
            .args(["print", &format!("gui/{uid}/{LABEL}")])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(if out.status.success() {
            "loaded".into()
        } else {
            "not loaded".into()
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod other_impl {
    pub(crate) fn available() -> bool {
        false
    }

    pub(crate) fn installed() -> bool {
        false
    }

    pub(crate) fn install() -> Result<String, String> {
        Err("no service manager on this platform".into())
    }

    pub(crate) fn remove() -> Result<String, String> {
        Err("no service manager on this platform".into())
    }

    pub(crate) fn status() -> Result<String, String> {
        Err("no service manager on this platform".into())
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux_impl::{available, install, installed, remove, status};
#[cfg(target_os = "macos")]
pub(crate) use macos_impl::{available, install, installed, remove, status};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) use other_impl::{available, install, installed, remove, status};

/// Render the LaunchAgent plist for `exe`, redirecting its stdout/stderr to
/// `log`, with `home` used only to put `~/.cargo/bin` on the `PATH`. Not
/// `cfg`-gated so its fixture test below runs on every platform, including
/// this Linux box; on non-macOS builds nothing else calls it.
#[allow(dead_code)]
pub(crate) fn render_plist(exe: &Path, log: &Path, home: &Path) -> String {
    include_str!("../../../packaging/dev.chronicled.chronicle.plist")
        .replace("@EXE@", &xml_escape(&exe.display().to_string()))
        .replace("@LOG@", &xml_escape(&log.display().to_string()))
        .replace("@HOME@", &xml_escape(&home.display().to_string()))
}

/// Escape the three XML metacharacters a path could plausibly contain
/// before splicing it into the plist template.
#[allow(dead_code)]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_has_exe_label_and_keepalive() {
        let rendered = render_plist(
            Path::new("/usr/local/bin/chronicle"),
            Path::new("/Users/j/Library/Application Support/chronicle/logs/launchd.log"),
            Path::new("/Users/j"),
        );
        assert!(rendered.contains("/usr/local/bin/chronicle"));
        assert!(rendered.contains("dev.chronicled.chronicle"));
        assert!(rendered.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn plist_escapes_ampersand_in_exe_path() {
        let rendered = render_plist(
            Path::new("/Users/j/dev & build/chronicle"),
            Path::new("/Users/j/logs/launchd.log"),
            Path::new("/Users/j"),
        );
        assert!(rendered.contains("/Users/j/dev &amp; build/chronicle"));
        assert!(!rendered.contains("dev & build"));
    }
}
