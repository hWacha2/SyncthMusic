// Предотвращаем открытие консоли в релизной сборке Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    syncth_desktop_lib::run()
}
