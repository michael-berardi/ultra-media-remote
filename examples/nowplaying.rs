//! Prints the current macOS Now Playing snapshot as JSON.
//!
//! Requires a Mac with MediaRemote available (all supported macOS versions).
//! Start some playback in Music, Spotify, or a browser first.

use std::time::Duration;

fn main() {
    println!(
        "MediaRemote metadata access: {}",
        if ultra_media_remote::now_playing_available() {
            "available"
        } else {
            "unavailable"
        }
    );
    println!(
        "MediaRemote transport:       {}",
        if ultra_media_remote::transport_available() {
            "available"
        } else {
            "unavailable"
        }
    );

    match ultra_media_remote::now_playing_fetch(Duration::from_secs(2)) {
        Some(now_playing) => {
            println!("\n{}", serde_json::to_string_pretty(&now_playing).expect("serialize"));
        }
        None => {
            println!("\nNo now-playing information: nothing is playing, the player did");
            println!("not answer in time, or MediaRemote is unavailable on this system.");
        }
    }

    let capabilities = ultra_media_remote::transport_capabilities();
    println!(
        "\nTransport capabilities: play_pause={} previous={} next={}",
        capabilities.play_pause, capabilities.previous, capabilities.next
    );
}
