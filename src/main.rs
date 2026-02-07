mod config;
mod niri;
mod ui;

use gtk4::prelude::*;
use gtk4::{gdk, CssProvider};

fn main() {
    let cfg = config::load_config();

    let app = gtk4::Application::builder()
        .application_id("dev.nickolaj.niri-dynamic-workspaces")
        .build();

    app.connect_startup(|_| {
        let provider = CssProvider::new();
        provider.load_from_data(include_str!("../style.css"));
        gtk4::style_context_add_provider_for_display(
            &gdk::Display::default().expect("Could not get default display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });

    app.connect_activate(move |app| {
        ui::build_ui(app, &cfg);
    });

    app.run_with_args::<&str>(&[]);
}
