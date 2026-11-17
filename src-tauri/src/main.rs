#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Отключает серую стартовую подложку контроллера WebView2 до первой отрисовки HTML.
    std::env::set_var("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", "0");
    // Запрещает WebView2 выгружать композитор прозрачного оверлея или замораживать кадры в фоне.
    if std::env::var_os("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_none() {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--disable-features=CalculateNativeWinOcclusion --disable-renderer-backgrounding --disable-backgrounding-occluded-windows",
        );
    }
    dynamic_island_lib::run()
}
