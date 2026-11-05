const COMMANDS: &[&str] = &["start", "stop", "touch", "setForeground", "status"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
