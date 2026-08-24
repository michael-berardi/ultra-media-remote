//! Samples the 11-band system-output spectrum for a few seconds.
//!
//! Requires the `spectrum` cargo feature and Screen Recording permission:
//!
//! ```console
//! cargo run --example spectrum --features spectrum
//! ```

use std::time::{Duration, Instant};

fn main() {
    let Some(bands) = ultra_media_remote::spectrum_start() else {
        eprintln!(
            "Could not start the system-audio spectrum. Requires macOS 15+ and Screen \
             Recording permission for this binary (never prompted automatically)."
        );
        std::process::exit(1);
    };

    println!("Sampling 11-band spectrum (63 Hz .. 14 kHz) for 5 seconds:\n");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if let Some(levels) = bands.fetch() {
            let bars: String = levels
                .iter()
                .map(|level| {
                    let filled = (level * 20.0).round() as usize;
                    format!("[{: <20}]", "\u{2588}".repeat(filled))
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("{bars}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Dropping `bands` stops the capture.
}
