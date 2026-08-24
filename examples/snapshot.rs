//! Prints one reusable macOS media snapshot as JSON.

use std::time::Duration;

fn main() {
    let snapshot = ultra_media_remote::media_snapshot(Duration::from_secs(2));
    println!(
        "{}",
        serde_json::to_string_pretty(&snapshot).expect("serialize media snapshot")
    );
}
