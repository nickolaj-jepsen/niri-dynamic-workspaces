use std::collections::HashMap;
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{Align, ApplicationWindow, Box as GtkBox, GestureClick, Label, Orientation};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{Component, ComponentController, ComponentParts, ComponentSender, Controller};

use super::helpers::{
    create_error_revealer, matches_close_keybind, new_key_controller, remove_app_controllers,
    wrap_in_backdrop,
};
use super::keyboard_view::{self, KeyboardView, KeyboardViewInit, KeyboardViewOutput};
use super::metrics::{apply_scaled_css, get_monitor_width, KeyboardMetrics};
use super::mode::Mode;
use super::template_picker::{self, TemplatePicker, TemplatePickerInit, TemplatePickerOutput};
use super::types::{build_full_keyboard_info, HookInfo};
use super::variable_input::{self, VariableInput, VariableInputInit, VariableInputOutput};
use crate::config::ResolvedConfig;
use crate::niri;

pub struct OverlayInit {
    pub app: gtk4::Application,
    pub config: Rc<ResolvedConfig>,
    pub mode: Mode,
}

enum ActiveView {
    Keyboard(Controller<KeyboardView>),
    TemplatePicker(Controller<TemplatePicker>),
    VariableInput(Controller<VariableInput>),
}

pub struct AppOverlay {
    config: Rc<ResolvedConfig>,
    metrics: KeyboardMetrics,
    mode: Mode,
    active_view: ActiveView,
    // Pending state passed between views
    pending_ws_char: Option<char>,
    pending_template_name: Option<String>,
    pending_programs: Vec<String>,
    pending_var_names: Vec<String>,
    error_message: Option<String>,
}

pub struct AppWidgets {
    popup_container: GtkBox,
    error_label: Label,
    error_revealer: gtk4::Revealer,
}

pub(crate) struct TemplateSelectedMsg {
    template_name: Option<String>,
    programs: Vec<String>,
    var_names: Vec<String>,
    variables: Vec<crate::config::TemplateVariable>,
}

#[derive(Debug)]
pub enum AppMsg {
    // Forwarded from KeyboardView
    ExecuteSwitch { ch: char, programs: Vec<String> },
    ExecuteDelete(char),
    ExecuteMove(char),
    ShowTemplates(char),
    ModeChanged(Mode),
    // Forwarded from TemplatePicker
    TemplateSelected(TemplateSelectedMsg),
    // Forwarded from VariableInput
    VariablesSubmitted(HashMap<String, String>),
    // Shared
    Back,
    Close,
    ShowError(String),
}

impl std::fmt::Debug for TemplateSelectedMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateSelectedMsg")
            .field("template_name", &self.template_name)
            .finish_non_exhaustive()
    }
}

impl AppOverlay {
    fn switch_and_close(
        &self,
        ws_name: &str,
        ws_key: char,
        programs: &[String],
        hook_info: &HookInfo,
        root: &ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let result = niri::switch_workspace(ws_name, programs).map(|(created, req)| {
            if let Some(r) = req {
                std::thread::Builder::new()
                    .name("reorder".into())
                    .spawn(move || niri::reorder_workspace_columns(&r))
                    .ok();
            }
            if created {
                let hooks = crate::config::collect_create_hooks(
                    &self.config,
                    hook_info.template_name.as_deref(),
                );
                let env = crate::config::build_hook_env(
                    ws_name,
                    ws_key,
                    hook_info.template_name.as_deref(),
                    &hook_info.variables,
                );
                niri::run_hooks(&hooks, &env);
            }
        });
        if let Err(e) = result {
            sender.input(AppMsg::ShowError(format!("Failed: {e:#}")));
            return;
        }
        root.close();
    }

