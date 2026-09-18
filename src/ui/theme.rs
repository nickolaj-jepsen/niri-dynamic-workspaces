use std::cell::RefCell;
use std::path::Path;

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
pub(super) fn apply(theme: &Theme) {
    let Some(display) = gdk4::Display::default() else {
        return;
    };
    let (provider, priority) = match theme {
        Theme::File(path) => match from_file(path) {
            Some(provider) => (provider, PRIORITY_THEME_FILE),
            None => (builtin(&Theme::Gtk), PRIORITY_BUILTIN_THEME),
        },
        builtin_theme => (builtin(builtin_theme), PRIORITY_BUILTIN_THEME),
    };
    THEME_PROVIDER.with(|cell| {
        if let Some(old) = cell.borrow_mut().replace(provider.clone()) {
            gtk4::style_context_remove_provider_for_display(&display, &old);
        }
    });
    add(&display, &provider, priority);
}

fn builtin(theme: &Theme) -> CssProvider {
    from_data(match theme {
        Theme::Dark => include_str!("../../themes/dark.css"),
        Theme::Light => include_str!("../../themes/light.css"),
        Theme::Gtk | Theme::File(_) => include_str!("../../themes/gtk.css"),
    })
}

/// Load a user theme file, reporting CSS errors on stderr. Whatever parsed
/// still applies; `None` only when the file cannot be read at all.
fn from_file(path: &Path) -> Option<CssProvider> {
    if let Err(e) = std::fs::metadata(path) {
        eprintln!(
            "theme warning: could not read {}: {e}, using the gtk theme",
            path.display()
        );
        return None;
    }
    let provider = CssProvider::new();
    provider.connect_parsing_error(|_, section, error| {
        eprintln!("theme warning: {}: {}", section.to_str(), error.message());
    });
    provider.load_from_path(path);
    Some(provider)
}

fn from_data(css: &str) -> CssProvider {
    let provider = CssProvider::new();
    provider.load_from_data(css);
    provider
}

fn add(display: &gdk4::Display, provider: &CssProvider, priority: u32) {
    gtk4::style_context_add_provider_for_display(display, provider, priority);
}
