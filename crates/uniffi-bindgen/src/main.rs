// Generates the Kotlin bindings: `bindings src:nori-android <out dir>`.
fn main() {
    uniffi_bindgen_kotlin_jni::main().unwrap();
    let args: Vec<String> = std::env::args().collect();
    if let (Some("bindings"), Some(out)) = (args.get(1).map(String::as_str), args.get(3)) {
        let path = std::path::Path::new(out).join("uniffi").join("Uniffi.kt");
        if let Ok(kt) = std::fs::read_to_string(&path) {
            std::fs::write(&path, nullable_futures(&kt)).unwrap();
        }
    }
}

/// Upstream bug workaround: async exports end in `return completion.value!!`, which throws when a
/// nullable return (`T?`) is null. Drops the `!!` for those. Remove once the pinned revision fixes it.
fn nullable_futures(kt: &str) -> String {
    let mut out = String::with_capacity(kt.len());
    let mut nullable = false;
    for line in kt.split_inclusive('\n') {
        if let Some(ret) = line.trim_end().strip_prefix(") : ") {
            nullable = ret.ends_with('?');
        }
        if nullable && line.trim() == "return completion.value!!" {
            out.push_str(&line.replacen("completion.value!!", "completion.value", 1));
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn nullable_future_drops_non_null_assertion() {
        let kt = "suspend fun a(\n    f: kotlin.Long,\n) : kotlin.String?\n{\n                return completion.value!!\n}\nsuspend fun b(\n    f: kotlin.Long,\n) : kotlin.String\n{\n                return completion.value!!\n}\n";
        let fixed = super::nullable_futures(kt);
        assert_eq!(fixed.matches("return completion.value!!").count(), 1);
        assert!(fixed.contains("kotlin.String?\n{\n                return completion.value\n}"));
    }
}
