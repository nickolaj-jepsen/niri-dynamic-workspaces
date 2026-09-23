use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use glib::Propagation;
use gtk4::gio;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Entry, EventControllerKey, GestureClick, Label, Orientation, PolicyType,
    ScrolledWindow,
};

use crate::actions::HookInfo;
use crate::config::{Select, VariableType};

use super::metrics::{apply_scaled_css, KeyboardMetrics};
use super::picker::{show_template_picker, TemplateOption};
use super::{
    attach_close_on_backdrop_click, build_hint_footer, create_error_revealer,
    format_workspace_display, matches_close_keybind, new_key_controller, remove_app_controllers,
    scroll_to_child, show_error, switch_and_close, wrap_in_backdrop, wrap_index, ActionContext,
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
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// Rows rendered by a fuzzy select; further matches are reached by typing.
const MAX_VISIBLE_OPTIONS: usize = 50;

/// Longer options are ellipsized, so a deep path cannot push the form off-screen.
const FUZZY_OPTION_MAX_CHARS: i32 = 60;

/// What Enter does with typed text that matches no option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unmatched {
    /// A closed list: refuse to submit.
    Reject,
    /// The list only aids discovery: submit the typed text.
    UseText,
    /// Submit the typed path if it is an existing absolute directory.
    UseDir,
}

impl Unmatched {
    fn for_source(source: &Select) -> Self {
        match source {
            Select::Options(_) => Self::Reject,
            Select::Command(_) => Self::UseText,
            Select::Dirs { .. } => Self::UseDir,
        }
    }
}

/// The value Enter submits: the highlighted option `best`, else `typed` as
/// `unmatched` allows. Errors describe why nothing can be submitted.
fn select_value(best: Option<&str>, typed: &str, unmatched: Unmatched) -> Result<String, String> {
    if let Some(best) = best {
        return Ok(best.to_owned());
    }
    let text = typed.trim();
    match unmatched {
        Unmatched::UseText if !text.is_empty() => Ok(text.to_owned()),
        Unmatched::UseDir => {
            let path = crate::config::expand_tilde(text);
            let dir = std::path::Path::new(&path);
            // A relative path would resolve against the daemon's cwd.
            if dir.is_absolute() && dir.is_dir() {
                Ok(path)
            } else {
                Err(format!("no directory at \"{text}\""))
            }
        }
        _ => Err(format!("no option matches \"{text}\"")),
    }
}

#[derive(Clone)]
struct FuzzySelect {
    entry: Entry,
    /// Index into `filtered`; always within the rendered rows.
    selected: Rc<Cell<usize>>,
    /// Indices into `options` of all current matches, best first.
    filtered: Rc<RefCell<Vec<usize>>>,
    options: Rc<Vec<String>>,
    unmatched: Unmatched,
}

impl FuzzySelect {
    fn value(&self) -> Result<String, String> {
        let best = self
            .filtered
            .borrow()
            .get(self.selected.get())
            .and_then(|&i| self.options.get(i))
            .cloned();
        select_value(best.as_deref(), &self.entry.text(), self.unmatched)
    }
}

#[derive(Clone)]
enum VariableWidget {
    Text(Entry),
    Enum(FuzzySelect),
    /// Placeholder while a command/dir source resolves on a worker thread.
    Loading(Entry),
}

impl VariableWidget {
    /// The value to submit, or why there is none. Empty text is a valid value.
    fn value(&self) -> Result<String, String> {
        match self {
            Self::Text(entry) => Ok(entry.text().to_string()),
            Self::Enum(fuzzy) => fuzzy.value(),
            Self::Loading(_) => Ok(String::new()),
        }
    }

    fn grab_focus(&self) {
        match self {
            Self::Text(entry) | Self::Loading(entry) => {
                entry.grab_focus();
            }
            Self::Enum(fuzzy) => {
                fuzzy.entry.grab_focus();
            }
        }
    }
}

