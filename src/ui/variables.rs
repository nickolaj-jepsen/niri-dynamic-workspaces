use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Entry, EventControllerKey, Label, Orientation, PolicyType, ScrolledWindow,
};

use crate::actions::HookInfo;
use crate::config::{Select, VariableType};

use super::metrics::KeyboardMetrics;
use super::picker::{show_template_picker, TemplateOption};
use super::{
    attach_close_on_backdrop_click, build_hint_footer, create_error_revealer,
    format_workspace_display, matches_close_keybind, new_key_controller, remove_app_controllers,
    switch_and_close, wrap_in_backdrop, wrap_index, ActionContext,
};

/// Filter `options` by fuzzy-matching against `query`, returning indices sorted
/// by match score (best first). An empty query returns all indices in order.
fn fuzzy_filter(query: &str, options: &[String], matcher: &mut Matcher) -> Vec<usize> {
    if query.is_empty() {
        return (0..options.len()).collect();
    }

    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = options
        .iter()
        .enumerate()
        .filter_map(|(i, opt)| {
            let haystack = Utf32Str::new(opt, &mut buf);
            pattern.score(haystack, matcher).map(|s| (i, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[derive(Clone)]
struct FuzzySelect {
    entry: Entry,
    selected: Rc<Cell<usize>>,
    filtered: Rc<RefCell<Vec<usize>>>,
    options: Vec<String>,
}

impl FuzzySelect {
    fn value(&self) -> String {
        let indices = self.filtered.borrow();
        let idx = self.selected.get();
        indices
            .get(idx)
            .and_then(|&i| self.options.get(i))
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone)]
enum VariableWidget {
    Text(Entry),
    Enum(FuzzySelect),
}

impl VariableWidget {
    fn value(&self) -> String {
        match self {
            Self::Text(entry) => entry.text().to_string(),
            Self::Enum(fuzzy) => fuzzy.value(),
        }
    }

    fn grab_focus(&self) {
        match self {
            Self::Text(entry) => {
                entry.grab_focus();
            }
            Self::Enum(fuzzy) => {
                fuzzy.entry.grab_focus();
            }
        }
    }
}

fn update_fuzzy_selection(labels: &[Label], filtered: &[usize], old_idx: usize, new_idx: usize) {
    if let Some(&old) = filtered.get(old_idx) {
        labels[old].remove_css_class("selected");
    }
    if let Some(&new) = filtered.get(new_idx) {
        labels[new].add_css_class("selected");
    }
}

/// Run a shell command and return each non-empty stdout line as a `String`.
///
/// Returns an empty `Vec` if the command fails or produces no output.
fn run_options_command(cmd: &str) -> Vec<String> {
    match std::process::Command::new("sh").args(["-c", cmd]).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        Ok(output) => {
            eprintln!(
                "warning: enum command failed (exit {}): {cmd}",
                output.status.code().unwrap_or(-1)
            );
            Vec::new()
        }
        Err(e) => {
            eprintln!("warning: could not run enum command '{cmd}': {e}");
            Vec::new()
        }
    }
}

/// Expand a leading `~/` in a path to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{rest}", home.display());
        }
    }
    path.to_string()
}

/// Recursively collect child directories up to `remaining` levels deep.
///
/// Skips hidden entries (names starting with `.`). Only directories are
/// included. Results are pushed as absolute paths.
fn collect_children(current: &std::path::Path, remaining: u32, results: &mut Vec<String>) {
    if remaining == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    let mut child_dirs: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| !n.starts_with('.')))
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|e| e.path())
        .collect();
    child_dirs.sort();
    for child in &child_dirs {
        results.push(child.to_string_lossy().into_owned());
        if remaining > 1 {
            collect_children(child, remaining - 1, results);
        }
    }
}

/// Scan directories for child directories up to a given depth.
///
/// Expands `~/` prefixes, skips missing directories, and returns a sorted
/// deduplicated list of absolute directory paths.
fn scan_dir_options(dirs: &[String], depth: u32) -> Vec<String> {
    let mut results = Vec::new();
    for dir in dirs {
        let expanded = expand_tilde(dir);
        let root = std::path::Path::new(&expanded);
        if root.is_dir() {
            collect_children(root, depth, &mut results);
        }
    }
    results.sort();
    results.dedup();
    results
}

