// Точка входа desktop-приложения.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Этот же exe запускается с правами администратора только для того,
    // чтобы добавить правило брандмауэра. В обычном запуске флага нет.
    if std::env::args().any(|arg| arg == "--allow-firewall") {
        std::process::exit(syncth_desktop_lib::firewall_helper());
    }
    syncth_desktop_lib::run()
}