/// Text for the row under the match list: what Enter does when nothing
/// matches, or how many matches are hidden.
fn fuzzy_hint(match_count: usize, unmatched: Unmatched) -> Option<String> {
    if match_count == 0 {
        return Some(
            match unmatched {
                Unmatched::Reject => "no match",
                Unmatched::UseText => "no match, Enter uses the typed text",
                Unmatched::UseDir => "no match, Enter uses the typed path if it is a directory",
            }
            .to_owned(),
        );
    }
    let hidden = match_count
        .checked_sub(MAX_VISIBLE_OPTIONS)
        .filter(|&n| n > 0)?;
    Some(format!("\u{2026} and {hidden} more, keep typing to narrow"))
}

/// Move the highlight to rendered row `new_idx`.
fn select_row(rows: &[Label], selected: &Cell<usize>, new_idx: usize) {
    rows[selected.get()].remove_css_class("selected");
    rows[new_idx].add_css_class("selected");
    selected.set(new_idx);
}

/// Highlight the row of `list_box` that a click lands on.
///
/// One gesture on the list: a per-row one capturing `rows` would keep every
/// row alive through a reference cycle. For the same reason the closure must
/// not capture an ancestor of `list_box`.
fn select_clicked_rows(
    list_box: &GtkBox,
    rows: Rc<Vec<Label>>,
    selected: Rc<Cell<usize>>,
    filtered: Rc<RefCell<Vec<usize>>>,
) {
    let click = GestureClick::new();
    click.connect_released(move |gesture, _, _, y| {
        let Some(list) = gesture.widget() else {
            return;
        };
        // By row band rather than pick(): the labels are only as wide as their text.
        let slot = rows.iter().take(filtered.borrow().len()).position(|row| {
            row.compute_bounds(&list).is_some_and(|bounds| {
                let top = f64::from(bounds.y());
                (top..top + f64::from(bounds.height())).contains(&y)
            })
        });
        if let Some(slot) = slot {
            select_row(&rows, &selected, slot);
        }
    });
    list_box.add_controller(click);
}

