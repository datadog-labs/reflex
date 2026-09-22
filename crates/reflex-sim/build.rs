use std::path::Path;

fn main() {
    for file in [
        "src/playground/controls.js",
        "src/playground/controls.css",
        "src/playground/scenario-ui.js",
        "src/playground/scenario-ui.css",
        "src/typography.css",
    ] {
        println!("cargo:rerun-if-changed={file}");
        assert!(
            Path::new(file).is_file(),
            "Missing UI asset {file}. From the repository root run:\n  npm ci --prefix crates/reflex-sim/ui\n  npm run build --prefix crates/reflex-sim/ui\nThen rerun Cargo. Browser bundles are local build outputs, not checked-in source."
        );
    }
}
