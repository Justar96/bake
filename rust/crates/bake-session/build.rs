fn main() {
    println!("cargo:rerun-if-env-changed=ZSTD_SYS_USE_PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=DEP_ZSTD_ROOT");
    assert!(
        std::env::var_os("ZSTD_SYS_USE_PKG_CONFIG").is_none()
            && std::env::var_os("DEP_ZSTD_ROOT").is_some(),
        "bake-session requires the pinned vendored Zstd decoder; unset ZSTD_SYS_USE_PKG_CONFIG"
    );
}
