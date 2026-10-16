fn main() {
    // Unicorn's QEMU code uses 16-byte atomics; with GCC on Linux/glibc these
    // are provided by libatomic, which Rust does not link by default.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "linux" && env == "gnu" {
        println!("cargo:rustc-link-lib=atomic");
    }
}
