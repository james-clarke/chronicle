//! Print the Wayland idle clock once a second (m39), the signal `afk_loop`
//! polls every 30 s:
//!
//! ```text
//! cargo run -p chronicle-capture --example idle-probe -- [seconds]
//! ```

fn main() {
    #[cfg(target_os = "linux")]
    {
        let seconds: u64 = std::env::args()
            .nth(1)
            .and_then(|a| a.parse().ok())
            .unwrap_or(10);
        if let Err(e) = run(seconds) {
            eprintln!("idle-probe: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("idle-probe is Linux only");
}

#[cfg(target_os = "linux")]
fn run(seconds: u64) -> Result<(), chronicle_capture::BoxError> {
    use chronicle_capture::AfkProvider;
    use chronicle_capture::wayland::idle::WaylandAfkProvider;

    let afk = WaylandAfkProvider::new()?;
    for _ in 0..seconds {
        println!("idle_ms={}", afk.idle_ms()?);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(())
}
