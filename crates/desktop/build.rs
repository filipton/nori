fn main() {
    // Only a few std widgets are used (text fields, scroll bars); cupertino-dark suits both macOS and Linux.
    let config = slint_build::CompilerConfiguration::new().with_style("cupertino-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("the interface compiles");
}
