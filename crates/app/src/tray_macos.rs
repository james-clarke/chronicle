//! macOS menu-bar icon (m38 chunk 3): a `tray-icon` status item plus the
//! `NSApplication` run loop it needs, both hosted on the process's main
//! thread — `daemon::run` moves the daemon loop to its own thread and gives
//! this one the main thread instead.

use crossbeam_channel::Sender;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::daemon::CtrlMsg;

/// Runs the app on the main thread; never returns. A tray build failure is
/// non-fatal (logged) — the `NSApplication` loop still has to run either way
/// to keep the process (and the daemon thread) alive.
pub(crate) fn run_main(ctrl_tx: Sender<CtrlMsg>) -> ! {
    let mtm = MainThreadMarker::new().expect("run_main must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon or app switcher entry for an unbundled binary launched by
    // launchd — just the status item.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    match build_tray() {
        Ok((tray, show_hide_id, quit_id)) => {
            // Leaked on purpose: the icon must live for the process's life
            // and this thread never returns to drop it anyway.
            std::mem::forget(tray);
            spawn_event_thread(ctrl_tx, show_hide_id, quit_id);
        }
        Err(e) => tracing::warn!("tray unavailable: {e}"),
    }

    app.run();
    unreachable!("NSApplication::run never returns");
}

/// The status item and its "Show/Hide" / "Quit" menu (same `CtrlMsg`s as the
/// Linux tray), plus the two items' ids for the event thread to match on.
fn build_tray() -> tray_icon::Result<(tray_icon::TrayIcon, MenuId, MenuId)> {
    let show_hide = MenuItem::new("Show/Hide", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let (show_hide_id, quit_id) = (show_hide.id().clone(), quit.id().clone());
    let menu = Menu::new();
    menu.append_items(&[&show_hide, &quit])
        .map_err(|e| tray_icon::Error::OsError(std::io::Error::other(e)))?;

    let (width, height, rgba) = crate::daemon::tray_pixels();
    // Template icon: alpha-only (r=g=b=0), so AppKit tints it for light and
    // dark menu bars instead of showing it in the fixed accent color.
    let (pixels, _) = rgba.as_chunks::<4>();
    let template: Vec<u8> = pixels
        .iter()
        .flat_map(|&[_, _, _, a]| [0, 0, 0, a])
        .collect();
    let icon = Icon::from_rgba(template, width, height)
        .map_err(|e| tray_icon::Error::OsError(std::io::Error::other(e)))?;

    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_icon_as_template(true)
        .with_tooltip("Chronicle")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;
    Ok((tray, show_hide_id, quit_id))
}

/// Forwards menu clicks and left-clicks on the icon itself to the daemon's
/// control channel. tray-icon/muda hand these out as plain
/// `crossbeam_channel` receivers, so this needs no run loop of its own.
fn spawn_event_thread(ctrl_tx: Sender<CtrlMsg>, show_hide_id: MenuId, quit_id: MenuId) {
    std::thread::Builder::new()
        .name("tray-events".into())
        .spawn(move || {
            let menu_rx = MenuEvent::receiver();
            let tray_rx = TrayIconEvent::receiver();
            loop {
                crossbeam_channel::select! {
                    recv(menu_rx) -> event => {
                        let Ok(event) = event else { break };
                        if event.id == show_hide_id {
                            let _ = ctrl_tx.send(CtrlMsg::Toggle);
                        } else if event.id == quit_id {
                            let _ = ctrl_tx.send(CtrlMsg::Shutdown);
                        }
                    }
                    recv(tray_rx) -> event => {
                        if let Ok(TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }) = event
                        {
                            let _ = ctrl_tx.send(CtrlMsg::Toggle);
                        }
                    }
                }
            }
        })
        .expect("failed to spawn tray-events thread");
}
