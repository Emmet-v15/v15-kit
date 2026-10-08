const COMMANDS: &[&str] = &["info", "check"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
