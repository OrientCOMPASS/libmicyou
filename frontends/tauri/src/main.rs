/*
 * libmicyou — Tauri 2 reference frontend.
 *
 * Copyright (C) 2026 OrientCOMPASS
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

// Hide the console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    micyou_tauri_lib::run();
}
