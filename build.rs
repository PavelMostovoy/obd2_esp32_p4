fn main() {
    // Passes ESP-IDF linker args and cfg flags from esp-idf-sys to this crate.
    embuild::espidf::sysenv::output();
}
