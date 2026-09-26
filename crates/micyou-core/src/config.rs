/*
 * MicYou — Turns your Android device into a high-quality PC microphone.
 * Copyright (C) 2026 LanRhyme <https://github.com/LanRhyme/MicYou>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 */

use micyou_audio::dsp::AudioDspSettings;
use std::fs;
use std::path::PathBuf;

/// Preference DTOs are part of the frontend contract (see `micyou-api`).
pub use micyou_api::config::{ServerPrefs, ThemeColors, UiPrefs};

/// Process-wide config directory override (daemon `--config-dir`, tests).
static CONFIG_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Override the shared config directory for this process. Must be called
/// before any config access; later calls are ignored (first writer wins).
pub fn set_config_dir(dir: impl Into<PathBuf>) {
    let _ = CONFIG_DIR_OVERRIDE.set(dir.into());
}

/// Shared config directory.
///
/// Resolution order: [`set_config_dir`] override, `$MICYOU_CONFIG_DIR`,
/// then the platform default (Windows: `%APPDATA%\micyou`,
/// unix: `$XDG_CONFIG_HOME/micyou` or `~/.config/micyou`).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = CONFIG_DIR_OVERRIDE.get() {
        return dir.clone();
    }
    if let Some(dir) = std::env::var_os("MICYOU_CONFIG_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("micyou");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let dir = PathBuf::from(xdg).join("micyou");
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("micyou")
}

/// settings.json: the DSP settings shared by GUI, CLI and TUI.
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// ui.json: GUI UI preferences (language, theme color) that the TUI reads.
pub fn ui_prefs_path() -> PathBuf {
    config_dir().join("ui.json")
}

/// theme.json: current GUI theme colors exported for the TUI.
pub fn theme_path() -> PathBuf {
    config_dir().join("theme.json")
}

/// server.json: connection-level settings shared by GUI, CLI and TUI
/// (port, mode, bind address, output device).
pub fn server_prefs_path() -> PathBuf {
    config_dir().join("server.json")
}

/// Load DSP settings from settings.json, falling back to defaults.
pub fn load_dsp_settings() -> AudioDspSettings {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| serde_json::from_str::<AudioDspSettings>(&text).ok())
        .map(|mut settings| {
            settings.normalize();
            settings
        })
        .unwrap_or_default()
}

/// Persist DSP settings to settings.json (GUI, CLI and TUI share this file).
pub fn save_dsp_settings(settings: &AudioDspSettings) -> Result<(), String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create config dir failed: {e}"))?;
    let mut normalized = settings.clone();
    normalized.normalize();
    let json = serde_json::to_string_pretty(&normalized)
        .map_err(|e| format!("serialize settings failed: {e}"))?;
    fs::write(settings_path(), json).map_err(|e| format!("write settings.json failed: {e}"))
}

/// Raw settings.json as a JSON value (for the CLI `settings get`).
pub fn settings_json() -> serde_json::Value {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::to_value(AudioDspSettings::default()).unwrap_or_default())
}

pub fn load_ui_prefs() -> UiPrefs {
    fs::read_to_string(ui_prefs_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_ui_prefs(prefs: &UiPrefs) -> Result<(), String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create config dir failed: {e}"))?;
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| format!("serialize ui prefs failed: {e}"))?;
    fs::write(ui_prefs_path(), json).map_err(|e| format!("write ui.json failed: {e}"))
}

pub fn load_theme_colors() -> ThemeColors {
    fs::read_to_string(theme_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_theme_colors(colors: &ThemeColors) -> Result<(), String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create config dir failed: {e}"))?;
    let json =
        serde_json::to_string_pretty(colors).map_err(|e| format!("serialize theme failed: {e}"))?;
    fs::write(theme_path(), json).map_err(|e| format!("write theme.json failed: {e}"))
}

/// Load connection settings from server.json, falling back to defaults.
pub fn load_server_prefs() -> ServerPrefs {
    fs::read_to_string(server_prefs_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist connection settings to server.json (GUI, CLI and TUI share this file).
pub fn save_server_prefs(prefs: &ServerPrefs) -> Result<(), String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create config dir failed: {e}"))?;
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| format!("serialize server prefs failed: {e}"))?;
    fs::write(server_prefs_path(), json).map_err(|e| format!("write server.json failed: {e}"))
}
