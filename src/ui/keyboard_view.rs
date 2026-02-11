use std::collections::HashMap;
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{Align, ApplicationWindow, Box as GtkBox, GestureClick, Label, Orientation};
use relm4::{Component, ComponentParts, ComponentSender};

use super::helpers::{build_hint_footer, display_key_char, new_key_controller};
use super::metrics::KeyboardMetrics;
use super::mode::Mode;
use super::types::DynWorkspaceInfo;
use super::ACTION_MODS;
use crate::config::ResolvedConfig;

pub struct KeyboardViewInit {
    pub config: Rc<ResolvedConfig>,
    pub infos: HashMap<char, DynWorkspaceInfo>,
    pub mode: Mode,
    pub metrics: KeyboardMetrics,
}

pub struct KeyboardView {
    config: Rc<ResolvedConfig>,
    infos: HashMap<char, DynWorkspaceInfo>,
    mode: Mode,
    metrics: KeyboardMetrics,
}

pub struct KeyboardViewWidgets {}

#[derive(Debug)]
pub enum KeyboardViewMsg {
    WorkspaceKeyPressed(char),
    NextMode,
    PrevMode,
    SetMode(Mode),
}

#[derive(Debug)]
pub enum KeyboardViewOutput {
    ExecuteSwitch { ch: char, programs: Vec<String> },
    ExecuteDelete(char),
    ExecuteMove(char),
    ShowTemplates(char),
    ModeChanged(Mode),
}

impl Component for KeyboardView {
    type Init = KeyboardViewInit;
    type Input = KeyboardViewMsg;
    type Output = KeyboardViewOutput;
    type CommandOutput = ();
    type Root = GtkBox;
    type Widgets = KeyboardViewWidgets;

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
        let model = KeyboardView {
            config: init.config,
            infos: init.infos,
            mode: init.mode,
            metrics: init.metrics,
        };

        let keyboard = build_keyboard(&model.infos, model.mode, &sender, &model.metrics);
        let mode_tabs = build_mode_tabs(model.mode, &sender);
        root.append(&keyboard);
        root.append(&build_hint_footer(
            &model.metrics,
            &["press key to select", "Tab switch mode", "Escape close"],
        ));
        root.append(&mode_tabs);

        let widgets = KeyboardViewWidgets {};

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        _widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        let mut rebuild = false;

        match message {
            KeyboardViewMsg::WorkspaceKeyPressed(ch) => {
                self.handle_workspace_key(ch, &sender);
            }
            KeyboardViewMsg::NextMode => {
                self.mode = self.mode.next();
                rebuild = true;
                sender
                    .output(KeyboardViewOutput::ModeChanged(self.mode))
                    .ok();
            }
            KeyboardViewMsg::PrevMode => {
                self.mode = self.mode.prev();
                rebuild = true;
                sender
                    .output(KeyboardViewOutput::ModeChanged(self.mode))
                    .ok();
            }
            KeyboardViewMsg::SetMode(mode) => {
                self.mode = mode;
                rebuild = true;
                sender
                    .output(KeyboardViewOutput::ModeChanged(self.mode))
                    .ok();
            }
        }

        if rebuild {
            while let Some(child) = root.first_child() {
                root.remove(&child);
            }

            let keyboard = build_keyboard(&self.infos, self.mode, &sender, &self.metrics);
            let mode_tabs = build_mode_tabs(self.mode, &sender);
            root.append(&keyboard);
            root.append(&build_hint_footer(
                &self.metrics,
                &["press key to select", "Tab switch mode", "Escape close"],
            ));
            root.append(&mode_tabs);
        }
    }
}

impl KeyboardView {
    fn handle_workspace_key(&self, ch: char, sender: &ComponentSender<Self>) {
        match self.mode {
            Mode::Normal => {
                let is_uncreated = self.infos.get(&ch).is_none_or(|info| info.is_uncreated);
                if is_uncreated && self.config.should_show_templates(ch) {
                    sender.output(KeyboardViewOutput::ShowTemplates(ch)).ok();
                    return;
                }
                let programs = self.config.programs_for(ch).to_vec();
                sender
                    .output(KeyboardViewOutput::ExecuteSwitch { ch, programs })
                    .ok();
            }
            Mode::Delete => {
                sender.output(KeyboardViewOutput::ExecuteDelete(ch)).ok();
            }
            Mode::MoveWindow => {
                sender.output(KeyboardViewOutput::ExecuteMove(ch)).ok();
            }
        }
    }
}

