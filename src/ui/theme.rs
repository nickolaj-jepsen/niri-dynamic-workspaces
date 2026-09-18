use gtk4::CssProvider;

/// Register the stylesheets every overlay needs: GTK colour-name fallbacks,
/// the structural stylesheet and the GTK-derived primaries.
pub fn install_base(display: &gdk4::Display) {
    add(
        display,
        include_str!("../../themes/gtk-fallback.css"),
        gtk4::STYLE_PROVIDER_PRIORITY_FALLBACK,
    );
    add(
        display,
        concat!(
            include_str!("../../style.css"),
            include_str!("../../themes/gtk-primaries.css")
        ),
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    add(
        display,
        include_str!("../../themes/gtk.css"),
        PRIORITY_THEME,
    );
}

/// Above the structural stylesheet and the scaled metrics.
const PRIORITY_THEME: u32 = gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 2;

fn add(display: &gdk4::Display, css: &str, priority: u32) -> CssProvider {
    let provider = CssProvider::new();
    provider.load_from_data(css);
    gtk4::style_context_add_provider_for_display(display, &provider, priority);
    provider
}
