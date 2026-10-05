//! A non-modal inspector for one terminal session.
//!
//! The inspector never owns the application's session state and never signals
//! numeric PIDs. Its caller supplies live snapshots, using the existing
//! spawn-time identity checks in `core::running_process_identity`.

use crate::core::RunningProcessIdentity;
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use vte4::prelude::TerminalExt;

const MAX_DETAIL_CHARS: usize = 2_048;
const MAX_VISIBLE_PROCESS_IDS: usize = 24;
const MAX_TERMINAL_DIMENSION: i64 = 1_000;

/// An owned view of the original tab, rather than whichever tab is active now.
///
/// The process must come from the tab's saved child identity, or from
/// `RunningProcessIdentity::unverified` if that identity is unavailable. Do not
/// recreate a spawn-time identity from the current numeric PID here.
#[derive(Clone, Debug)]
pub(crate) struct InspectorSnapshot {
    pub profile_names: Vec<String>,
    pub profile_name: String,
    pub title: String,
    pub working_directory: Option<String>,
    pub process: Option<RunningProcessIdentity>,
    pub pending: bool,
}

struct InspectorView {
    controls: gtk::Box,
    profile: gtk::DropDown,
    profile_names: RefCell<Vec<String>>,
    syncing_profile: Cell<bool>,
    title: gtk::Label,
    directory: gtk::Label,
    dimensions: gtk::Label,
    child_pid: gtk::Label,
    foreground: gtk::Label,
    foreground_group: gtk::Label,
    session_processes: gtk::Label,
    process_note: gtk::Label,
    columns: gtk::SpinButton,
    rows: gtk::SpinButton,
    resize: gtk::Button,
    resize_note: gtk::Label,
    status: gtk::Label,
    size_edited: Cell<bool>,
    syncing_size: Cell<bool>,
}

