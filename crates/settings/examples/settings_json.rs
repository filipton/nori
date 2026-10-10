//! Prints every offered setting as JSON (name, kind, options, range, default), for the website's
//! settings reference: `cargo run -j4 -q -p nori-settings --example settings_json`.

use nori_settings::settings_model::{specs, SettingKind};
use serde_json::json;

fn main() {
    let specs: Vec<_> = specs()
        .into_iter()
        .map(|s| {
            let kind = match s.kind {
                SettingKind::Switch => "switch",
                SettingKind::Choice => "choice",
                SettingKind::Level => "level",
                SettingKind::Text => "text",
                SettingKind::Colour => "colour",
            };
            json!({ "name": s.name, "kind": kind, "options": s.options, "min": s.min, "max": s.max, "default": s.default })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&specs).expect("plain values serialise"));
}