/// Resolve the options for a select variable from its source.
fn resolve_select_options(source: &Select) -> Vec<String> {
    match source {
        Select::Options(opts) => opts.clone(),
        Select::Command(cmd) => run_options_command(cmd),
        Select::Dirs { dirs, depth } => scan_dir_options(dirs, *depth),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "fuzzy select widget with filter and key handling setup"
)]
fn build_fuzzy_select(
    row: &GtkBox,
    options: &[String],
    metrics: &KeyboardMetrics,
) -> VariableWidget {
    let search_entry = Entry::builder()
        .css_classes(["variable-entry"])
        .placeholder_text("Type to filter\u{2026}")
        .build();
    row.append(&search_entry);

    let list_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["fuzzy-list"])
        .build();

    let option_labels: Vec<Label> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let mut classes = vec!["fuzzy-option"];
            if i == 0 {
                classes.push("selected");
            }
            let label = Label::builder()
                .label(opt)
                .css_classes(classes)
                .halign(Align::Start)
                .build();
            list_box.append(&label);
            label
        })
        .collect();

    let all_indices: Vec<usize> = (0..options.len()).collect();
    let filtered = Rc::new(RefCell::new(all_indices));
    let selected = Rc::new(Cell::new(0_usize));
    let labels_rc = Rc::new(option_labels);
    let matcher = Rc::new(RefCell::new(Matcher::new(MatcherConfig::DEFAULT)));

    // Filter on text change
    {
        let opts: Vec<String> = options.to_vec();
        let filt = filtered.clone();
        let sel = selected.clone();
        let labels = labels_rc.clone();
        let matcher = matcher.clone();
        let list = list_box.clone();
        search_entry.connect_changed(move |entry| {
            let query = entry.text();

            // Remove old selection highlight
            {
                let old_filt = filt.borrow();
                if let Some(&old_real) = old_filt.get(sel.get()) {
                    labels[old_real].remove_css_class("selected");
                }
            }

            let new_indices = fuzzy_filter(&query, &opts, &mut matcher.borrow_mut());

            for (i, label) in labels.iter().enumerate() {
                label.set_visible(new_indices.contains(&i));
            }

            // Reorder labels in the list to match score order (best first)
            for (pos, &i) in new_indices.iter().enumerate() {
                if pos == 0 {
                    labels[i].insert_after(&list, None::<&Label>);
                } else {
                    labels[i].insert_after(&list, Some(&labels[new_indices[pos - 1]]));
                }
            }

            sel.set(0);
            if let Some(&first_idx) = new_indices.first() {
                labels[first_idx].add_css_class("selected");
            }

            *filt.borrow_mut() = new_indices;
        });
    }

    // Handle Up/Down keys on the search entry
    {
        let filt = filtered.clone();
        let sel = selected.clone();
        let labels = labels_rc.clone();
        let key_ctrl = EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            let indices = filt.borrow();
            if indices.is_empty() {
                return Propagation::Proceed;
            }

            let is_up = key == gdk4::Key::Up || key == gdk4::Key::KP_Up;
            let is_down = key == gdk4::Key::Down || key == gdk4::Key::KP_Down;
            if !is_up && !is_down {
                return Propagation::Proceed;
            }
            let current = sel.get();
            let new_idx = wrap_index(current, indices.len(), is_down);

            update_fuzzy_selection(&labels, &indices, current, new_idx);
            sel.set(new_idx);

            Propagation::Stop
        });
        search_entry.add_controller(key_ctrl);
    }

    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .max_content_height(metrics.key_size * 3)
        .propagate_natural_height(true)
        .child(&list_box)
        .build();
    row.append(&scrolled);

    VariableWidget::Enum(FuzzySelect {
        entry: search_entry,
        selected,
        filtered,
        options: options.to_vec(),
    })
}