/// Build an inspector; the caller presents it and may keep a weak reference to
/// reuse it. Both callbacks should capture weak application state. `snapshot`
/// returns `None` once the original tab closes. `apply_profile` returns true
/// only after applying that named profile to the same original tab.
///
/// Process information is deliberately read-only: exposing a signal control
/// would require per-process identity checks and a separate confirmation flow.
pub(crate) fn build_inspector<F, P>(
    parent: &gtk::ApplicationWindow,
    terminal: &vte4::Terminal,
    snapshot: F,
    apply_profile: P,
) -> gtk::Window
where
    F: Fn() -> Option<InspectorSnapshot> + 'static,
    P: Fn(&str) -> bool + 'static,
{
    let window = gtk::Window::builder()
        .title("Inspector")
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(false)
        .default_width(480)
        .default_height(600)
        .build();
    window.set_widget_name("terminal-inspector");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_margin_top(20);
    content.set_margin_bottom(20);

    let heading = gtk::Label::new(Some("Terminal Inspector"));
    heading.set_xalign(0.0);
    heading.add_css_class("title-2");
    content.append(&heading);
    let description = note("Inspect and adjust the terminal tab that opened this window.");
    content.append(&description);

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 16);
    let details = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(12)
        .hexpand(true)
        .build();
    let profile = gtk::DropDown::from_strings(&[]);
    profile.set_widget_name("inspector-profile");
    profile.set_hexpand(true);
    profile.set_enable_search(true);
    attach_row(&details, 0, "Profile", &profile);
    let title = detail_label("inspector-title");
    let directory = detail_label("inspector-directory");
    let dimensions = detail_label("inspector-dimensions");
    attach_row(&details, 1, "Title", &title);
    attach_row(&details, 2, "Directory", &directory);
    attach_row(&details, 3, "Current Size", &dimensions);
    controls.append(&details);

    let size_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let columns = dimension_spin("inspector-columns");
    let rows = dimension_spin("inspector-rows");
    let columns_label = gtk::Label::new(Some("Columns"));
    columns_label.set_mnemonic_widget(Some(&columns));
    let rows_label = gtk::Label::new(Some("Rows"));
    rows_label.set_mnemonic_widget(Some(&rows));
    let resize = gtk::Button::with_label("Resize");
    resize.set_widget_name("inspector-resize");
    size_row.append(&columns_label);
    size_row.append(&columns);
    size_row.append(&rows_label);
    size_row.append(&rows);
    size_row.append(&resize);
    controls.append(&size_row);
    let resize_note = note("Resizing changes this window only. The desktop may limit its size.");
    controls.append(&resize_note);
    controls.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let processes_heading = gtk::Label::new(Some("Processes"));
    processes_heading.set_xalign(0.0);
    processes_heading.add_css_class("heading");
    controls.append(&processes_heading);
    let processes = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(12)
        .hexpand(true)
        .build();
    let child_pid = detail_label("inspector-child-pid");
    let foreground = detail_label("inspector-foreground");
    let foreground_group = detail_label("inspector-foreground-group");
    let session_processes = detail_label("inspector-session-processes");
    attach_row(&processes, 0, "Child PID", &child_pid);
    attach_row(&processes, 1, "Foreground", &foreground);
    attach_row(&processes, 2, "Process Group", &foreground_group);
    attach_row(&processes, 3, "Session PIDs", &session_processes);
    controls.append(&processes);
    let process_note = note("");
    process_note.set_widget_name("inspector-process-note");
    controls.append(&process_note);
    content.append(&controls);

    let status = note("");
    status.set_widget_name("inspector-status");
    content.append(&status);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    close.set_widget_name("inspector-close");
    let close_window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = close_window.upgrade() {
            window.close();
        }
    });
    content.append(&close);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&content)
        .build();
    window.set_child(Some(&scroll));

    let view = Rc::new(InspectorView {
        controls,
        profile,
        profile_names: RefCell::new(Vec::new()),
        syncing_profile: Cell::new(false),
        title,
        directory,
        dimensions,
        child_pid,
        foreground,
        foreground_group,
        session_processes,
        process_note,
        columns,
        rows,
        resize,
        resize_note,
        status,
        size_edited: Cell::new(false),
        syncing_size: Cell::new(false),
    });
    let snapshot = Rc::new(snapshot);
    refresh(&view, parent, terminal, snapshot());

    let profile_view = Rc::downgrade(&view);
    let profile_snapshot = snapshot.clone();
    view.profile.connect_selected_notify(move |selector| {
        let Some(view) = profile_view.upgrade() else {
            return;
        };
        if view.syncing_profile.get() {
            return;
        }
        let name = view
            .profile_names
            .borrow()
            .get(selector.selected() as usize)
            .cloned();
        let Some(name) = name else {
            return;
        };
        let Some(current) = profile_snapshot() else {
            close_session_view(&view);
            return;
        };
        if current.profile_name == name {
            return;
        }
        if apply_profile(&name) {
            view.status.set_text("Profile changed for this terminal.");
        } else {
            view.status
                .set_text("That profile is no longer available for this terminal.");
            update_profiles(&view, &current);
        }
    });

    for spin in [&view.columns, &view.rows] {
        let size_view = Rc::downgrade(&view);
        spin.connect_changed(move |_| {
            if let Some(view) = size_view.upgrade() {
                if !view.syncing_size.get() {
                    view.size_edited.set(true);
                }
            }
        });
    }
    let resize_view = Rc::downgrade(&view);
    let resize_parent = parent.downgrade();
    let resize_terminal = terminal.downgrade();
    let resize_snapshot = snapshot.clone();
    view.resize.connect_clicked(move |_| {
        let (Some(view), Some(parent), Some(terminal)) = (
            resize_view.upgrade(),
            resize_parent.upgrade(),
            resize_terminal.upgrade(),
        ) else {
            return;
        };
        if resize_snapshot().is_none() {
            close_session_view(&view);
            return;
        }
        if !terminal.is_mapped() || parent.is_maximized() || parent.is_fullscreen() {
            return;
        }
        view.columns.update();
        view.rows.update();
        let columns = i64::from(view.columns.value_as_int());
        let rows = i64::from(view.rows.value_as_int());
        let Some((width, height)) = requested_window_size(
            (parent.width(), parent.height()),
            (terminal.column_count(), terminal.row_count()),
            (terminal.char_width(), terminal.char_height()),
            (columns, rows),
        ) else {
            view.status
                .set_text("Terminal dimensions are not available yet. Try again shortly.");
            return;
        };
        terminal.set_size(columns, rows);
        parent.set_default_size(width, height);
        view.size_edited.set(false);
        view.status.set_text(&format!(
            "Requested {columns} columns × {rows} rows. Current Size shows the actual result."
        ));
    });

    // Only the timeout owns `view`; widget signal handlers hold weak references.
    // Remove the source immediately on destruction, rather than leaving closed
    // inspectors, their widgets, or session callbacks alive indefinitely.
    let refresh_parent = parent.downgrade();
    let refresh_terminal = terminal.downgrade();
    let refresh_window = window.downgrade();
    let source = glib::timeout_add_local(Duration::from_secs(1), move || {
        let (Some(parent), Some(terminal), Some(window)) = (
            refresh_parent.upgrade(),
            refresh_terminal.upgrade(),
            refresh_window.upgrade(),
        ) else {
            return glib::ControlFlow::Continue;
        };
        if window.is_visible() {
            refresh(&view, &parent, &terminal, snapshot());
        }
        glib::ControlFlow::Continue
    });
    let source = Rc::new(RefCell::new(Some(source)));
    let close_source = source.clone();
    window.connect_close_request(move |_| {
        if let Some(source) = close_source.borrow_mut().take() {
            source.remove();
        }
        glib::Propagation::Proceed
    });
    window.connect_destroy(move |_| {
        if let Some(source) = source.borrow_mut().take() {
            source.remove();
        }
    });
    window
}

