fn main() {
    let config = slint_build::CompilerConfiguration::new();
    slint_build::compile_with_config("src/ui.slint", config).expect("compile Sonic mobile UI");
}
