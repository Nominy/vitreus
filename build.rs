fn main() {
    println!("cargo:rerun-if-changed=assets/windows/vitreus.rc");
    println!("cargo:rerun-if-changed=assets/app/vitreus.ico");
    println!("cargo:rerun-if-changed=assets/app/vitreus-file.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Icon resource id 1 is what gpui loads for the window class.
        embed_resource::compile("assets/windows/vitreus.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