fn refresh(
    view: &InspectorView,
    parent: &gtk::ApplicationWindow,
    terminal: &vte4::Terminal,
    snapshot: Option<InspectorSnapshot>,
) {
    let Some(snapshot) = snapshot else {
        close_session_view(view);
        return;
    };
    view.controls.set_sensitive(true);
    update_profiles(view, &snapshot);
    view.title.set_text(&display_detail(&snapshot.title));
    view.directory.set_text(
        &snapshot
            .working_directory
            .as_deref()
            .map(display_detail)
            .unwrap_or_else(|| "Not reported by the shell".into()),
    );
    let columns = terminal.column_count();
    let rows = terminal.row_count();
    view.dimensions
        .set_text(&format!("{columns} columns × {rows} rows"));
    if !view.size_edited.get() {
        view.syncing_size.set(true);
        view.columns.set_value(columns as f64);
        view.rows.set_value(rows as f64);
        view.syncing_size.set(false);
    }
    let can_resize = terminal.is_mapped() && !parent.is_maximized() && !parent.is_fullscreen();
    view.resize.set_sensitive(can_resize);
    view.resize_note.set_text(if !terminal.is_mapped() {
        "Select this terminal tab to resize its window."
    } else if parent.is_maximized() || parent.is_fullscreen() {
        "Restore the terminal window from maximized or full-screen mode to resize it."
    } else {
        "Resizing changes this window only. The desktop may limit its size."
    });

    let details = process_details(snapshot.process.as_ref(), snapshot.pending);
    view.child_pid.set_text(&details.child_pid);
    view.foreground.set_text(&details.foreground);
    view.foreground_group.set_text(&details.foreground_group);
    view.session_processes.set_text(&details.session_processes);
    view.process_note.set_text(details.note);
}

fn close_session_view(view: &InspectorView) {
    view.controls.set_sensitive(false);
    view.status.set_text("This terminal tab has closed.");
    view.child_pid.set_text("Not running");
    view.foreground.set_text("Not running");
    view.foreground_group.set_text("Unavailable");
    view.session_processes.set_text("Unavailable");
}

