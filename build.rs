fn main() {
    // Re-embed the exe icon whenever the icon files change.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
