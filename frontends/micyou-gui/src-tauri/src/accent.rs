/* libmicyou — micyou-gui frontend (original MicYou Vue UI). */

//! Desktop accent color detection (theme seed follow-system). Shell-local:
//! reads the OS setting directly, no backend involvement.

use std::process::Command;

use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SystemAccentColor {
    pub hex: String,
    pub source: String,
    pub supported: bool,
}

fn accent_color(hex: impl Into<String>, source: impl Into<String>) -> SystemAccentColor {
    SystemAccentColor {
        hex: hex.into(),
        source: source.into(),
        supported: true,
    }
}

fn fallback_accent_color() -> SystemAccentColor {
    SystemAccentColor {
        hex: "#5b7cfa".to_string(),
        source: "fallback".to_string(),
        supported: false,
    }
}

#[cfg(windows)]
fn windows_accent_color() -> Option<SystemAccentColor> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let dwm = current_user
        .open_subkey("Software\\Microsoft\\Windows\\DWM")
        .ok()?;
    let value: u32 = dwm.get_value("AccentColor").ok()?;
    Some(accent_color(argb_to_hex(value), "windows-dwm"))
}

#[cfg(windows)]
fn argb_to_hex(value: u32) -> String {
    // Windows stores AccentColor as AABBGGRR.
    format!(
        "#{:02x}{:02x}{:02x}",
        value & 0xff,
        (value >> 8) & 0xff,
        (value >> 16) & 0xff
    )
}

#[cfg(target_os = "macos")]
fn macos_accent_color() -> Option<SystemAccentColor> {
    let output = Command::new("defaults")
        .args(["read", "-g", "AppleAccentColor"])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i32>()
        .ok()?;
    let hex = match value {
        0 => "#ff3b30",
        1 => "#ff9500",
        2 => "#ffcc00",
        3 => "#34c759",
        4 => "#007aff",
        5 => "#af52de",
        6 => "#ff2d55",
        _ => return None,
    };
    Some(accent_color(hex, "macos-accent"))
}

#[cfg(target_os = "linux")]
fn linux_accent_color() -> Option<SystemAccentColor> {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "accent-color"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_matches('\'')
        .to_ascii_lowercase();
    let hex = match value.as_str() {
        "red" => "#f44336",
        "orange" => "#ff9800",
        "yellow" => "#fbc02d",
        "green" => "#4caf50",
        "blue" => "#2196f3",
        "purple" => "#9c27b0",
        "pink" => "#e91e63",
        "slate" => "#607d8b",
        _ => return None,
    };
    Some(accent_color(hex, "gnome-accent"))
}

/// Read the desktop accent color once at application startup.
#[tauri::command]
pub fn get_system_accent_color() -> SystemAccentColor {
    #[cfg(windows)]
    if let Some(color) = windows_accent_color() {
        return color;
    }

    #[cfg(target_os = "macos")]
    if let Some(color) = macos_accent_color() {
        return color;
    }

    #[cfg(target_os = "linux")]
    if let Some(color) = linux_accent_color() {
        return color;
    }

    fallback_accent_color()
}

#[cfg(test)]
mod tests {
    use super::fallback_accent_color;

    #[test]
    fn fallback_is_stable_and_marked_unsupported() {
        let color = fallback_accent_color();
        assert!(!color.supported);
        assert_eq!(color.hex, "#5b7cfa");
        assert_eq!(color.source, "fallback");
    }
}
