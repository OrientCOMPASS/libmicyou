/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! Daemon logging: a dependency-free `log` backend writing timestamped lines
//! to `<config_dir>/logs/daemon.log` (with a single `.old` rotation at
//! startup) and optionally mirroring to stderr for foreground runs.
//!
//! The RPC service layer exposes the log file through `system/logPath`,
//! `system/logContent` and `system/exportLog` so frontends keep the same
//! diagnostics they had with the Tauri log plugin.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Level, LevelFilter, Metadata, Record};

/// Rotate the active log beyond this size at initialization.
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

struct DaemonLogger {
    file: Mutex<File>,
    stderr: bool,
    level: LevelFilter,
}

impl log::Log for DaemonLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "[{} {:>5} {}] {}\n",
            now,
            record.level(),
            record.target(),
            record.args()
        );
        if self.stderr {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Directory holding daemon logs.
pub fn log_dir() -> PathBuf {
    crate::config::config_dir().join("logs")
}

/// Active daemon log file path.
pub fn log_file_path() -> PathBuf {
    log_dir().join("daemon.log")
}

/// Initialize global logging. Returns the log file path.
///
/// `mirror_stderr` echoes every line to stderr (useful when the daemon runs
/// in a terminal or under a supervisor that captures stdio). Rotation: if the
/// existing log exceeds [`MAX_LOG_BYTES`] it is moved to `daemon.log.old`.
pub fn init(level: LevelFilter, mirror_stderr: bool) -> std::io::Result<PathBuf> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)?;
    let path = log_file_path();

    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let old = dir.join("daemon.log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
    }

    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let logger = Box::new(DaemonLogger {
        file: Mutex::new(file),
        stderr: mirror_stderr,
        level,
    });
    // Re-initialization (tests, embedded hosts) keeps the first logger; that
    // matches `log`'s global-slot semantics and is not an error for us.
    let _ = log::set_boxed_logger(logger).map(|()| log::set_max_level(level));
    log::set_max_level(level);
    Ok(path)
}

/// Read the tail of the daemon log (newest last), capped at `max_bytes`
/// from the end of the file.
pub fn read_log_tail(max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let path = log_file_path();
    if !path.exists() {
        return Ok(String::new());
    }
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::End(-(max_bytes as i64)))?;
    }
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Copy the daemon log next to `dest_dir` with a timestamped name and return
/// the exported path (backs the frontend "export log" action).
pub fn export_log(dest_dir: Option<PathBuf>) -> std::io::Result<PathBuf> {
    let dir = dest_dir.unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "micyou-logs-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ))
    });
    std::fs::create_dir_all(&dir)?;
    let source = log_file_path();
    let dest = dir.join("micyou-daemon.log");
    if source.exists() {
        std::fs::copy(&source, &dest)?;
    } else {
        File::create(&dest)?;
    }
    Ok(dest)
}

/// Parse a `log` level name ("info", "debug", …) for CLI flags.
pub fn parse_level(value: &str) -> Option<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// Level filter currently in effect.
pub fn current_level() -> LevelFilter {
    log::max_level()
}

#[allow(dead_code)]
fn level_label(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_accepts_common_spellings() {
        assert_eq!(parse_level("INFO"), Some(LevelFilter::Info));
        assert_eq!(parse_level(" warn"), Some(LevelFilter::Warn));
        assert_eq!(parse_level("verbose"), None);
    }

    #[test]
    fn log_paths_live_under_config_dir() {
        assert_eq!(log_file_path(), log_dir().join("daemon.log"));
        assert!(log_dir().ends_with("logs"));
    }
}
