use std::fs;
use std::path::Path;

fn main() {
    let icons_dir = Path::new("icons");
    if !icons_dir.exists() {
        let _ = fs::create_dir_all(icons_dir);
    }

    let icon_path = icons_dir.join("icon.ico");
    if !icon_path.exists() {
        let minimal_ico: [u8; 70] = [
            0, 0, 1, 0, 1, 0,
            1, 1, 0, 0, 1, 0, 32, 0,
            40, 0, 0, 0, 22, 0, 0, 0,
            40, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 0, 32, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let _ = fs::write(&icon_path, &minimal_ico);
    }

    let mut windows = tauri_build::WindowsAttributes::new();
    windows = windows.app_manifest(include_str!("app.manifest"));
    tauri_build::try_build(
        tauri_build::Attributes::new().windows_attributes(windows)
    ).expect("failed to run build script");
}