    fn execute_delete(&self, ch: char, root: &ApplicationWindow, sender: &ComponentSender<Self>) {
        let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);
        let result = niri::delete_workspace(&ws_name);
        if result.is_ok() {
            let env = crate::config::build_hook_env(&ws_name, ch, None, &HashMap::new());
            niri::run_hooks(&self.config.hooks.on_delete, &env);
        }
        if let Err(e) = result {
            sender.input(AppMsg::ShowError(format!("Failed: {e:#}")));
            return;
        }
        root.close();
    }

    fn execute_move(&self, ch: char, root: &ApplicationWindow, sender: &ComponentSender<Self>) {
        let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);
        if let Err(e) = niri::move_window_to_workspace(&ws_name) {
            sender.input(AppMsg::ShowError(format!("Failed: {e:#}")));
            return;
        }
        root.close();
    }

    fn transition_to_keyboard(
        &mut self,
        root: &ApplicationWindow,
        popup_container: &GtkBox,
        sender: &ComponentSender<Self>,
    ) {
        remove_app_controllers(root);
        self.remove_active_view_widget(popup_container);

        let infos = build_full_keyboard_info(&self.config);
        let controller = KeyboardView::builder()
            .launch(KeyboardViewInit {
                config: Rc::clone(&self.config),
                infos,
                mode: self.mode,
                metrics: self.metrics,
            })
            .forward(sender.input_sender(), |output| match output {
                KeyboardViewOutput::ExecuteSwitch { ch, programs } => {
                    AppMsg::ExecuteSwitch { ch, programs }
                }
                KeyboardViewOutput::ExecuteDelete(ch) => AppMsg::ExecuteDelete(ch),
                KeyboardViewOutput::ExecuteMove(ch) => AppMsg::ExecuteMove(ch),
                KeyboardViewOutput::ShowTemplates(ch) => AppMsg::ShowTemplates(ch),

                KeyboardViewOutput::ModeChanged(mode) => AppMsg::ModeChanged(mode),
            });

        popup_container.append(controller.widget());
        keyboard_view::attach_key_handler(root, &self.config, controller.sender());
        self.attach_close_key_handler(root, sender);
        self.active_view = ActiveView::Keyboard(controller);

        // Update container CSS
        popup_container.remove_css_class("template-picker");
        popup_container.remove_css_class("delete-mode");
        popup_container.remove_css_class("move-window-mode");
        if let Some(cls) = self.mode.container_css_class() {
            popup_container.add_css_class(cls);
        }
        root.set_widget_name(self.mode.widget_name());
    }

    fn transition_to_template_picker(
        &mut self,
        ch: char,
        root: &ApplicationWindow,
        popup_container: &GtkBox,
        sender: &ComponentSender<Self>,
    ) {
        remove_app_controllers(root);
        self.remove_active_view_widget(popup_container);

        self.pending_ws_char = Some(ch);

        let controller = TemplatePicker::builder()
            .launch(TemplatePickerInit {
                config: Rc::clone(&self.config),
                ws_char: ch,
                metrics: self.metrics,
            })
            .forward(sender.input_sender(), |output| match output {
                TemplatePickerOutput::Selected {
                    template_name,
                    programs,
                    var_names,
                    variables,
                } => AppMsg::TemplateSelected(TemplateSelectedMsg {
                    template_name,
                    programs,
                    var_names,
                    variables,
                }),
            });

        popup_container.append(controller.widget());
        template_picker::attach_key_handler(root, &self.config, controller.sender());
        self.attach_close_key_handler(root, sender);
        self.active_view = ActiveView::TemplatePicker(controller);

        popup_container.remove_css_class("delete-mode");
        popup_container.remove_css_class("move-window-mode");
        popup_container.add_css_class("template-picker");
    }

    fn transition_to_variable_input(
        &mut self,
        root: &ApplicationWindow,
        popup_container: &GtkBox,
        sender: &ComponentSender<Self>,
    ) {
        remove_app_controllers(root);
        self.remove_active_view_widget(popup_container);

        let ch = self.pending_ws_char.unwrap_or('?');

        // Find the matching template variables
        let variables = if let Some(ref tmpl_name) = self.pending_template_name {
            self.config
                .templates
                .iter()
                .find(|t| t.name == *tmpl_name)
                .map_or_else(Vec::new, |t| t.variables.clone())
        } else {
            Vec::new()
        };

        let controller = VariableInput::builder()
            .launch(VariableInputInit {
                config: Rc::clone(&self.config),
                ws_char: ch,
                template_name: self.pending_template_name.clone(),
                var_names: self.pending_var_names.clone(),
                variables,
                metrics: self.metrics,
            })
            .forward(sender.input_sender(), |output| match output {
                VariableInputOutput::Submitted(values) => AppMsg::VariablesSubmitted(values),
            });

        popup_container.append(controller.widget());
        variable_input::attach_key_handler(root, &self.config, controller.sender());
        self.attach_close_key_handler(root, sender);
        self.active_view = ActiveView::VariableInput(controller);

        popup_container.add_css_class("template-picker");
    }

    fn remove_active_view_widget(&self, popup_container: &GtkBox) {
        let widget = match &self.active_view {
            ActiveView::Keyboard(c) => c.widget().clone(),
            ActiveView::TemplatePicker(c) => c.widget().clone(),
            ActiveView::VariableInput(c) => c.widget().clone(),
        };
        popup_container.remove(&widget);
    }

    fn recompute_metrics(&mut self) {
        self.metrics = KeyboardMetrics::from_monitor_width(get_monitor_width(), self.config.layout);
        apply_scaled_css(&self.metrics.scaled_css_variables());
    }

    fn handle_template_selected(
        &mut self,
        msg: TemplateSelectedMsg,
        root: &ApplicationWindow,
        popup_container: &GtkBox,
        sender: &ComponentSender<Self>,
    ) {
        if msg.variables.is_empty() {
            let ch = self.pending_ws_char.unwrap_or('?');
            let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);
            let hook_info = HookInfo {
                template_name: msg.template_name,
                variables: HashMap::new(),
            };
            self.switch_and_close(&ws_name, ch, &msg.programs, &hook_info, root, sender);
        } else {
            self.pending_template_name = msg.template_name;
            self.pending_programs = msg.programs;
            self.pending_var_names = msg.var_names;
            self.recompute_metrics();
            self.transition_to_variable_input(root, popup_container, sender);
        }
    }

    fn handle_back(
        &mut self,
        root: &ApplicationWindow,
        popup_container: &GtkBox,
        sender: &ComponentSender<Self>,
    ) {
        match &self.active_view {
            ActiveView::Keyboard(_) => root.close(),
            ActiveView::TemplatePicker(_) => {
                self.pending_ws_char = None;
                self.recompute_metrics();
                self.transition_to_keyboard(root, popup_container, sender);
            }
            ActiveView::VariableInput(_) => {
                let ch = self.pending_ws_char.unwrap_or('?');
                self.pending_template_name = None;
                self.pending_programs.clear();
                self.pending_var_names.clear();
                self.recompute_metrics();
                self.transition_to_template_picker(ch, root, popup_container, sender);
            }
        }
    }

    /// Attach a window-level key handler for close/escape keybinds.
    /// This runs at capture phase and handles close/back before child handlers.
    fn attach_close_key_handler(&self, window: &ApplicationWindow, sender: &ComponentSender<Self>) {
        let close_keybinds = self.config.close_keybinds.clone();
        let is_keyboard = matches!(self.active_view, ActiveView::Keyboard(_));
        let key_sender = sender.clone();
        let key_controller = new_key_controller();
        // Use a different name so we can have both handlers
        key_controller.set_name(Some("ndw-close"));

        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            if matches_close_keybind(key, modifier, &close_keybinds) {
                if is_keyboard {
                    key_sender.input(AppMsg::Close);
                } else {
                    key_sender.input(AppMsg::Back);
                }
                return Propagation::Stop;
            }
            Propagation::Proceed
        });
        window.add_controller(key_controller);
    }
}

