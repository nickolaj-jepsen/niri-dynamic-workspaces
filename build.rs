//! Generates `BUILTIN_THEMES` from `themes/*.css`; `gtk-*.css` are support files, not themes.

use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=themes");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let themes_dir = Path::new(&manifest_dir).join("themes");

    let mut themes: Vec<_> = fs::read_dir(&themes_dir)
        .expect("themes/ is readable")
        .filter_map(|entry| {
            let path = entry.expect("themes/ entry is readable").path();
            let name = path.file_stem()?.to_str()?.to_owned();
            let is_theme =
                path.extension().is_some_and(|ext| ext == "css") && !name.starts_with("gtk-");
            // A string, so Debug formatting below yields an escaped literal.
            is_theme.then_some((name, path.to_str()?.to_owned()))
        })
        .collect();
    themes.sort();

    assert!(
        themes.iter().any(|(name, _)| name == "gtk"),
        "themes/gtk.css is the default theme and must exist"
    );

    let mut table = String::from("pub static BUILTIN_THEMES: &[BuiltinTheme] = &[\n");
    for (name, path) in &themes {
        writeln!(
            table,
            "    BuiltinTheme {{ name: {name:?}, css: include_str!({path:?}) }},"
        )
        .expect("writing to a String cannot fail");
    }
    table.push_str("];\n");

    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    fs::write(Path::new(&out_dir).join("builtin_themes.rs"), table).expect("OUT_DIR is writable");
}
