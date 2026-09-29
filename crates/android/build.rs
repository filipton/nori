use std::path::PathBuf;

fn main() {
    // The uniffi scaffolding as `Java_uniffi_Scaffolding_*` JNI functions.
    uniffi_bindgen_kotlin_jni::generate_scaffolding();

    // A library has one `JNI_OnLoad` and ours registers the natives, so the generated one is renamed and
    // called from ours. A uniffi revision that generates it differently fails the build here.
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("uniffi_bindgen_kotlin_jni.uniffi.rs");
    let scaffolding = std::fs::read_to_string(&path).expect("the scaffolding was generated");
    let mut lines: Vec<&str> = scaffolding.lines().collect();
    let on_load = lines.iter().position(|l| l.trim_start().starts_with("unsafe extern \"system\" fn JNI_OnLoad(")).expect("the generated scaffolding has a JNI_OnLoad");
    assert!(on_load > 0 && lines[on_load - 1].trim() == "#[unsafe(no_mangle)]", "the generated JNI_OnLoad is exported as expected");
    let renamed = lines[on_load].replacen("unsafe extern \"system\" fn JNI_OnLoad(", "pub(crate) unsafe extern \"system\" fn uniffi_on_load(", 1);
    lines[on_load] = &renamed;
    lines.remove(on_load - 1);
    let scaffolding = lines.join("\n");
    assert!(!scaffolding.contains("JNI_OnLoad"), "the generated scaffolding has a second JNI_OnLoad");
    std::fs::write(&path, scaffolding).expect("the scaffolding is writable");

    // The scaffolding is generated from these crates' sources, which cargo does not watch for us.
    println!("cargo:rerun-if-changed=build.rs");
    for krate in ["core", "model", "db", "net", "library", "automix", "settings", "lyrics", "devices", "queue", "transfers", "perf"] {
        for input in ["src", "Cargo.toml", "uniffi.toml"] {
            println!("cargo:rerun-if-changed=../{krate}/{input}");
        }
    }
}
