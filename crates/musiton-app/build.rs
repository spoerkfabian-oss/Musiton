//! Embeds the application icon into the Windows executable (also when cross-compiling
//! from Linux with the MinGW `windres`).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    if let Err(e) = res.compile() {
        println!("cargo:warning=could not embed the icon: {e}");
    }
}
