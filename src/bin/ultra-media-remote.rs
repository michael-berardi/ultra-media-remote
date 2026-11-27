//! Command-line frontend for `ultra-media-remote`.
//!
//! Every command writes a single JSON value to stdout so output can be piped
//! into `jq` or consumed by another process. Diagnostics go to stderr.
//! Arguments are parsed by hand to keep the binary dependency-free beyond the
//! library's existing `serde_json`. The update notice and `update` command
//! (docs/updates.md) live in the CLI only; the library makes no network calls.

#[path = "ultra-media-remote/update.rs"]
mod update;

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::json;
use ultra_media_remote::{
    media_snapshot, now_playing_fetch, output_volume, seek, set_output_volume,
    transport_capabilities, transport_send, TransportCommand,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(1);
const WATCH_INTERVAL: Duration = Duration::from_millis(500);
const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
ultra-media-remote — read macOS Now Playing state and control playback

Usage:
  ultra-media-remote <command> [arguments]

Commands:
  now                    Print the current Now Playing item as JSON (null when idle)
  snapshot               Print Now Playing, transport capabilities and volume as JSON
  caps                   Print transport capabilities as JSON
  play-pause             Toggle playback; prints {\"command\":..,\"sent\":bool}
  next                   Skip to the next item
  previous               Return to the previous item
  seek <seconds>         Seek to an absolute position in seconds
  volume                 Print the default output volume as {\"volume\":number|null}
  volume <0..1>          Set the default output volume
  watch                  Print one JSON line whenever the Now Playing item changes
  update                 Install the newest release (see docs/updates.md); also
                         --check, --skip, --rollback, --auto on|off|status,
                         --notify on|off|status

Global options:
  --version, -V          Print the CLI version
  --help, -h, help       Print this help

Exit status: 0 on success, 1 when a media operation was not delivered, 2 on
invalid arguments; `update --check` exits 10 when an update is available.
The adapter is discovered next to this binary (mediaremote/
or lib/mediaremote/) or via ULTRA_MEDIA_REMOTE_ADAPTER_DIR.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("update") => {
            return match update::parse_action(&args[1..]) {
                Ok(action) => update::command(action),
                Err(message) => usage_error(&message),
            }
        }
        Some(update::HELPER) => {
            update::background(args.get(1).map(String::as_str) == Some("--check"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    // --version and --help are what installers and scripts probe with: they
    // never prune a kept release or print an update notice.
    let probe = matches!(
        args.first().map(String::as_str),
        Some("--version" | "-V" | "--help" | "-h" | "help")
    );
    let result = run(&args);
    if !probe {
        update::settle(matches!(&result, Ok(code) if *code == ExitCode::SUCCESS));
    }
    let code = result.unwrap_or_else(|message| usage_error(&message));
    if !probe {
        update::after_command();
    }
    code
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("ultra-media-remote: {message}");
    eprintln!("Run 'ultra-media-remote --help' for usage.");
    ExitCode::from(2)
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    match args.first().map(String::as_str) {
        None => Err("missing command".to_owned()),
        Some("--version" | "-V") if args.len() == 1 => {
            println!("ultra-media-remote {VERSION}");
            Ok(ExitCode::SUCCESS)
        }
        Some("--help" | "-h" | "help") if args.len() == 1 => {
            print!("{HELP}");
            Ok(ExitCode::SUCCESS)
        }
        Some("now") => {
            no_extra_args(args, "now")?;
            print_json(&now_playing_fetch(FETCH_TIMEOUT));
            Ok(ExitCode::SUCCESS)
        }
        Some("snapshot") => {
            no_extra_args(args, "snapshot")?;
            print_json(&media_snapshot(FETCH_TIMEOUT));
            Ok(ExitCode::SUCCESS)
        }
        Some("caps") => {
            no_extra_args(args, "caps")?;
            print_json(&transport_capabilities());
            Ok(ExitCode::SUCCESS)
        }
        Some(command @ ("play-pause" | "next" | "previous")) => {
            no_extra_args(args, command)?;
            let transport = match command {
                "play-pause" => TransportCommand::PlayPause,
                "next" => TransportCommand::Next,
                _ => TransportCommand::Previous,
            };
            delivery(command, transport_send(transport))
        }
        Some("seek") => {
            let seconds = single_number(args, "seek")?;
            let sent = seek(seconds);
            delivery("seek", sent)
        }
        Some("volume") => volume(args),
        Some("watch") => {
            no_extra_args(args, "watch")?;
            watch();
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(format!("unknown command '{other}'")),
    }
}

fn no_extra_args(args: &[String], command: &str) -> Result<(), String> {
    match args.len() {
        1 => Ok(()),
        _ => Err(format!("'{command}' takes no arguments")),
    }
}

fn single_number(args: &[String], command: &str) -> Result<f64, String> {
    if args.len() != 2 {
        return Err(format!("'{command}' takes exactly one number"));
    }
    parse_number(&args[1], command)
}

fn parse_number(value: &str, command: &str) -> Result<f64, String> {
    let number = value
        .parse::<f64>()
        .map_err(|_| format!("'{command}' needs a number, got '{value}'"))?;
    if !number.is_finite() {
        return Err(format!("'{command}' needs a finite number, got '{value}'"));
    }
    Ok(number)
}

fn volume(args: &[String]) -> Result<ExitCode, String> {
    match args.len() {
        1 => {
            print_json(&json!({ "volume": output_volume() }));
            Ok(ExitCode::SUCCESS)
        }
        2 => {
            let value = parse_number(&args[1], "volume")?;
            if !(0.0..=1.0).contains(&value) {
                return Err(format!("'volume' must be between 0 and 1, got {}", args[1]));
            }
            let sent = set_output_volume(value);
            print_json(&json!({ "volume": value, "sent": sent }));
            Ok(ExitCode::from(!sent as u8))
        }
        _ => Err("'volume' takes no more than one number in [0, 1]".to_owned()),
    }
}

fn delivery(command: &str, sent: bool) -> Result<ExitCode, String> {
    print_json(&json!({ "command": command, "sent": sent }));
    Ok(ExitCode::from(!sent as u8))
}

fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(line) => println!("{line}"),
        Err(error) => eprintln!("ultra-media-remote: cannot encode output: {error}"),
    }
}

fn watch() {
    let mut stdout = io::stdout().lock();
    let mut previous: Option<String> = None;
    loop {
        let value = now_playing_fetch(FETCH_TIMEOUT);
        let line = match serde_json::to_string(&value) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("ultra-media-remote: cannot encode output: {error}");
                return;
            }
        };
        if previous.as_deref() != Some(line.as_str()) {
            if writeln!(stdout, "{line}").is_err() || stdout.flush().is_err() {
                return; // downstream closed the pipe
            }
            previous = Some(line);
        }
        std::thread::sleep(WATCH_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_number;

    #[test]
    fn parse_number_rejects_missing_and_non_finite_values() {
        assert_eq!(parse_number("12.5", "seek"), Ok(12.5));
        assert!(parse_number("abc", "seek").is_err());
        assert!(parse_number("NaN", "seek").is_err());
        assert!(parse_number("inf", "seek").is_err());
    }
}
