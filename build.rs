fn main() {
    // Compile the .slint UI to Rust. Fonts/images are pre-rendered for the software renderer.
    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .expect("Slint build failed");

    // Passes ESP-IDF linker args and cfg flags from esp-idf-sys to this crate.
    embuild::espidf::sysenv::output();
}
