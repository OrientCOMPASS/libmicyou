/* libmicyou — micyou-gui frontend (original MicYou Vue UI). */

// Hide the console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    micyou_gui_lib::run();
}
