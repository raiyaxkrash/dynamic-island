#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Отключает серую стартовую подложку контроллера WebView2 до первой отрисовки HTML.
    std::env::set_var("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", "0");
    dynamic_island_lib::run()
}
