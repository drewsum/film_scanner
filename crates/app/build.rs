//! Embeds the icon and version details in the Windows .exe (shown by Explorer and the taskbar).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "Film Scanner")
        .set("FileDescription", "Film Scanner");
    if let Err(e) = res.compile() {
        // Without a resource compiler the program still builds, just without the icon.
        println!("cargo:warning=no icon embedded: {e}");
    }
}