fn update_profiles(view: &InspectorView, snapshot: &InspectorSnapshot) {
    view.syncing_profile.set(true);
    if *view.profile_names.borrow() != snapshot.profile_names {
        let names: Vec<&str> = snapshot.profile_names.iter().map(String::as_str).collect();
        view.profile.set_model(Some(&gtk::StringList::new(&names)));
        *view.profile_names.borrow_mut() = snapshot.profile_names.clone();
    }
    let selected = snapshot
        .profile_names
        .iter()
        .position(|name| name == &snapshot.profile_name)
        .map_or(gtk::INVALID_LIST_POSITION, |index| index as u32);
    if view.profile.selected() != selected {
        view.profile.set_selected(selected);
    }
    view.syncing_profile.set(false);
}

fn attach_row(grid: &gtk::Grid, row: i32, text: &str, control: &impl IsA<gtk::Widget>) {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_valign(gtk::Align::Start);
    label.set_mnemonic_widget(Some(control));
    grid.attach(&label, 0, row, 1, 1);
    grid.attach(control, 1, row, 1, 1);
}

fn detail_label(name: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_widget_name(name);
    label.set_xalign(0.0);
    label.set_selectable(true);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(44);
    label.set_hexpand(true);
    label
}

fn note(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(56);
    label.add_css_class("dim-label");
    label
}

fn dimension_spin(name: &str) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(1.0, MAX_TERMINAL_DIMENSION as f64, 1.0);
    spin.set_widget_name(name);
    spin.set_numeric(true);
    spin.set_width_chars(4);
    spin
}

fn display_detail(value: &str) -> String {
    let mut text: String = value
        .chars()
        .take(MAX_DETAIL_CHARS)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if value.chars().nth(MAX_DETAIL_CHARS).is_some() {
        text.push('…');
    }
    if text.trim().is_empty() {
        "Not reported".into()
    } else {
        text
    }
}

/// Account for existing window chrome, VTE padding, and fractional leftover
/// cells by applying a cell-size delta to the actual allocated window size.
/// GTK/compositor minimum and maximum dimensions still determine the result.
fn requested_window_size(
    window: (i32, i32),
    current: (i64, i64),
    cell: (i64, i64),
    requested: (i64, i64),
) -> Option<(i32, i32)> {
    if window.0 <= 0
        || window.1 <= 0
        || current.0 <= 0
        || current.1 <= 0
        || cell.0 <= 0
        || cell.1 <= 0
        || !(1..=MAX_TERMINAL_DIMENSION).contains(&requested.0)
        || !(1..=MAX_TERMINAL_DIMENSION).contains(&requested.1)
    {
        return None;
    }
    let width = i64::from(window.0).checked_add((requested.0 - current.0).checked_mul(cell.0)?)?;
    let height = i64::from(window.1).checked_add((requested.1 - current.1).checked_mul(cell.1)?)?;
    Some((
        i32::try_from(width.max(1)).ok()?,
        i32::try_from(height.max(1)).ok()?,
    ))
}

#[derive(Debug, Eq, PartialEq)]
struct ProcessDetails {
    child_pid: String,
    foreground: String,
    foreground_group: String,
    session_processes: String,
    note: &'static str,
}

