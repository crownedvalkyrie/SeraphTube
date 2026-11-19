fn main() {
    // Registers our commands as permissions (allow-<command>), so capabilities can gate them.
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_state", "get_stats", "set_theme", "set_adblock", "update_filters",
            "add_filter_list", "remove_filter_list", "toggle_filter_list",
            "open_themes_dir", "record_block", "open_settings",
            "get_performance", "set_performance", "set_intro",
        ])),
    )
    .expect("tauri-build failed");
}