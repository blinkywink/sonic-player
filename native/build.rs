fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/sonic.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/sonic.rc", embed_resource::NONE)
            .manifest_required()
            .expect("embed Sonic's Windows icon");
    }
    let config = slint_build::CompilerConfiguration::new().with_debug_info(true);
    slint_build::compile_with_config("src/sonic.slint", config).expect("compile Sonic UI");
}
