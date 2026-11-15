// Тонкая обёртка десктопного бинарника: вся логика — в lib.rs (run),
// т.к. Android/iOS сборкам Tauri нужен crate-type cdylib/staticlib.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    netdrop_ui_lib::run()
}
