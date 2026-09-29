//! Checks that every file under each crate's `tests/` is reachable from a test target (a `[[test]]` or
//! an auto-discovered one) through `mod` declarations; an unregistered file silently never runs.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/").to_path_buf()
}

/// The test targets Cargo builds for one crate: its `[[test]]` paths, plus `tests/*.rs` and
/// `tests/*/main.rs` unless `autotests = false`.
fn targets(krate: &Path) -> Vec<PathBuf> {
    let manifest = fs::read_to_string(krate.join("Cargo.toml")).unwrap_or_default();
    let mut out = Vec::new();
    let mut in_test = false;
    let mut autotests = true;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_test = line == "[[test]]";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim().trim_matches('"'));
        if key == "autotests" && value == "false" {
            autotests = false;
        }
        if in_test && key == "path" {
            out.push(krate.join(value));
        }
    }
    if autotests {
        for entry in fs::read_dir(krate.join("tests")).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            } else if p.join("main.rs").is_file() {
                out.push(p.join("main.rs"));
            }
        }
    }
    out
}

/// Adds every file reachable from `root` through `mod x;` and `#[path = "..."] mod x;` to `seen`.
/// `owns_dir`: children live beside `root` (crate roots, `#[path]` files), otherwise in `<stem>/`.
fn reach(root: &Path, owns_dir: bool, seen: &mut BTreeSet<PathBuf>) {
    let Ok(root) = root.canonicalize() else { return };
    if !seen.insert(root.clone()) {
        return;
    }
    let text = fs::read_to_string(&root).unwrap_or_default();
    let here = root.parent().unwrap().to_path_buf();
    let dir = if owns_dir || root.file_name().is_some_and(|n| n == "mod.rs") {
        here.clone()
    } else {
        here.join(root.file_stem().unwrap())
    };
    let mut path_attr: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#[path = \"") {
            path_attr = rest.split('"').next().map(str::to_owned);
            continue;
        }
        let decl = line.strip_prefix("pub ").unwrap_or(line);
        let decl = decl.strip_prefix("pub(crate) ").unwrap_or(decl);
        if let Some(name) = decl.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) {
            match path_attr.take() {
                Some(p) => reach(&here.join(p), true, seen),
                None => {
                    let flat = dir.join(format!("{name}.rs"));
                    let nested = dir.join(name).join("mod.rs");
                    reach(if flat.is_file() { &flat } else { &nested }, false, seen);
                }
            }
        } else if !line.starts_with("#[") {
            path_attr = None;
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p.canonicalize().unwrap());
        }
    }
}

#[test]
fn every_test_file_is_registered() {
    let mut orphans = Vec::new();
    let mut checked = 0;
    let root = crates_dir().canonicalize().unwrap();
    for krate in fs::read_dir(&root).unwrap().flatten().map(|e| e.path()) {
        let tests = krate.join("tests");
        if !tests.is_dir() {
            continue;
        }
        let mut seen = BTreeSet::new();
        for target in targets(&krate) {
            reach(&target, true, &mut seen);
        }
        let mut files = Vec::new();
        rust_files(&tests, &mut files);
        checked += files.len();
        orphans.extend(files.into_iter().filter(|f| !seen.contains(f)));
    }
    assert!(checked > 20, "only {checked} test files found under crates/*/tests: is the walk looking in the right place?");
    assert!(
        orphans.is_empty(),
        "these test files are in no test binary, so their tests never run: add a [[test]] to the crate's \
         Cargo.toml (autotests = false) or a `mod` to the binary's main.rs:\n  {}",
        orphans.iter().map(|p| format!("crates/{}", p.strip_prefix(&root).unwrap_or(p).display())).collect::<Vec<_>>().join("\n  ")
    );
}
