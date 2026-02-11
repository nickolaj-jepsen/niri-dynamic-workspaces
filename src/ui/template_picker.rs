use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, GestureClick, Label, Orientation, PolicyType,
    ScrolledWindow,
};
use relm4::{Component, ComponentParts, ComponentSender};

use super::helpers::{
    build_hint_footer, display_key_char, format_workspace_display, new_key_controller,
    update_selection,
};
use super::metrics::KeyboardMetrics;
use super::types::{build_template_options, TemplateOption};
use super::ACTION_MODS;
use crate::config::ResolvedConfig;

pub struct TemplatePickerInit {
    pub config: Rc<ResolvedConfig>,
    pub ws_char: char,
    pub metrics: KeyboardMetrics,
}

pub struct TemplatePicker {
    selected_idx: usize,
    options: Vec<TemplateOption>,
    option_widgets: Vec<GtkBox>,
}

pub struct TemplatePickerWidgets {}

#[derive(Debug)]
pub enum TemplatePickerMsg {
    Up,
    Down,
    Confirm,
    SelectAndConfirm(usize),
    Hotkey(char),
}

#[derive(Debug)]
pub enum TemplatePickerOutput {
    Selected {
        template_name: Option<String>,
        programs: Vec<String>,
        var_names: Vec<String>,
        variables: Vec<crate::config::TemplateVariable>,
    },
}

impl Component for TemplatePicker {
    type Init = TemplatePickerInit;
    type Input = TemplatePickerMsg;
    type Output = TemplatePickerOutput;
    type CommandOutput = ();
    type Root = GtkBox;
    type Widgets = TemplatePickerWidgets;

    fn init_root() -> Self::Root {
        GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(0)
            .halign(Align::Center)
            .valign(Align::Center)
            .build()
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let options = build_template_options(&init.config);

        let title = Label::builder()
            .label(format!(
                "Create workspace {}",
                format_workspace_display(init.ws_char, &init.config)
            ))
            .css_classes(["template-title"])
            .build();
        root.append(&title);

        let list_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(init.metrics.key_gap / 2)
            .css_classes(["template-list"])
            .build();

        let option_widgets: Vec<GtkBox> = options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let widget = build_template_option_widget(opt, i == 0, &init.metrics);
                list_box.append(&widget);

                let click_sender = sender.clone();
                let click = GestureClick::new();
                click.connect_released(move |_, _, _, _| {
                    click_sender.input(TemplatePickerMsg::SelectAndConfirm(i));
                });
                widget.add_controller(click);

                widget
            })
            .collect();

        let scrolled = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .max_content_height(init.metrics.key_size * 5)
            .propagate_natural_height(true)
            .child(&list_box)
            .build();
        root.append(&scrolled);

        root.append(&build_hint_footer(
            &init.metrics,
            &[
                "press key to select",
                "\u{2191}\u{2193} navigate",
                "Enter confirm",
                "Escape cancel",
            ],
        ));

        let model = TemplatePicker {
            selected_idx: 0,
            options,
            option_widgets,
        };

        let widgets = TemplatePickerWidgets {};

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            TemplatePickerMsg::Up => {
                if !self.options.is_empty() {
                    let count = self.options.len();
                    self.selected_idx = if self.selected_idx == 0 {
                        count - 1
                    } else {
                        self.selected_idx - 1
                    };
                    update_selection(&self.option_widgets, self.selected_idx);
                }
            }
            TemplatePickerMsg::Down => {
                if !self.options.is_empty() {
                    let count = self.options.len();
                    self.selected_idx = if self.selected_idx >= count - 1 {
                        0
                    } else {
                        self.selected_idx + 1
                    };
                    update_selection(&self.option_widgets, self.selected_idx);
                }
            }
            TemplatePickerMsg::Confirm => {
                self.confirm_selection(&sender);
            }
            TemplatePickerMsg::SelectAndConfirm(idx) => {
                self.selected_idx = idx;
                self.confirm_selection(&sender);
            }
            TemplatePickerMsg::Hotkey(pressed) => {
                if let Some(idx) = self.options.iter().position(|opt| opt.key == Some(pressed)) {
                    self.selected_idx = idx;
                    self.confirm_selection(&sender);
                }
            }
        }
    }
}

impl TemplatePicker {
    fn confirm_selection(&self, sender: &ComponentSender<Self>) {
        let idx = self.selected_idx;
        if idx >= self.options.len() {
            return;
        }

        let template_name = if self.options[idx].name == "Empty" {
            None
        } else {
            Some(self.options[idx].name.clone())
        };

        let var_names: Vec<String> = self.options[idx]
            .variables
            .iter()
            .map(|v| v.name.clone())
            .collect();

        sender
            .output(TemplatePickerOutput::Selected {
                template_name,
                programs: self.options[idx].programs.clone(),
                var_names,
                variables: self.options[idx].variables.clone(),
            })
            .ok();
    }
}

pub fn attach_key_handler(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    sender: &relm4::Sender<TemplatePickerMsg>,
) {
    let close_keybinds = config.close_keybinds.clone();
    let key_sender = sender.clone();
    let key_controller = new_key_controller();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        if super::helpers::matches_close_keybind(key, modifier, &close_keybinds) {
            // Close/Escape in template picker means "Back"
            return Propagation::Proceed;
        }

        if key == gdk4::Key::Up || key == gdk4::Key::KP_Up {
            key_sender.send(TemplatePickerMsg::Up).ok();
            return Propagation::Stop;
        }
        if key == gdk4::Key::Down || key == gdk4::Key::KP_Down {
            key_sender.send(TemplatePickerMsg::Down).ok();
            return Propagation::Stop;
        }

        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            key_sender.send(TemplatePickerMsg::Confirm).ok();
            return Propagation::Stop;
        }

        if let Some(pressed) = key.to_unicode() {
            let pressed = pressed.to_ascii_lowercase();
            if (modifier & ACTION_MODS).is_empty() {
                key_sender.send(TemplatePickerMsg::Hotkey(pressed)).ok();
                return Propagation::Stop;
            }
        }

        Propagation::Proceed
    });
    window.add_controller(key_controller);
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

    let key_text = opt.key.map_or_else(String::new, display_key_char);
    let key_badge = Label::builder()
        .label(&key_text)
        .css_classes(["template-key"])
        .build();
    row.append(&key_badge);

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
