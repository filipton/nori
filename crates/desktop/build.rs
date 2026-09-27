fn main() {
    // The dark Cupertino style: the few std widgets used (the text fields, scroll bars) look at home on a Mac
    // and do not clash on Linux; everything else is drawn by the .slint files themselves.
    let config = slint_build::CompilerConfiguration::new().with_style("cupertino-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("the interface compiles");
}
