fn main() {
    println!("cargo:rerun-if-env-changed=REFLEX_REQUESTED_PROFILE");
    println!("cargo:rerun-if-env-changed=REFLEX_REQUESTED_RUSTFLAGS");
    println!(
        "cargo:rustc-env=REFLEX_BUILD_PROFILE={}",
        std::env::var("REFLEX_REQUESTED_PROFILE")
            .or_else(|_| std::env::var("PROFILE"))
            .unwrap_or_else(|_| "unknown".into())
    );
    println!(
        "cargo:rustc-env=REFLEX_BUILD_RUSTFLAGS={}",
        std::env::var("REFLEX_REQUESTED_RUSTFLAGS").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=REFLEX_BUILD_TARGET_FEATURES={}",
        std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default()
    );
}
