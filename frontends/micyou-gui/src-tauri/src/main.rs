/*
 * libmicyou — micyou-gui frontend (original MicYou Vue UI).
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou desktop application)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou port)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

// Hide the console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    micyou_gui_lib::run();
}