/// Show the best matches in the fixed row pool and highlight `selected`.
fn render_fuzzy_rows(
    rows: &[Label],
    more_label: &Label,
    options: &[String],
    filtered: &[usize],
    selected: usize,
    unmatched: Unmatched,
) {
    for (slot, row) in rows.iter().enumerate() {
        match filtered.get(slot) {
            Some(&i) => {
                row.set_label(&options[i]);
                row.set_visible(true);
            }
            None => row.set_visible(false),
        }
        if slot == selected {
            row.add_css_class("selected");
        } else {
            row.remove_css_class("selected");
        }
    }
    let hint = fuzzy_hint(filtered.len(), unmatched);
    more_label.set_visible(hint.is_some());
    more_label.set_label(hint.as_deref().unwrap_or_default());
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

/// Recursively collect child directories up to `remaining` levels deep.
///
/// Skips hidden entries (names starting with `.`). Only directories are
/// included; a symlink to a directory counts and is listed under its link
/// path. Results are pushed as absolute paths.
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
        // file_type() does not follow links; is_dir() does, so only links pay a stat.
        .filter(|e| {
            e.file_type()
                .is_ok_and(|ft| ft.is_dir() || (ft.is_symlink() && e.path().is_dir()))
        })
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
        let expanded = crate::config::expand_tilde(dir);
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

/// Build the widget for a resolved option list inside `row` (fuzzy select,
/// or a free-text entry when no options were produced).
fn build_resolved_select(
    row: &GtkBox,
    options: &[String],
    var_name: &str,
    metrics: &KeyboardMetrics,
    unmatched: Unmatched,
) -> VariableWidget {
    if options.is_empty() {
        let entry = Entry::builder()
            .css_classes(["variable-entry"])
            .placeholder_text(var_name)
            .build();
        row.append(&entry);
        VariableWidget::Text(entry)
    } else {
        build_fuzzy_select(row, options, metrics, unmatched)
    }
}

/// Resolve a command/dir select source on a worker thread and swap the
/// loading placeholder for the real widget once done.
///
/// Keeps arbitrary shell commands and deep directory scans from freezing the
/// overlay while the variable form opens.
fn spawn_select_resolution(
    source: Select,
    slot: Rc<RefCell<VariableWidget>>,
    row: GtkBox,
    placeholder: Entry,
    var_name: String,
    metrics: KeyboardMetrics,
) {
    let unmatched = Unmatched::for_source(&source);
    glib::spawn_future_local(async move {
        let resolved = gio::spawn_blocking(move || resolve_select_options(&source))
            .await
            .unwrap_or_default();
        let had_focus = placeholder.has_focus();
        row.remove(&placeholder);
        let widget = build_resolved_select(&row, &resolved, &var_name, &metrics, unmatched);
        if had_focus {
            widget.grab_focus();
        }
        *slot.borrow_mut() = widget;
    });
}

fn build_fuzzy_select(
    row: &GtkBox,
    options: &[String],
    metrics: &KeyboardMetrics,
    unmatched: Unmatched,
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

    // A fixed pool of rows: large option lists (deep dir scans) would
    // otherwise cost a widget per option and a relayout per keystroke.
    let rows: Vec<Label> = (0..options.len().min(MAX_VISIBLE_OPTIONS))
        .map(|_| {
            let label = Label::builder()
                .css_classes(["fuzzy-option"])
                .halign(Align::Start)
                // Middle keeps the root and basename of deep paths.
                .ellipsize(gtk4::pango::EllipsizeMode::Middle)
                .max_width_chars(FUZZY_OPTION_MAX_CHARS)
                .build();
            list_box.append(&label);
            label
        })
        .collect();
    let more_label = Label::builder()
        .css_classes(["fuzzy-more"])
        .halign(Align::Start)
        .build();
    list_box.append(&more_label);

    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .max_content_height(metrics.key_size * 3)
        .propagate_natural_height(true)
        // Otherwise it reports the ellipsized rows' minimum as its natural width.
        .propagate_natural_width(true)
        .child(&list_box)
        .build();
    row.append(&scrolled);

    let options: Rc<Vec<String>> = Rc::new(options.to_vec());
    let filtered = Rc::new(RefCell::new((0..options.len()).collect::<Vec<usize>>()));
    let selected = Rc::new(Cell::new(0_usize));
    let rows = Rc::new(rows);
    render_fuzzy_rows(
        &rows,
        &more_label,
        &options,
        &filtered.borrow(),
        0,
        unmatched,
    );

    // Filter on text change
    {
        let options = options.clone();
        let filtered = filtered.clone();
        let selected = selected.clone();
        let rows = rows.clone();
        let scrolled = scrolled.clone();
        let matcher = RefCell::new(Matcher::new(MatcherConfig::DEFAULT));
        search_entry.connect_changed(move |entry| {
            let hits = fuzzy_filter(&entry.text(), &options, &mut matcher.borrow_mut());
            selected.set(0);
            render_fuzzy_rows(&rows, &more_label, &options, &hits, 0, unmatched);
            scrolled.vadjustment().set_value(0.0);
            *filtered.borrow_mut() = hits;
        });
    }

    // Handle Up/Down keys on the search entry
    {
        let filtered = filtered.clone();
        let selected = selected.clone();
        let rows = rows.clone();
        let key_ctrl = EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            let is_up = key == gdk4::Key::Up || key == gdk4::Key::KP_Up;
            let is_down = key == gdk4::Key::Down || key == gdk4::Key::KP_Down;
            let visible = filtered.borrow().len().min(rows.len());
            if visible == 0 || (!is_up && !is_down) {
                return Propagation::Proceed;
            }

            let new_idx = wrap_index(selected.get(), visible, is_down);
            select_row(&rows, &selected, new_idx);
            scroll_to_child(&scrolled, &rows[new_idx]);

            Propagation::Stop
        });
        search_entry.add_controller(key_ctrl);
    }

    select_clicked_rows(&list_box, rows, selected.clone(), filtered.clone());

    VariableWidget::Enum(FuzzySelect {
        entry: search_entry,
        selected,
        filtered,
        options,
        unmatched,
    })
}

