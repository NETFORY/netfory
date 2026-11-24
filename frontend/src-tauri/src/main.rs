// Thin launcher — all logic lives in the `smartnet_lib` library crate.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    smartnet_lib::run();
}
