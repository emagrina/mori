fn main() {
    assert_eq!(std::env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("macos"));
    let mut build = cc::Build::new();
    build
        .file("../xpc/bridge.m")
        .define("MORI_SERVICE", None)
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .flag("-mmacosx-version-min=12.0");
    if std::env::var_os("CARGO_FEATURE_XPC_TESTING").is_some() {
        build.define("MORI_XPC_TESTING", None);
    }
    build.compile("mori_xpc_service");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=Security");
    println!("cargo:rerun-if-changed=../xpc/bridge.m");
}
