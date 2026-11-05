//! Compiles the vendored `bc7enc_rdo` BC7 encoder, but only when the `bc7`
//! feature asks for it (it needs a C++ toolchain).

fn main() {
    println!("cargo:rerun-if-changed=vendor/bc7enc_rdo");
    if std::env::var_os("CARGO_FEATURE_BC7").is_none() {
        return;
    }

    let root = std::path::Path::new("vendor/bc7enc_rdo");
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .include(root)
        .file(root.join("bc7enc.cpp"))
        .file(root.join("rgbcx.cpp"))
        .file(root.join("bc7decomp.cpp"))
        .file(root.join("anima_bc7_shim.cpp"))
        .warnings(false)
        .compile("anima_bc7enc");
}