/// Build one labeled variable row inside `form` and return its value slot.
///
/// Static sources build synchronously; command/dir sources can be slow, so
/// they show a placeholder and resolve off-thread (the slot is swapped in
/// place once resolution finishes).
fn build_variable_row(
    var: &crate::config::TemplateVariable,
    form: &GtkBox,
    metrics: &KeyboardMetrics,
) -> Rc<RefCell<VariableWidget>> {
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

    let mut deferred_source = None;
    let widget = match &var.var_type {
        VariableType::Text => {
            let entry = Entry::builder()
                .css_classes(["variable-entry"])
                .placeholder_text(&var.name)
                .build();
            row.append(&entry);
            VariableWidget::Text(entry)
        }
        VariableType::Select(Select::Options(opts)) => {
            build_resolved_select(&row, opts, &var.name, metrics, Unmatched::Reject)
        }
        VariableType::Select(source) => {
            let placeholder = Entry::builder()
                .css_classes(["variable-entry", "loading"])
                .placeholder_text("Loading\u{2026}")
                .editable(false)
                .build();
            row.append(&placeholder);
            deferred_source = Some(source.clone());
            VariableWidget::Loading(placeholder)
        }
    };

    form.append(&row);
    let slot = Rc::new(RefCell::new(widget));

    if let Some(source) = deferred_source {
        let placeholder = match &*slot.borrow() {
            VariableWidget::Loading(entry) => entry.clone(),
            _ => unreachable!("deferred source always pairs with a Loading widget"),
        };
        spawn_select_resolution(
            source,
            slot.clone(),
            row,
            placeholder,
            var.name.clone(),
            *metrics,
        );
    }

    slot
}

