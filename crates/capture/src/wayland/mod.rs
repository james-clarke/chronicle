//! Wayland capture (m39): two focus routes behind `FocusProvider` —
//! `wlr-foreign-toplevel-management` for wlroots compositors and a KWin
//! script over D-Bus for KDE Plasma — and one idle route for both
//! (`ext-idle-notify-v1`, `org_kde_kwin_idle` where that is missing). Lock
//! stays logind, which is display-server independent. Presence counts have
//! no Wayland protocol and stay off.

/// Globals arriving after `registry_queue_init` change nothing: what each
/// route needs is bound once, at start, or the route is not available.
macro_rules! ignore_registry {
    ($state:ty) => {
        impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for $state {
            fn event(
                _: &mut Self,
                _: &wayland_client::protocol::wl_registry::WlRegistry,
                _: <wayland_client::protocol::wl_registry::WlRegistry as wayland_client::Proxy>::Event,
                _: &wayland_client::globals::GlobalListContents,
                _: &wayland_client::Connection,
                _: &wayland_client::QueueHandle<Self>,
            ) {
            }
        }
    };
}

/// An object whose events carry nothing this crate reads.
macro_rules! ignore_events {
    ($state:ty, $iface:ty) => {
        impl wayland_client::Dispatch<$iface, ()> for $state {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as wayland_client::Proxy>::Event,
                _: &(),
                _: &wayland_client::Connection,
                _: &wayland_client::QueueHandle<Self>,
            ) {
            }
        }
    };
}

pub mod focus;
pub mod idle;
pub mod wlr;

use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::time::Duration;

use wayland_client::EventQueue;
use wayland_client::backend::WaylandError;

use crate::BoxError;

/// One dispatch pass over `queue`, waiting at most `wait` for the socket
/// (`None` blocks, `Some(ZERO)` only drains what has already arrived).
pub(crate) fn dispatch_until<S: 'static>(
    queue: &mut EventQueue<S>,
    state: &mut S,
    wait: Option<Duration>,
) -> Result<(), BoxError> {
    queue.dispatch_pending(state)?;
    let Some(guard) = queue.prepare_read() else {
        // Events were still buffered, so the caller has work already.
        return Ok(());
    };
    queue.flush()?;
    let fd = guard.connection_fd().as_raw_fd();
    let timeout = match wait {
        Some(d) => i32::try_from(d.as_millis()).unwrap_or(i32::MAX),
        None => -1,
    };
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd, a count that matches it, and a fd the
    // guard keeps open for the call.
    let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
    if ready < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(e.into());
    }
    if ready == 0 {
        return Ok(());
    }
    match guard.read() {
        Ok(_) => {}
        // Another queue drained the socket first.
        Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
        Err(e) => return Err(e.into()),
    }
    queue.dispatch_pending(state)?;
    Ok(())
}

/// Which focus provider the daemon runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    X11,
    /// `zwlr_foreign_toplevel_manager_v1`: Sway, Hyprland, Niri, river, labwc.
    Wlr,
    /// A KWin script reporting over the session bus: KDE Plasma 5 and 6.
    Kwin,
}

impl Route {
    /// The word `chronicle status` and the Settings capture card print.
    pub fn as_str(self) -> &'static str {
        match self {
            Route::X11 => "x11",
            Route::Wlr => "wlr",
            Route::Kwin => "kwin",
        }
    }
}

/// What the environment alone can decide. `ProbeWlr` needs a connection to
/// the compositor to confirm the toplevel protocol is there, which is the
/// one case that can still fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Route(Route),
    ProbeWlr,
}