pub(super) fn show_variable_input(
    option: &TemplateOption,
    ch: char,
    ctx: &ActionContext,
    template_name: Option<String>,
) {
    let window = &ctx.window;
    remove_app_controllers(window);

    let config = &ctx.config;
    let metrics = KeyboardMetrics::from_monitor_width(ctx.monitor_width, config.layout);

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["popup-container", "template-picker"])
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    // Title — match template picker style: "Template → KEY (Name)"
    let title = Label::builder()
        .label(format!(
            "{} \u{2192} {}",
            option.name,
            format_workspace_display(ch, config)
        ))
        .css_classes(["variable-title"])
        .build();
    container.append(&title);

    // Error revealer
    let (error_label, error_revealer) = create_error_revealer();

    // Variable form
    let form = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(metrics.key_gap / 2)
        .css_classes(["variable-form"])
        .build();

    let widgets: Vec<VariableWidget> = option
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

            let widget = match &var.var_type {
                VariableType::Text => {
                    let entry = Entry::builder()
                        .css_classes(["variable-entry"])
                        .placeholder_text(&var.name)
                        .build();
                    row.append(&entry);
                    VariableWidget::Text(entry)
                }
                VariableType::Select(source) => {
                    let resolved = resolve_select_options(source);
                    if resolved.is_empty() {
                        let entry = Entry::builder()
                            .css_classes(["variable-entry"])
                            .placeholder_text(&var.name)
                            .build();
                        row.append(&entry);
                        VariableWidget::Text(entry)
                    } else {
                        build_fuzzy_select(&row, &resolved, &metrics)
                    }
                }
            };

            form.append(&row);
            widget
        })
        .collect();
    container.append(&form);

    container.append(&error_revealer);

    container.append(&build_hint_footer(
        &metrics,
        &["Enter create", "\u{2191}\u{2193} navigate", "Escape back"],
    ));

    wrap_in_backdrop(window, &container);

    // Focus the first widget
    if let Some(first) = widgets.first() {
        first.grab_focus();
    }

    let var_ctx = ActionContext {
        error_label,
        error_revealer,
        ..ctx.clone()
    };

    attach_variable_input_key_handler(&var_ctx, ch, &widgets, option, template_name);
    attach_close_on_backdrop_click(window, &container);
}

