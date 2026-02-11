use std::collections::HashMap;
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{Align, ApplicationWindow, Box as GtkBox, Entry, Label, Orientation};
use relm4::{Component, ComponentParts, ComponentSender};

use super::helpers::{build_hint_footer, format_workspace_display, new_key_controller};
use super::metrics::KeyboardMetrics;
use crate::config::{ResolvedConfig, TemplateVariable};

pub struct VariableInputInit {
    pub config: Rc<ResolvedConfig>,
    pub ws_char: char,
    pub template_name: Option<String>,
    pub var_names: Vec<String>,
    pub variables: Vec<TemplateVariable>,
    pub metrics: KeyboardMetrics,
}

pub struct VariableInput {
    var_names: Vec<String>,
    entries: Vec<Entry>,
}

pub struct VariableInputWidgets {}

#[derive(Debug)]
pub enum VariableInputMsg {
    Submit,
}

#[derive(Debug)]
pub enum VariableInputOutput {
    Submitted(HashMap<String, String>),
}

impl Component for VariableInput {
    type Init = VariableInputInit;
    type Input = VariableInputMsg;
    type Output = VariableInputOutput;
    type CommandOutput = ();
    type Root = GtkBox;
    type Widgets = VariableInputWidgets;

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
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let tmpl_name = init.template_name.as_deref().unwrap_or("Template");

        let title = Label::builder()
            .label(format!(
                "{tmpl_name} \u{2192} {}",
                format_workspace_display(init.ws_char, &init.config)
            ))
            .css_classes(["variable-title"])
            .build();
        root.append(&title);

        let form = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(init.metrics.key_gap / 2)
            .css_classes(["variable-form"])
            .build();

        let entries: Vec<Entry> = init
            .variables
            .iter()
            .map(|var| {
                let row = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(0)
                    .css_classes(["variable-row"])
                    .build();

                let label = Label::builder()
                    .label(&var.label)
                    .css_classes(["variable-label"])
                    .halign(Align::Start)
                    .build();
                row.append(&label);

                let entry = Entry::builder()
                    .css_classes(["variable-entry"])
                    .placeholder_text(&var.name)
                    .build();
                row.append(&entry);

                form.append(&row);
                entry
            })
            .collect();

        root.append(&form);

        root.append(&build_hint_footer(
            &init.metrics,
            &["Enter create", "Escape back"],
        ));

        if let Some(first) = entries.first() {
            first.grab_focus();
        }

        let model = VariableInput {
            var_names: init.var_names,
            entries,
        };

        let widgets = VariableInputWidgets {};

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            VariableInputMsg::Submit => {
                let mut values = HashMap::new();
                for (name, entry) in self.var_names.iter().zip(self.entries.iter()) {
                    values.insert(name.clone(), entry.text().to_string());
                }
                sender.output(VariableInputOutput::Submitted(values)).ok();
            }
        }
    }
}

pub fn attach_key_handler(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    sender: &relm4::Sender<VariableInputMsg>,
) {
    let close_keybinds = config.close_keybinds.clone();
    let key_sender = sender.clone();
    let key_controller = new_key_controller();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        if super::helpers::matches_close_keybind(key, modifier, &close_keybinds) {
            // Close/Escape in variable input means "Back"
            return Propagation::Proceed;
        }

        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            key_sender.send(VariableInputMsg::Submit).ok();
            return Propagation::Stop;
        }

        // Let GTK handle Tab, text input, etc.
        Propagation::Proceed
    });
    window.add_controller(key_controller);
}