pub(super) fn show_variable_input(
    option: &TemplateOption,
    ch: char,
    ctx: &ActionContext,
    template_name: Option<String>,
) {
    let window = &ctx.window;
    ctx.session.in_subview.set(true);
    remove_app_controllers(window);

    let config = &ctx.session.config;
    let metrics = ctx.session.metrics.get();
    // The overlay may have moved monitors since the last view applied its sizes.
    apply_scaled_css(&metrics.scaled_css_variables());

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["content", "variable-prompt"])
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

    let widgets: Vec<Rc<RefCell<VariableWidget>>> = option
        .variables
        .iter()
        .map(|var| build_variable_row(var, &form, &metrics))
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
        first.borrow().grab_focus();
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
    widgets: &[Rc<RefCell<VariableWidget>>],
    option: &TemplateOption,
    template_name: Option<String>,
) {
    let key_ctx = ctx.clone();
    let close_keybinds = ctx.session.config.close_keybinds.clone();
    let prefix = ctx.session.config.workspace_prefix.clone();
    let widgets: Vec<Rc<RefCell<VariableWidget>>> = widgets.to_vec();
    let programs = option.programs.clone();
    let template_title = option.title.clone();
    let template_variables = option.variables.clone();

    let key_controller = new_key_controller(&ctx.session);
    key_controller.connect_key_pressed(move |ctrl, key, keycode, _| {
        let Some(event) = super::keys::current_key_event(ctrl) else {
            return Propagation::Proceed;
        };
        // Close keybinds / Escape → go back to template picker
        if matches_close_keybind(&event, &close_keybinds) {
            key_ctx.session.held_key.hold(keycode);
            let ctx_clone = key_ctx.clone();
            glib::idle_add_local_once(move || {
                show_template_picker(ch, &ctx_clone);
            });
            return Propagation::Stop;
        }

        // Enter → collect values and create workspace
        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            key_ctx.session.held_key.hold(keycode);
            // Ignore Enter while any select source is still resolving.
            if widgets
                .iter()
                .any(|w| matches!(&*w.borrow(), VariableWidget::Loading(_)))
            {
                return Propagation::Stop;
            }
            // Widgets were built from the same variables, in the same order.
            let mut values = HashMap::new();
            for (var, widget) in template_variables.iter().zip(widgets.iter()) {
                let value = widget.borrow().value();
                match value {
                    Ok(value) => {
                        values.insert(var.name.clone(), value);
                    }
                    Err(reason) => {
                        show_error(&key_ctx, &format!("{}: {reason}", var.label));
                        widget.borrow().grab_focus();
                        return Propagation::Stop;
                    }
                }
            }
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
            switch_and_close(&full_name, ch, &programs, &key_ctx, &hook_info);
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

    #[test]
    fn fuzzy_hint_only_when_truncated_or_empty() {
        assert_eq!(
            fuzzy_hint(0, Unmatched::Reject).as_deref(),
            Some("no match")
        );
        assert_eq!(
            fuzzy_hint(0, Unmatched::UseText).as_deref(),
            Some("no match, Enter uses the typed text")
        );
        assert_eq!(
            fuzzy_hint(0, Unmatched::UseDir).as_deref(),
            Some("no match, Enter uses the typed path if it is a directory")
        );
        assert_eq!(fuzzy_hint(1, Unmatched::Reject), None);
        assert_eq!(fuzzy_hint(MAX_VISIBLE_OPTIONS, Unmatched::UseText), None);
        assert_eq!(
            fuzzy_hint(MAX_VISIBLE_OPTIONS + 3, Unmatched::Reject).as_deref(),
            Some("\u{2026} and 3 more, keep typing to narrow")
        );
    }

    // --- select_value ---

    #[test]
    fn unmatched_for_source() {
        assert_eq!(
            Unmatched::for_source(&Select::Options(vec!["a".into()])),
            Unmatched::Reject
        );
        assert_eq!(
            Unmatched::for_source(&Select::Command("ls".into())),
            Unmatched::UseText
        );
        assert_eq!(
            Unmatched::for_source(&Select::Dirs {
                dirs: vec!["~/dev".into()],
                depth: 1
            }),
            Unmatched::UseDir
        );
    }

    #[test]
    fn select_value_prefers_highlighted_match() {
        for unmatched in [Unmatched::Reject, Unmatched::UseText, Unmatched::UseDir] {
            assert_eq!(
                select_value(Some("main"), "mn", unmatched),
                Ok("main".to_string())
            );
        }
    }

    #[test]
    fn select_value_options_rejects_unmatched() {
        assert_eq!(
            select_value(None, "feature-x", Unmatched::Reject),
            Err("no option matches \"feature-x\"".to_string())
        );
    }

    #[test]
    fn select_value_command_uses_trimmed_text() {
        assert_eq!(
            select_value(None, "  feature-x ", Unmatched::UseText),
            Ok("feature-x".to_string())
        );
        assert!(select_value(None, "  ", Unmatched::UseText).is_err());
    }

    #[test]
    fn select_value_dir_accepts_existing_absolute_dir() {
        let tmp = TempDir::new("ndw_test_select_dir");
        let typed = format!(" {} ", tmp.path_str());
        assert_eq!(
            select_value(None, &typed, Unmatched::UseDir),
            Ok(tmp.path_str())
        );
    }

    #[test]
    fn select_value_dir_rejects_missing_or_relative() {
        assert_eq!(
            select_value(None, "/nonexistent_ndw_dir", Unmatched::UseDir),
            Err("no directory at \"/nonexistent_ndw_dir\"".to_string())
        );
        // Exists relative to the test's cwd, but would follow the daemon's.
        assert!(std::path::Path::new("src").is_dir());
        assert!(select_value(None, "src", Unmatched::UseDir).is_err());
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
    fn scan_dir_options_follows_symlinked_dirs() {
        let tmp = TempDir::new("ndw_test_scan_symlink");
        let target = TempDir::new("ndw_test_scan_symlink_target");
        tmp.mkdir("real");
        tmp.touch("file.txt");
        target.mkdir("inner");
        let link = |dest: &std::path::Path, name: &str| {
            std::os::unix::fs::symlink(dest, tmp.0.join(name)).unwrap();
        };
        link(&target.0, "linked");
        link(&tmp.0.join("file.txt"), "file_link");
        link(std::path::Path::new("/nonexistent_ndw_target"), "dangling");

        let result = scan_dir_options(&[tmp.path_str()], 2);
        let base = tmp.path_str();
        assert_eq!(
            result,
            vec![
                format!("{base}/linked"),
                format!("{base}/linked/inner"),
                format!("{base}/real"),
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