pub fn attach_key_handler(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    sender: &relm4::Sender<KeyboardViewMsg>,
) {
    let close_keybinds = config.close_keybinds.clone();
    let key_sender = sender.clone();
    let key_controller = new_key_controller();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        // Close keybinds are handled by the overlay's close handler
        if super::helpers::matches_close_keybind(key, modifier, &close_keybinds) {
            return Propagation::Proceed;
        }

        if key == gdk4::Key::Tab {
            key_sender.send(KeyboardViewMsg::NextMode).ok();
            return Propagation::Stop;
        }
        if key == gdk4::Key::ISO_Left_Tab {
            key_sender.send(KeyboardViewMsg::PrevMode).ok();
            return Propagation::Stop;
        }

        if let Some(ch) = key.to_unicode() {
            let ch = ch.to_ascii_lowercase();
            if crate::config::is_workspace_char(ch) && (modifier & ACTION_MODS).is_empty() {
                key_sender
                    .send(KeyboardViewMsg::WorkspaceKeyPressed(ch))
                    .ok();
                return Propagation::Stop;
            }
        }

        Propagation::Proceed
    });
    window.add_controller(key_controller);
}

fn build_key_widget(
    info: &DynWorkspaceInfo,
    mode: Mode,
    sender: &ComponentSender<KeyboardView>,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let mut classes = vec!["keyboard-key"];

    if info.is_focused || info.is_active {
        classes.push("active");
    }
    if info.is_urgent {
        classes.push("urgent");
    }

    let is_disabled = match mode {
        Mode::MoveWindow => info.is_uncreated || info.is_focused,
        Mode::Delete | Mode::Normal => info.is_uncreated,
    };
    if is_disabled {
        classes.push("disabled");
    }

    let key_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(classes)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    key_box.set_size_request(metrics.key_size, metrics.key_size);

    let inner = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .vexpand(true)
        .valign(Align::Center)
        .halign(Align::Center)
        .build();

    let char_label = Label::builder()
        .label(display_key_char(info.char_id))
        .css_classes(["key-char"])
        .build();
    inner.append(&char_label);

    if let Some(ref name) = info.name {
        let name_label = Label::builder()
            .label(name)
            .css_classes(["key-name"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(8)
            .build();
        inner.append(&name_label);
    }

    let apps_text = if !info.app_names.is_empty() {
        info.app_names.join(", ")
    } else if info.is_uncreated && !info.configured_programs.is_empty() {
        info.configured_programs.join(", ")
    } else {
        String::new()
    };
    if !apps_text.is_empty() {
        let apps_label = Label::builder()
            .label(&apps_text)
            .css_classes(["key-apps"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(10)
            .build();
        inner.append(&apps_label);
    }

    if let Some(ref text) = info.status_text() {
        let status_label = Label::builder()
            .label(text.as_str())
            .css_classes(["key-status"])
            .build();
        inner.append(&status_label);
    }

    key_box.append(&inner);

    // Click handler
    let ch = info.char_id;
    let click_sender = sender.clone();
    let click = GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        click_sender.input(KeyboardViewMsg::WorkspaceKeyPressed(ch));
    });
    key_box.add_controller(click);

    key_box
}

fn build_keyboard(
    infos: &HashMap<char, DynWorkspaceInfo>,
    mode: Mode,
    sender: &ComponentSender<KeyboardView>,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let keyboard = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(metrics.key_gap)
        .css_classes(["keyboard"])
        .halign(Align::Center)
        .build();

    for (row_idx, row) in metrics.layout.rows.iter().enumerate() {
        let row_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(metrics.key_gap)
            .css_classes(["keyboard-row"])
            .margin_start(metrics.row_margin(row_idx))
            .build();

        for &ch in *row {
            if let Some(info) = infos.get(&ch) {
                row_box.append(&build_key_widget(info, mode, sender, metrics));
            }
        }

        keyboard.append(&row_box);
    }

    keyboard
}

fn build_mode_tabs(current_mode: Mode, sender: &ComponentSender<KeyboardView>) -> GtkBox {
    let mode_tabs = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(0)
        .css_classes(["mode-tabs"])
        .halign(Align::Center)
        .build();
    for m in Mode::all() {
        let mut classes = vec!["mode-tab", m.css_class()];
        if m == current_mode {
            classes.push("active");
        }
        let tab_label = Label::builder()
            .label(m.display_name())
            .css_classes(classes)
            .build();

        let tab_sender = sender.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            tab_sender.input(KeyboardViewMsg::SetMode(m));
        });
        tab_label.add_controller(click);

        mode_tabs.append(&tab_label);
    }
    mode_tabs
}
