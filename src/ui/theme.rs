use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gtk4::CssProvider;

use crate::config::Theme;

/// Above the structural stylesheet and the scaled metrics.
const PRIORITY_BUILTIN_THEME: u32 = gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 2;
/// Above ~/.config/gtk-4.0/gtk.css, which GTK loads at user priority.
const PRIORITY_THEME_FILE: u32 = gtk4::STYLE_PROVIDER_PRIORITY_USER + 1;

thread_local! {
    static THEME_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
}

/// Register the stylesheets every theme builds on.
pub fn install_base(display: &gdk4::Display) {
    add(
        display,
        &from_data(include_str!("../../themes/gtk-fallback.css")),
        gtk4::STYLE_PROVIDER_PRIORITY_FALLBACK,
    );
    add(
        display,
        &from_data(concat!(
            include_str!("../../style.css"),
            include_str!("../../themes/gtk-primaries.css")
        )),
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Replace the active theme. Files are re-read each call, so edits apply on the next open.
///
/// Returns the theme file's problems, which are also printed to stderr.
pub(super) fn apply(theme: &Theme) -> Vec<String> {
    let Some(display) = gdk4::Display::default() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let (provider, priority) = match theme {
        Theme::Builtin(builtin) => (from_data(builtin.css), PRIORITY_BUILTIN_THEME),
        Theme::File(path) => {
            let Some(provider) = from_file(path, &mut warnings) else {
                apply(&Theme::default());
                return warnings;
            };
            (provider, PRIORITY_THEME_FILE)
        }
    };
    THEME_PROVIDER.with(|cell| {
        if let Some(old) = cell.borrow_mut().replace(provider.clone()) {
            gtk4::style_context_remove_provider_for_display(&display, &old);
        }
    });
    add(&display, &provider, priority);
    warnings
}

/// Load a theme file, adding CSS errors to `warnings` and stderr; `None`
/// only when it cannot be read.
fn from_file(path: &Path, warnings: &mut Vec<String>) -> Option<CssProvider> {
    if let Err(e) = std::fs::metadata(path) {
        warn(
            warnings,
            &format!(
                "could not read {}: {e}, using the gtk theme",
                path.display()
            ),
        );
        return None;
    }
    let provider = CssProvider::new();
    let parse_errors = Rc::new(RefCell::new(Vec::new()));
    let sink = parse_errors.clone();
    provider.connect_parsing_error(move |_, section, error| {
        warn(
            &mut sink.borrow_mut(),
            &format!("{}: {}", section.to_str(), error.message()),
        );
    });
    // Parsing is synchronous, so every error has been reported once this returns.
    provider.load_from_path(path);
    warnings.append(&mut parse_errors.borrow_mut());
    Some(provider)
}

/// Print a theme warning (render-lib.sh greps the prefix) and keep it.
fn warn(warnings: &mut Vec<String>, message: &str) {
    let warning = format!("theme warning: {message}");
    eprintln!("{warning}");
    warnings.push(warning);
}

fn from_data(css: &str) -> CssProvider {
    let provider = CssProvider::new();
    provider.load_from_data(css);
    provider
}

fn add(display: &gdk4::Display, provider: &CssProvider, priority: u32) {
    gtk4::style_context_add_provider_for_display(display, provider, priority);
}
