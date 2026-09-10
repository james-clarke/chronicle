//! Print the focus stream the daemon would record, without a daemon or a
//! database (m39). `scripts/wayland-check.sh` drives it inside a nested
//! compositor; by hand:
//!
//! ```text
//! cargo run -p chronicle-capture --example focus-probe -- [auto|x11|wlr|kwin]
//! ```

fn main() {
    #[cfg(target_os = "linux")]
    {
        let configured = std::env::args().nth(1).unwrap_or_else(|| "auto".into());
        if let Err(e) = run(&configured) {
            eprintln!("focus-probe: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("focus-probe is Linux only");
}

#[cfg(target_os = "linux")]
fn run(configured: &str) -> Result<(), chronicle_capture::BoxError> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::wayland::{Choice, Route};
    use chronicle_core::types::CaptureEvent;

    let route = match chronicle_capture::wayland::choose(configured, |k| std::env::var(k).ok())? {
        Choice::Route(route) => route,
        Choice::ProbeWlr => Route::Wlr,
    };
    eprintln!("focus-probe: route {}", route.as_str());
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let result = match route {
            Route::X11 => chronicle_capture::x11::X11FocusProvider::new().map(|p| p.run(tx)),
            Route::Wlr => {
                chronicle_capture::wayland::wlr::WlrFocusProvider::new().map(|p| p.run(tx))
            }
            Route::Kwin => Err("the KWin focus route lands in m39 chunk 3".into()),
        };
        match result {
            Ok(Ok(())) => eprintln!("focus-probe: provider stopped"),
            Ok(Err(e)) | Err(e) => eprintln!("focus-probe: provider failed: {e}"),
        }
    });
    for event in rx {
        match event {
            CaptureEvent::Focus(f) => {
                println!("focus  app={:?} title={:?} pid={:?}", f.app, f.title, f.pid)
            }
            CaptureEvent::TitleChanged(f) => {
                println!("title  app={:?} title={:?} pid={:?}", f.app, f.title, f.pid)
            }
            CaptureEvent::Activity(a) => {
                println!("cwd    repo={:?} detail={:?}", a.repo, a.detail)
            }
            other => println!("other  {other:?}"),
        }
    }
    Ok(())
}