/// Route from `focus_route` and the session environment. `auto`:
/// `WAYLAND_DISPLAY` decides Wayland against X11, then KDE against
/// everything else, since KWin does not expose the toplevel protocol to
/// ordinary clients and has to go through its scripting API.
pub fn choose(configured: &str, env: impl Fn(&str) -> Option<String>) -> Result<Choice, BoxError> {
    match configured {
        "x11" => return Ok(Choice::Route(Route::X11)),
        "wlr" => return Ok(Choice::Route(Route::Wlr)),
        "kwin" => return Ok(Choice::Route(Route::Kwin)),
        "auto" => {}
        other => {
            return Err(
                format!("focus_route = \"{other}\": expected auto, x11, wlr or kwin").into(),
            );
        }
    }
    let wayland = env("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if !wayland {
        return Ok(Choice::Route(Route::X11));
    }
    let desktop = env("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let kde = desktop
        .split(':')
        .any(|part| part.eq_ignore_ascii_case("kde"))
        || env("KDE_SESSION_VERSION").is_some();
    if kde {
        return Ok(Choice::Route(Route::Kwin));
    }
    Ok(Choice::ProbeWlr)
}

/// The error a compositor without `zwlr_foreign_toplevel_manager_v1` gets:
/// GNOME is the one that matters and needs a Shell extension, which is a
/// later milestone.
pub fn unsupported(desktop: &str) -> BoxError {
    let name = if desktop.is_empty() {
        "this compositor".to_string()
    } else {
        desktop.to_string()
    };
    if desktop.split(':').any(|p| p.eq_ignore_ascii_case("gnome")) {
        return format!(
            "{name} does not expose wlr-foreign-toplevel-management; GNOME Wayland needs the \
             Chronicle Shell extension, which is not shipped yet. Log in to an X11 session, or \
             set focus_route = \"x11\" to capture X clients through Xwayland."
        )
        .into();
    }
    format!(
        "{name} does not expose wlr-foreign-toplevel-management and is not KDE, so Chronicle has \
         no focus route for it. Set focus_route = \"x11\" to capture X clients through Xwayland."
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn no_wayland_display_is_x11() {
        let choice = choose("auto", env_of(&[("XDG_CURRENT_DESKTOP", "sway")])).unwrap();
        assert_eq!(choice, Choice::Route(Route::X11));
    }

    #[test]
    fn empty_wayland_display_is_x11() {
        let choice = choose("auto", env_of(&[("WAYLAND_DISPLAY", "")])).unwrap();
        assert_eq!(choice, Choice::Route(Route::X11));
    }

    #[test]
    fn kde_wayland_is_kwin() {
        let choice = choose(
            "auto",
            env_of(&[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ]),
        )
        .unwrap();
        assert_eq!(choice, Choice::Route(Route::Kwin));
    }

    #[test]
    fn kde_is_matched_inside_a_colon_list() {
        let choice = choose(
            "auto",
            env_of(&[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE:plasmawayland"),
            ]),
        )
        .unwrap();
        assert_eq!(choice, Choice::Route(Route::Kwin));
    }

    #[test]
    fn other_wayland_probes_for_the_toplevel_protocol() {
        for desktop in ["sway", "Hyprland", "niri", "GNOME", ""] {
            let choice = choose(
                "auto",
                env_of(&[
                    ("WAYLAND_DISPLAY", "wayland-1"),
                    ("XDG_CURRENT_DESKTOP", desktop),
                ]),
            )
            .unwrap();
            assert_eq!(choice, Choice::ProbeWlr, "desktop = {desktop}");
        }
    }

    #[test]
    fn an_explicit_route_ignores_the_environment() {
        let env = env_of(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert_eq!(choose("x11", &env).unwrap(), Choice::Route(Route::X11));
        assert_eq!(choose("kwin", &env).unwrap(), Choice::Route(Route::Kwin));
        assert_eq!(choose("wlr", &env).unwrap(), Choice::Route(Route::Wlr));
    }

    #[test]
    fn an_unknown_route_names_the_valid_ones() {
        let err = choose("wayland", env_of(&[])).unwrap_err().to_string();
        assert!(err.contains("auto, x11, wlr or kwin"), "{err}");
    }

    #[test]
    fn gnome_gets_the_extension_line() {
        let err = unsupported("GNOME").to_string();
        assert!(err.contains("Shell extension"), "{err}");
        assert!(err.contains("focus_route = \"x11\""), "{err}");
    }

    #[test]
    fn an_unknown_compositor_still_gets_the_xwayland_fallback() {
        let err = unsupported("weston").to_string();
        assert!(!err.contains("Shell extension"), "{err}");
        assert!(err.contains("focus_route = \"x11\""), "{err}");
    }
}