fn attach_variable_input_key_handler(
    ctx: &ActionContext,
    ch: char,
    widgets: &[VariableWidget],
    option: &TemplateOption,
    template_name: Option<String>,
) {
    let key_ctx = ctx.clone();
    let close_keybinds = ctx.config.close_keybinds.clone();
    let prefix = ctx.config.workspace_prefix.clone();
    let widgets: Vec<VariableWidget> = widgets.to_vec();
    let var_names: Vec<String> = option.variables.iter().map(|v| v.name.clone()).collect();
    let programs = option.programs.clone();
    let template_title = option.title.clone();
    let template_variables = option.variables.clone();

    let key_controller = new_key_controller();
    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        // Close keybinds / Escape → go back to template picker
        if matches_close_keybind(key, modifier, &close_keybinds) {
            let ctx_clone = key_ctx.clone();
            glib::idle_add_local_once(move || {
                show_template_picker(ch, &ctx_clone);
            });
            return Propagation::Stop;
        }

        // Enter → collect values and create workspace
        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            let mut values = HashMap::new();
            for (name, widget) in var_names.iter().zip(widgets.iter()) {
                values.insert(name.clone(), widget.value());
            }
            let substituted = crate::config::substitute_variables_quoted(&programs, &values);
            let title = crate::config::resolve_workspace_title(
                template_title.as_deref(),
                &template_variables,
                &values,
            );
            let full_name = crate::config::workspace_name_with_title(&prefix, ch, title.as_deref());
            let hook_info = HookInfo {
                template_name: template_name.clone(),
                variables: values,
            };
            switch_and_close(&full_name, ch, &substituted, &key_ctx, &hook_info);
            return Propagation::Stop;
        }

        // Let GTK handle Tab, text input, etc.
        Propagation::Proceed
    });
    ctx.window.add_controller(key_controller);
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fuzzy_filter ---

    fn test_matcher() -> Matcher {
        Matcher::new(MatcherConfig::DEFAULT)
    }

    #[test]
    fn fuzzy_filter_empty_query() {
        let opts = vec!["a".into(), "b".into(), "c".into()];
        let result = fuzzy_filter("", &opts, &mut test_matcher());
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn fuzzy_filter_exact_match_first() {
        let opts: Vec<String> = vec!["something_main".into(), "main".into(), "xmyaziznw".into()];
        let result = fuzzy_filter("main", &opts, &mut test_matcher());
        // "main" (exact/short) should score highest over substring matches
        assert_eq!(result[0], 1);
        assert!(result.len() >= 2);
    }

    #[test]
    fn fuzzy_filter_no_match() {
        let opts: Vec<String> = vec!["main".into(), "develop".into()];
        let result = fuzzy_filter("xyz", &opts, &mut test_matcher());
        assert!(result.is_empty());
    }

    #[test]
    fn fuzzy_filter_ranks_by_relevance() {
        let opts: Vec<String> = vec!["administrator".into(), "dev".into(), "develop".into()];
        let result = fuzzy_filter("dev", &opts, &mut test_matcher());
        // "dev" (exact) should rank above "develop" (prefix)
        assert_eq!(result[0], 1);
        assert!(result.contains(&2));
    }

    // --- run_options_command ---

    #[test]
    fn run_options_command_basic() {
        let result = run_options_command("printf 'a\nb\nc'");
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn run_options_command_trims_and_filters() {
        let result = run_options_command("printf '  a \n\n  b  \n\n'");
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn run_options_command_failure() {
        let result = run_options_command("nonexistent_command_12345");
        assert!(result.is_empty());
    }

    // --- resolve_select_options ---

    #[test]
    fn resolve_select_options_static() {
        let source = Select::Options(vec!["a".to_string(), "b".to_string()]);
        let result = resolve_select_options(&source);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn resolve_select_options_command_succeeds() {
        let source = Select::Command("printf 'x\ny'".to_string());
        let result = resolve_select_options(&source);
        assert_eq!(result, vec!["x", "y"]);
    }

    #[test]
    fn resolve_select_options_command_fails() {
        let source = Select::Command("nonexistent_cmd_12345".to_string());
        let result = resolve_select_options(&source);
        assert!(result.is_empty());
    }

    // --- expand_tilde ---

    #[test]
    fn expand_tilde_with_home() {
        let result = expand_tilde("~/dev");
        assert!(!result.starts_with('~'));
        assert!(result.ends_with("/dev"));
    }

    #[test]
    fn expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/tmp/foo"), "/tmp/foo");
    }

    #[test]
    fn expand_tilde_only_tilde_slash() {
        let result = expand_tilde("~/");
        assert!(!result.starts_with('~'));
    }

    // --- scan_dir_options ---

    /// RAII wrapper for a temporary directory that cleans up on drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(name);
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path_str(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
        fn mkdir(&self, sub: &str) {
            std::fs::create_dir_all(self.0.join(sub)).unwrap();
        }
        fn touch(&self, sub: &str) {
            std::fs::write(self.0.join(sub), "").unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_dir_options_missing_dir() {
        let result = scan_dir_options(&["/nonexistent_dir_12345".to_string()], 1);
        assert!(result.is_empty());
    }

    #[test]
    fn scan_dir_options_basic() {
        let tmp = TempDir::new("ndw_test_scan_basic");
        tmp.mkdir("alpha");
        tmp.mkdir("beta");
        tmp.mkdir(".hidden");
        tmp.touch("file.txt");

        let result = scan_dir_options(&[tmp.path_str()], 1);
        let base = tmp.path_str();
        assert_eq!(
            result,
            vec![format!("{base}/alpha"), format!("{base}/beta")]
        );
    }

    #[test]
    fn scan_dir_options_depth_2() {
        let tmp = TempDir::new("ndw_test_scan_depth2");
        tmp.mkdir("a/child");
        tmp.mkdir("b");

        let result = scan_dir_options(&[tmp.path_str()], 2);
        let base = tmp.path_str();
        assert_eq!(
            result,
            vec![
                format!("{base}/a"),
                format!("{base}/a/child"),
                format!("{base}/b"),
            ]
        );
    }

    #[test]
    fn scan_dir_options_multiple_dirs() {
        let tmp1 = TempDir::new("ndw_test_multi1");
        let tmp2 = TempDir::new("ndw_test_multi2");
        tmp1.mkdir("shared");
        tmp1.mkdir("only1");
        tmp2.mkdir("shared");
        tmp2.mkdir("only2");

        let result = scan_dir_options(&[tmp1.path_str(), tmp2.path_str()], 1);
        let b1 = tmp1.path_str();
        let b2 = tmp2.path_str();
        assert_eq!(
            result,
            vec![
                format!("{b1}/only1"),
                format!("{b1}/shared"),
                format!("{b2}/only2"),
                format!("{b2}/shared"),
            ]
        );
    }
}
