fn main() {
    // The frontend assets are embedded into the binary, and nothing else tells
    // cargo they are an input: without this, editing anything under `dist/` (or
    // rebuilding the UI) leaves cargo reporting "Finished" while the executable
    // still serves the previous CSS and JS. A directory path is watched
    // recursively, so this covers every asset.
    println!("cargo:rerun-if-changed=../dist");

    // `dialog.rs` declares `GetOpenFileNameW` from comdlg32 directly rather than
    // depending on the `windows` crate for two file pickers.
    #[cfg(windows)]
    println!("cargo:rustc-link-lib=comdlg32");

    tauri_build::build()
}
