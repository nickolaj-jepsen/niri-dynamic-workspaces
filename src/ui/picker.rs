use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, GestureClick, Label, Orientation, PolicyType, ScrolledWindow};

use crate::actions::HookInfo;
use crate::config::{ResolvedConfig, TemplateVariable};

use super::metrics::KeyboardMetrics;
use super::variables::show_variable_input;
use super::{
    attach_close_on_backdrop_click, build_hint_footer, create_error_revealer, display_key_char,
    format_workspace_display, matches_close_keybind, new_key_controller, populate_overlay,
    remove_app_controllers, switch_and_close, wrap_in_backdrop, wrap_index, ActionContext, Mode,
    ACTION_MODS,
};

/// An option in the template picker (either "Empty" or a named template).
pub(super) struct TemplateOption {
    pub(super) key: Option<char>,
    pub(super) name: String,
    pub(super) programs: Vec<String>,
    pub(super) variables: Vec<TemplateVariable>,
    pub(super) title: Option<String>,
}

fn build_template_options(config: &ResolvedConfig) -> Vec<TemplateOption> {
    let mut options = Vec::with_capacity(config.templates.len() + 1);

    // "Empty" option always gets key '1' (reserved during config resolution)
    options.push(TemplateOption {
        key: Some('1'),
        name: "Empty".to_string(),
        programs: config.default_programs.clone(),
        variables: Vec::new(),
        title: None,
    });

    for tmpl in &config.templates {
        options.push(TemplateOption {
            key: tmpl.key,
            name: tmpl.name.clone(),
            programs: tmpl.programs.clone(),
            variables: tmpl.variables.clone(),
            title: tmpl.title.clone(),
        });
    }

    options
}

fn build_template_option_widget(
    opt: &TemplateOption,
    is_selected: bool,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let mut classes = vec!["template-option"];
    if is_selected {
        classes.push("selected");
    }

    let row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(metrics.key_gap)
        .css_classes(classes)
        .build();

    // Key badge — styled like a small keyboard key
    let key_text = opt.key.map_or_else(String::new, display_key_char);
    let key_badge = Label::builder()
        .label(&key_text)
        .css_classes(["template-key"])
        .build();
    row.append(&key_badge);

    // Text column: name on top, programs below
    let text_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .halign(Align::Start)
        .hexpand(true)
        .build();

    let name_label = Label::builder()
        .label(&opt.name)
        .css_classes(["template-name"])
        .halign(Align::Start)
        .build();
    text_box.append(&name_label);

    if !opt.programs.is_empty() {
        let programs_text = opt.programs.join(", ");
        let programs_label = Label::builder()
            .label(&programs_text)
            .css_classes(["template-programs"])
            .halign(Align::Start)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(40)
            .build();
        text_box.append(&programs_label);
    }

    row.append(&text_box);
    row
}

fn update_selection(option_widgets: &[GtkBox], selected: usize) {
    for (i, w) in option_widgets.iter().enumerate() {
        if i == selected {
            w.add_css_class("selected");
        } else {
            w.remove_css_class("selected");
        }
    }
}

fn select_template_option(option: &TemplateOption, ch: char, ctx: &ActionContext) {
    let template_name = if option.name == "Empty" {
        None
    } else {
        Some(option.name.clone())
    };
    if option.variables.is_empty() {
        let hook_info = HookInfo {
            template_name,
            variables: HashMap::new(),
        };
        // No variables, so title stays as-is (no substitution needed)
        let full_name = crate::config::workspace_name_with_title(
            &ctx.session.config.workspace_prefix,
            ch,
            option.title.as_deref(),
        );
        switch_and_close(&full_name, ch, &option.programs, ctx, &hook_info);
    } else {
        show_variable_input(option, ch, ctx, template_name);
    }
}

