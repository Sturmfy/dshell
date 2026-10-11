fn main() {
    // `dialog.rs` declares `GetOpenFileNameW` from comdlg32 directly rather than
    // depending on the `windows` crate for two file pickers.
    #[cfg(windows)]
    println!("cargo:rustc-link-lib=comdlg32");

    tauri_build::build()
}