fn process_details(process: Option<&RunningProcessIdentity>, pending: bool) -> ProcessDetails {
    let unavailable = "Unavailable";
    let pid = |value: Option<i32>| {
        value
            .filter(|pid| *pid > 0)
            .map_or_else(|| unavailable.into(), |pid| pid.to_string())
    };
    let Some(process) = process else {
        let state = if pending {
            "Starting…"
        } else {
            "Not running"
        };
        return ProcessDetails {
            child_pid: state.into(),
            foreground: state.into(),
            foreground_group: unavailable.into(),
            session_processes: unavailable.into(),
            note: if pending {
                "Waiting for the terminal's child process to start."
            } else {
                "No running child process is attached to this terminal."
            },
        };
    };
    let session_processes = process.session_processes.as_ref().map(|pids| {
        let visible: Vec<String> = pids
            .iter()
            .take(MAX_VISIBLE_PROCESS_IDS)
            .map(ToString::to_string)
            .collect();
        let mut text = visible.join(", ");
        if pids.len() > MAX_VISIBLE_PROCESS_IDS {
            text.push_str(&format!(
                " (+{} more)",
                pids.len() - MAX_VISIBLE_PROCESS_IDS
            ));
        }
        if text.is_empty() {
            "None reported".into()
        } else {
            text
        }
    });
    ProcessDetails {
        child_pid: pid(process.child_pid),
        foreground: process
            .name
            .as_deref()
            .map(display_detail)
            .unwrap_or_else(|| unavailable.into()),
        foreground_group: pid(process.foreground_pgid),
        session_processes: session_processes.unwrap_or_else(|| unavailable.into()),
        note: if process.session_processes.is_some() {
            "Live, read-only process information. Process signals are not available in this inspector."
        } else {
            "Process details cannot be verified or are outside the sandbox. The child PID may be a host-process proxy. No signals are sent."
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_preserves_window_chrome_and_padding() {
        assert_eq!(
            requested_window_size((810, 530), (80, 24), (10, 20), (100, 30)),
            Some((1010, 650))
        );
        assert_eq!(
            requested_window_size((1010, 650), (100, 30), (10, 20), (80, 24)),
            Some((810, 530))
        );
    }

    #[test]
    fn resize_rejects_unrealized_and_invalid_dimensions() {
        assert_eq!(
            requested_window_size((0, 530), (80, 24), (10, 20), (80, 24)),
            None
        );
        assert_eq!(
            requested_window_size((810, 530), (0, 24), (10, 20), (80, 24)),
            None
        );
        assert_eq!(
            requested_window_size((810, 530), (80, 24), (0, 20), (80, 24)),
            None
        );
        assert_eq!(
            requested_window_size((810, 530), (80, 24), (10, 20), (0, 24)),
            None
        );
        assert_eq!(
            requested_window_size((810, 530), (80, 24), (10, 20), (1001, 24)),
            None
        );
        assert_eq!(
            requested_window_size((810, 530), (80, 24), (i64::MAX, 20), (100, 24)),
            None
        );
    }

    #[test]
    fn process_status_distinguishes_starting_exited_and_unverified() {
        assert_eq!(process_details(None, true).child_pid, "Starting…");
        assert_eq!(process_details(None, false).child_pid, "Not running");
        let details = process_details(
            Some(&RunningProcessIdentity::unverified(glib::Pid(123))),
            false,
        );
        assert_eq!(details.child_pid, "123");
        assert_eq!(details.foreground, "Unavailable");
        assert_eq!(details.session_processes, "Unavailable");
        assert!(details.note.contains("cannot be verified"));
    }

    #[test]
    fn process_list_is_bounded_and_foreground_text_is_plain() {
        let mut process = RunningProcessIdentity::unverified(glib::Pid(123));
        process.name = Some("<shell>\n\u{1b}[31m".into());
        process.foreground_pgid = Some(456);
        process.session_processes = Some((1..=100).collect());
        let details = process_details(Some(&process), false);
        assert_eq!(details.foreground, "<shell>  [31m");
        assert_eq!(details.foreground_group, "456");
        assert!(details.session_processes.ends_with("24 (+76 more)"));
        assert!(!details.session_processes.contains(", 25"));
    }

    #[test]
    fn display_text_is_bounded_on_unicode_characters() {
        assert_eq!(display_detail("\n\u{1b}\t"), "Not reported");
        let large = "界".repeat(MAX_DETAIL_CHARS + 10);
        let text = display_detail(&large);
        assert_eq!(text.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(text.ends_with('…'));
        assert_eq!(display_detail("<b>title</b>"), "<b>title</b>");
    }
}