impl Component for AppOverlay {
    type Init = OverlayInit;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> Self::Root {
        let window = ApplicationWindow::builder().build();
        window.remove_css_class("background");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(Edge::Left, true);
        window.set_anchor(Edge::Right, true);
        window
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.set_application(Some(&init.app));
        root.set_widget_name(init.mode.widget_name());

        let metrics = KeyboardMetrics::from_monitor_width(get_monitor_width(), init.config.layout);
        apply_scaled_css(&metrics.scaled_css_variables());

        let infos = build_full_keyboard_info(&init.config);

        // Build container
        let mut container_classes = vec!["popup-container"];
        if let Some(cls) = init.mode.container_css_class() {
            container_classes.push(cls);
        }

        let popup_container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(0)
            .css_classes(container_classes)
            .halign(Align::Center)
            .valign(Align::Center)
            .build();

        let (error_label, error_revealer) = create_error_revealer();

        // Launch keyboard view as initial child
        let keyboard_controller = KeyboardView::builder()
            .launch(KeyboardViewInit {
                config: Rc::clone(&init.config),
                infos,
                mode: init.mode,
                metrics,
            })
            .forward(sender.input_sender(), |output| match output {
                KeyboardViewOutput::ExecuteSwitch { ch, programs } => {
                    AppMsg::ExecuteSwitch { ch, programs }
                }
                KeyboardViewOutput::ExecuteDelete(ch) => AppMsg::ExecuteDelete(ch),
                KeyboardViewOutput::ExecuteMove(ch) => AppMsg::ExecuteMove(ch),
                KeyboardViewOutput::ShowTemplates(ch) => AppMsg::ShowTemplates(ch),

                KeyboardViewOutput::ModeChanged(mode) => AppMsg::ModeChanged(mode),
            });

        popup_container.append(keyboard_controller.widget());

        // Wrap in backdrop
        wrap_in_backdrop(&root, &popup_container);

        // Attach close-on-backdrop-click
        {
            let close_sender = sender.clone();
            let container_ref = popup_container.clone();
            let window_ref = root.clone();
            let click = GestureClick::new();
            click.set_name(Some("ndw-backdrop"));
            click.connect_released(move |_, _, x, y| {
                let (cx, cy) = container_ref
                    .translate_coordinates(&window_ref, 0.0, 0.0)
                    .unwrap_or((0.0, 0.0));
                let cw = f64::from(container_ref.width());
                let ch = f64::from(container_ref.height());
                if x < cx || x > cx + cw || y < cy || y > cy + ch {
                    close_sender.input(AppMsg::Close);
                }
            });
            root.add_controller(click);
        }

        // Attach key handlers
        keyboard_view::attach_key_handler(&root, &init.config, keyboard_controller.sender());

        let model = AppOverlay {
            config: Rc::clone(&init.config),
            metrics,
            mode: init.mode,
            active_view: ActiveView::Keyboard(keyboard_controller),
            pending_ws_char: None,
            pending_template_name: None,
            pending_programs: Vec::new(),
            pending_var_names: Vec::new(),
            error_message: None,
        };

        // Attach close handler after model is created (need to know view type)
        model.attach_close_key_handler(&root, &sender);

        let widgets = AppWidgets {
            popup_container,
            error_label,
            error_revealer,
        };

        root.present();

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        // Clear error on any action except ShowError
        if !matches!(message, AppMsg::ShowError(_)) {
            self.error_message = None;
        }

        match message {
            AppMsg::ExecuteSwitch { ch, programs } => {
                let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);
                self.switch_and_close(&ws_name, ch, &programs, &HookInfo::default(), root, &sender);
            }
            AppMsg::ExecuteDelete(ch) => {
                self.execute_delete(ch, root, &sender);
            }
            AppMsg::ExecuteMove(ch) => {
                self.execute_move(ch, root, &sender);
            }
            AppMsg::ShowTemplates(ch) => {
                self.recompute_metrics();
                self.transition_to_template_picker(ch, root, &widgets.popup_container, &sender);
            }
            AppMsg::ModeChanged(mode) => {
                self.mode = mode;
                root.set_widget_name(mode.widget_name());
                widgets.popup_container.remove_css_class("delete-mode");
                widgets.popup_container.remove_css_class("move-window-mode");
                if let Some(cls) = mode.container_css_class() {
                    widgets.popup_container.add_css_class(cls);
                }
            }
            AppMsg::TemplateSelected(msg) => {
                self.handle_template_selected(msg, root, &widgets.popup_container, &sender);
            }
            AppMsg::VariablesSubmitted(values) => {
                let ch = self.pending_ws_char.unwrap_or('?');
                let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);
                let substituted =
                    crate::config::substitute_variables(&self.pending_programs, &values);
                let hook_info = HookInfo {
                    template_name: self.pending_template_name.clone(),
                    variables: values,
                };
                self.switch_and_close(&ws_name, ch, &substituted, &hook_info, root, &sender);
            }
            AppMsg::Back => {
                self.handle_back(root, &widgets.popup_container, &sender);
            }
            AppMsg::Close => root.close(),
            AppMsg::ShowError(msg) => {
                self.error_message = Some(msg);
            }
        }

        // Sync error display
        if let Some(ref msg) = self.error_message {
            widgets.error_label.set_label(msg);
            widgets.error_revealer.set_reveal_child(true);
        } else {
            widgets.error_revealer.set_reveal_child(false);
        }
    }
}