pub(super) fn show_template_picker(ch: char, ctx: &ActionContext) {
    let window = &ctx.window;
    remove_app_controllers(window);

    let config = &ctx.session.config;
    let metrics =
        KeyboardMetrics::from_monitor_width(ctx.session.monitor_width.get(), config.layout);

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["popup-container", "template-picker"])
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    // Title
    let title = Label::builder()
        .label(format!(
            "Create workspace {}",
            format_workspace_display(ch, config)
        ))
        .css_classes(["template-title"])
        .build();
    container.append(&title);

    // Error revealer
    let (error_label, error_revealer) = create_error_revealer();

    // Build template options
    let options = build_template_options(config);
    let option_count = options.len();

    // Build option widgets
    let list_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(metrics.key_gap / 2)
        .css_classes(["template-list"])
        .build();

    let selected_idx = Rc::new(Cell::new(0_usize));
    let option_widgets: Vec<GtkBox> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| build_template_option_widget(opt, i == 0, &metrics))
        .collect();

    // Shared context for the template picker
    let picker_ctx = ActionContext {
        error_label,
        error_revealer: error_revealer.clone(),
        ..ctx.clone()
    };

    // Store options as Rc for sharing with handlers
    let options = Rc::new(options);
    let option_widgets_rc = Rc::new(option_widgets);

    for (i, widget) in option_widgets_rc.iter().enumerate() {
        list_box.append(widget);

        // Click handler
        let click_ctx = picker_ctx.clone();
        let click_options = options.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            select_template_option(&click_options[i], ch, &click_ctx);
        });
        widget.add_controller(click);
    }

    // Wrap in scrolled window for many templates
    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .max_content_height(metrics.key_size * 5)
        .propagate_natural_height(true)
        .child(&list_box)
        .build();
    container.append(&scrolled);

    container.append(&error_revealer);

    container.append(&build_hint_footer(
        &metrics,
        &[
            "press key to select",
            "\u{2191}\u{2193} navigate",
            "Enter confirm",
            "Escape cancel",
        ],
    ));

    wrap_in_backdrop(window, &container);

    // Key handler
    attach_template_key_handler(
        &picker_ctx,
        &options,
        &option_widgets_rc,
        &selected_idx,
        option_count,
        ch,
    );
    attach_close_on_backdrop_click(window, &container);
}

fn attach_template_key_handler(
    ctx: &ActionContext,
    options: &Rc<Vec<TemplateOption>>,
    option_widgets: &Rc<Vec<GtkBox>>,
    selected_idx: &Rc<Cell<usize>>,
    option_count: usize,
    ws_char: char,
) {
    let key_ctx = ctx.clone();
    let close_keybinds = ctx.session.config.close_keybinds.clone();
    let options = options.clone();
    let widgets = option_widgets.clone();
    let sel = selected_idx.clone();

    let key_controller = new_key_controller();
    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        // Close keybinds / Escape → go back to main view
        if matches_close_keybind(key, modifier, &close_keybinds) {
            let ctx = key_ctx.clone();
            glib::idle_add_local_once(move || {
                populate_overlay(&ctx.window, &ctx.session, Mode::Normal, None);
            });
            return Propagation::Stop;
        }

        // Arrow Up/Down → navigate
        let is_up = key == gdk4::Key::Up || key == gdk4::Key::KP_Up;
        let is_down = key == gdk4::Key::Down || key == gdk4::Key::KP_Down;
        if is_up || is_down {
            let new_idx = wrap_index(sel.get(), option_count, is_down);
            sel.set(new_idx);
            update_selection(&widgets, new_idx);
            return Propagation::Stop;
        }

        // Enter → confirm selected
        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            let idx = sel.get();
            select_template_option(&options[idx], ws_char, &key_ctx);
            return Propagation::Stop;
        }

        // Shortcut keys — match template options
        if let Some(pressed) = key.to_unicode() {
            let pressed = pressed.to_ascii_lowercase();
            if (modifier & ACTION_MODS).is_empty() {
                for opt in options.iter() {
                    if opt.key == Some(pressed) {
                        select_template_option(opt, ws_char, &key_ctx);
                        return Propagation::Stop;
                    }
                }
            }
        }

        Propagation::Proceed
    });
    ctx.window.add_controller(key_controller);
}

#[cfg(test)]
mod tests {
    use super::super::cards::tests::default_test_config;
    use super::*;

    #[test]
    fn build_template_options_empty_first_with_key_1() {
        use crate::config::Template;

        let mut config = default_test_config();
        config.default_programs = vec!["kitty".to_string()];
        config.templates = vec![
            Template {
                name: "dev".to_string(),
                programs: vec!["code".to_string()],
                key: Some('d'),
                variables: Vec::new(),
                on_create: Vec::new(),
                title: None,
            },
            Template {
                name: "browser".to_string(),
                programs: vec!["firefox".to_string()],
                key: Some('2'),
                variables: Vec::new(),
                on_create: Vec::new(),
                title: None,
            },
        ];

        let opts = build_template_options(&config);

        assert_eq!(opts.len(), 3);
        // First option is always "Empty" with key '1'
        assert_eq!(opts[0].name, "Empty");
        assert_eq!(opts[0].key, Some('1'));
        assert_eq!(opts[0].programs, vec!["kitty"]);
        // Templates follow in config order
        assert_eq!(opts[1].name, "dev");
        assert_eq!(opts[1].key, Some('d'));
        assert_eq!(opts[2].name, "browser");
        assert_eq!(opts[2].key, Some('2'));
    }

    #[test]
    fn build_template_options_no_default_programs() {
        use crate::config::Template;

        let mut config = default_test_config();
        config.templates = vec![Template {
            name: "dev".to_string(),
            programs: vec!["code".to_string()],
            key: Some('2'),
            variables: Vec::new(),
            on_create: Vec::new(),
            title: None,
        }];

        let opts = build_template_options(&config);

        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].name, "Empty");
        assert!(opts[0].programs.is_empty());
    }
}
