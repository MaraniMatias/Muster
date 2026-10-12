fn main() {
    // A capability may only name a permission the build can resolve, and the automation
    // bridge is an optional plugin: a release build has no bridge to resolve it, and naming
    // it there would fail the build. So the bridge's own capability joins the build only
    // where the plugin is in the graph, and every build keeps the app's capabilities.
    let capabilities: &'static str = if std::env::var_os("CARGO_FEATURE_DEV_BRIDGE").is_some() {
        "./capabilities/*.json"
    } else {
        "./capabilities/default.json"
    };
    println!("cargo:rerun-if-changed=capabilities");
    tauri_build::try_build(tauri_build::Attributes::new().capabilities_path_pattern(capabilities))
        .expect("failed to run tauri-build");
}
