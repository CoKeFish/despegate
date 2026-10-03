//! Embeds the icon in the three executables. Needs the Windows SDK's
//! resource compiler; without it the build goes on without an icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/despegate.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/despegate.ico");
    if let Err(e) = resource.compile() {
        println!("cargo:warning=no icon embedded: {e}");
    }
}
