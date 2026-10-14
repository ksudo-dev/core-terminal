//! Per-terminal contextual actions. Terminal output is data, never shell code.
use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::Path,
    rc::Rc,
};
use vte4::prelude::*;

type NewSession = dyn Fn(Option<String>, Option<Vec<String>>, bool);
type Popup = dyn Fn(Option<(f64, f64)>);
pub struct ContextMenuHooks {
    pub directory: Box<dyn Fn() -> Option<String>>,
    /// Directory, optional direct argv, and whether to create a window.
    pub new_session: Box<NewSession>,
    pub inspector: Box<dyn Fn()>,
    pub paste: Box<dyn Fn()>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Context {
    text: String,
    link: Option<String>,
    folder: Option<String>,
    first_row: i64,
    last_row: i64,
}

/// Only these schemes may reach a desktop handler. Never open terminal-supplied
/// file:, ssh:, or arbitrary application URLs.
pub fn supported_link(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && uri.len() <= 8192
        && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        )
}

fn bounded_text(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= 4096 && !text.contains('\0')).then_some(text)
}

fn man_topic(text: &str) -> Option<&str> {
    let text = bounded_text(text)?;
    (text.len() <= 128
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)))
    .then_some(text)
}

fn directory_from_text(text: &str, cwd: Option<&str>) -> Option<String> {
    let text = bounded_text(text)?;
    if text.chars().any(char::is_control) {
        return None;
    }
    let path = if let Some(rest) = text.strip_prefix("~/") {
        glib::home_dir().join(rest)
    } else if text == "~" {
        glib::home_dir()
    } else if text.starts_with("file:") {
        let (path, host) = glib::filename_from_uri(text).ok()?;
        if host
            .as_deref()
            .is_some_and(|host| !host.is_empty() && host != "localhost")
        {
            return None;
        }
        path
    } else {
        let path = Path::new(text);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(cwd?).join(path)
        }
    };
    let path = path.canonicalize().ok()?;
    path.is_dir().then(|| path.to_string_lossy().into_owned())
}

fn search_uri(text: &str) -> Option<String> {
    let text = bounded_text(text)?;
    Some(format!(
        "https://www.google.com/search?q={}",
        glib::uri_escape_string(text, None::<&str>, false)
    ))
}

fn menu_model(context: &Context) -> gio::Menu {
    let menu = gio::Menu::new();
    let related = gio::Menu::new();
    if context.link.is_some() {
        related.append(Some("Open URL"), Some("context.open-link"));
        related.append(Some("Copy Link Address"), Some("context.copy-link"));
    }
    if context.folder.is_some() {
        related.append(
            Some("New Window at Folder"),
            Some("context.window-at-folder"),
        );
        related.append(Some("New Tab at Folder"), Some("context.tab-at-folder"));
    }
    related.append(Some("Open man Page"), Some("context.man"));
    related.append(Some("Search man Page Index"), Some("context.apropos"));
    related.append(Some("Search with Google"), Some("context.search-web"));
    menu.append_section(None, &related);
    let clipboard = gio::Menu::new();
    clipboard.append(Some("Copy"), Some("context.copy"));
    clipboard.append(Some("Copy as HTML"), Some("context.copy-html"));
    clipboard.append(Some("Paste"), Some("context.paste"));
    clipboard.append(Some("Paste Selection"), Some("context.paste-selection"));
    clipboard.append(Some("Paste Escaped Text"), Some("context.paste-escaped"));
    clipboard.append(Some("Select All"), Some("context.select-all"));
    menu.append_section(None, &clipboard);
    let marks = gio::Menu::new();
    marks.append(Some("Mark"), Some("context.mark"));
    marks.append(Some("Mark as Bookmark"), Some("context.bookmark"));
    marks.append(Some("Unmark"), Some("context.unmark"));
    let navigation = gio::Menu::new();
    navigation.append(Some("Previous Mark"), Some("context.previous-mark"));
    navigation.append(Some("Next Mark"), Some("context.next-mark"));
    navigation.append(Some("Previous Bookmark"), Some("context.previous-bookmark"));
    navigation.append(Some("Next Bookmark"), Some("context.next-bookmark"));
    marks.append_submenu(Some("Go to Mark"), &navigation);
    menu.append_section(None, &marks);
    let tools = gio::Menu::new();
    tools.append(Some("Show Inspector"), Some("context.inspector"));
    tools.append(Some("Find…"), Some("win.search"));
    tools.append(Some("Export Text…"), Some("win.export-text"));
    tools.append(Some("Clear Scrollback"), Some("context.clear-scrollback"));
    let platform = gio::Menu::new();
    platform.append(
        Some("Look Up (requires macOS Dictionary)"),
        Some("context.unavailable"),
    );
    platform.append(
        Some("Services (requires macOS Services)"),
        Some("context.unavailable"),
    );
    tools.append_submenu(Some("macOS Actions"), &platform);
    menu.append_section(None, &tools);
    menu
}

#[derive(Default, Debug)]
struct Marks(BTreeMap<i64, bool>);
impl Marks {
    fn mark(&mut self, first: i64, last: i64, bookmark: bool) {
        // Bound work even if an enormous scrollback range is selected.
        self.0.insert(first.min(last), bookmark);
    }
    fn remove(&mut self, first: i64, last: i64) {
        self.0
            .retain(|row, _| *row < first.min(last) || *row > first.max(last));
    }
    fn contains(&self, first: i64, last: i64) -> bool {
        self.0
            .range(first.min(last)..=first.max(last))
            .next()
            .is_some()
    }
    fn next(&self, row: i64, forward: bool, bookmarks_only: bool) -> Option<i64> {
        let mut matching = self
            .0
            .iter()
            .filter(|(_, bookmark)| !bookmarks_only || **bookmark)
            .map(|(row, _)| *row)
            .filter(|candidate| {
                if forward {
                    *candidate > row
                } else {
                    *candidate < row
                }
            });
        if forward {
            matching.next()
        } else {
            matching.next_back()
        }
    }
}

#[allow(deprecated)]
fn viewport_row(terminal: &vte4::Terminal, y: f64) -> i64 {
    let top = terminal
        .vadjustment()
        .map_or(0.0, |adjustment| adjustment.value());
    let padding = terminal.style_context().padding().top() as f64;
    (top + ((y - padding).max(0.0) / terminal.char_height().max(1) as f64).floor()) as i64
}

fn cursor_scrollback_row(cursor_row: i64, upper: f64, page_size: f64) -> i64 {
    // VTE reports the cursor relative to the visible terminal rows. The
    // adjustment's final page is the live bottom of scrollback, even while a
    // reader has scrolled the viewport away from it.
    ((upper - page_size).max(0.0) as i64).saturating_add(cursor_row.max(0))
}

fn automatic_mark_row(terminal: &vte4::Terminal) -> Option<i64> {
    let adjustment = terminal.vadjustment()?;
    let (_, cursor_row) = terminal.cursor_position();
    Some(cursor_scrollback_row(
        cursor_row,
        adjustment.upper(),
        adjustment.page_size(),
    ))
}

fn action(group: &gio::SimpleActionGroup, name: &str, callback: impl Fn() + 'static) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| callback());
    group.add_action(&action);
}

fn enable(group: &gio::SimpleActionGroup, name: &str, enabled: bool) {
    if let Some(action) = group
        .lookup_action(name)
        .and_downcast::<gio::SimpleAction>()
    {
        action.set_enabled(enabled);
    }
}

#[allow(deprecated)]
pub fn install(terminal: &vte4::Terminal, hooks: ContextMenuHooks) {
    let hooks = Rc::new(hooks);
    let context = Rc::new(RefCell::new(Context::default()));
    let marks = Rc::new(RefCell::new(Marks::default()));
    let group = gio::SimpleActionGroup::new();
    terminal.insert_action_group("context", Some(&group));
    let popover = gtk::PopoverMenu::from_model(Some(&menu_model(&Context::default())));
    popover.set_widget_name("terminal-context-menu");
    popover.set_parent(terminal);
    popover.set_has_arrow(false);
    // A weak reference avoids a terminal -> callback -> terminal cycle.
    let weak_terminal = terminal.downgrade();
    let gutter = gtk::DrawingArea::new();
    gutter.set_can_target(false);
    gutter.set_hexpand(true);
    gutter.set_vexpand(true);
    if let Some(overlay) = terminal.parent().and_downcast::<gtk::Overlay>() {
        overlay.add_overlay(&gutter);
    }
    let draw_terminal = weak_terminal.clone();
    let draw_marks = marks.clone();
    gutter.set_draw_func(move |_, cr, width, _| {
        let Some(terminal) = draw_terminal.upgrade() else {
            return;
        };
        let Some(adjustment) = terminal.vadjustment() else {
            return;
        };
        let height = terminal.char_height().max(1) as f64;
        let padding = terminal.style_context().padding().top() as f64;
        for (row, bookmark) in &draw_marks.borrow().0 {
            let y = (*row as f64 - adjustment.value()) * height + padding;
            if y < -height || y > terminal.height() as f64 {
                continue;
            }
            if *bookmark {
                cr.set_source_rgba(1.0, 0.72, 0.18, 0.9);
            } else {
                cr.set_source_rgba(0.2, 0.65, 1.0, 0.8);
            }
            cr.rectangle(0.0, y, 5.0, height);
            cr.rectangle(5.0, y, (width - 5).max(0) as f64, 1.0);
            let _ = cr.fill();
        }
    });
    if let Some(adjustment) = terminal.vadjustment() {
        let redraw = gutter.downgrade();
        adjustment.connect_value_changed(move |_| {
            if let Some(redraw) = redraw.upgrade() {
                redraw.queue_draw();
            }
        });
        let trim_marks = marks.clone();
        let redraw = gutter.downgrade();
        let previous_upper = Cell::new(adjustment.upper());
        adjustment.connect_changed(move |adjustment| {
            if adjustment.upper() < previous_upper.replace(adjustment.upper()) {
                trim_marks.borrow_mut().0.clear();
            } else {
                trim_marks
                    .borrow_mut()
                    .0
                    .retain(|row, _| *row as f64 >= adjustment.lower());
            }
            if let Some(redraw) = redraw.upgrade() {
                redraw.queue_draw();
            }
        });
    }
    // VTE shell integration publishes this immediately before a shell starts
    // a command. It is unavailable for shells that do not opt in, in which
    // case manual marks continue to work unchanged.
    {
        let marks = marks.clone();
        let gutter = gutter.downgrade();
        terminal.connect_termprop_changed(Some("shell-preexec"), move |terminal, _| {
            if let Some(row) = automatic_mark_row(terminal) {
                marks.borrow_mut().mark(row, row, false);
                if let Some(gutter) = gutter.upgrade() {
                    gutter.queue_draw();
                }
            }
        });
    }
    for (name, bookmark) in [("mark", false), ("bookmark", true)] {
        let context = context.clone();
        let marks = marks.clone();
        let gutter = gutter.downgrade();
        action(&group, name, move || {
            let context = context.borrow();
            marks
                .borrow_mut()
                .mark(context.first_row, context.last_row, bookmark);
            if let Some(gutter) = gutter.upgrade() {
                gutter.queue_draw();
            }
        });
    }
    {
        let context = context.clone();
        let marks = marks.clone();
        let gutter = gutter.downgrade();
        action(&group, "unmark", move || {
            let context = context.borrow();
            marks
                .borrow_mut()
                .remove(context.first_row, context.last_row);
            if let Some(gutter) = gutter.upgrade() {
                gutter.queue_draw();
            }
        });
    }
    for (name, forward, bookmarks_only) in [
        ("previous-mark", false, false),
        ("next-mark", true, false),
        ("previous-bookmark", false, true),
        ("next-bookmark", true, true),
    ] {
        let terminal = weak_terminal.clone();
        let marks = marks.clone();
        let context = context.clone();
        action(&group, name, move || {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let Some(adjustment) = terminal.vadjustment() else {
                return;
            };
            if let Some(row) =
                marks
                    .borrow()
                    .next(context.borrow().first_row, forward, bookmarks_only)
            {
                adjustment.set_value((row as f64).clamp(
                    adjustment.lower(),
                    (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()),
                ));
            }
        });
    }
    for (name, new_window) in [("window-at-folder", true), ("tab-at-folder", false)] {
        let context = context.clone();
        let hooks = hooks.clone();
        action(&group, name, move || {
            if let Some(folder) = context
                .borrow()
                .folder
                .as_deref()
                .filter(|folder| Path::new(folder).is_dir())
            {
                (hooks.new_session)(Some(folder.into()), None, new_window);
            }
        });
    }
    for (name, command) in [("man", "man"), ("apropos", "apropos")] {
        let context = context.clone();
        let hooks = hooks.clone();
        action(&group, name, move || {
            let text = context.borrow().text.clone();
            let Some(program) = glib::find_program_in_path(command) else {
                return;
            };
            let text = if command == "man" {
                man_topic(&text)
            } else {
                bounded_text(&text)
            };
            if let Some(text) = text {
                (hooks.new_session)(
                    (hooks.directory)(),
                    Some(vec![
                        program.to_string_lossy().into_owned(),
                        "--".into(),
                        text.into(),
                    ]),
                    false,
                );
            }
        });
    }
    for name in ["open-link", "search-web"] {
        let context = context.clone();
        let terminal = weak_terminal.clone();
        action(&group, name, move || {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let context = context.borrow();
            let uri = if name == "open-link" {
                context.link.clone().filter(|link| supported_link(link))
            } else {
                search_uri(&context.text)
            };
            let Some(uri) = uri else {
                return;
            };
            let parent = terminal.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(
                parent.as_ref(),
                None::<&gio::Cancellable>,
                |result| {
                    if let Err(error) = result {
                        eprintln!("Core Terminal: could not open context URL: {error}");
                    }
                },
            );
        });
    }
    for name in ["copy", "copy-link"] {
        let terminal = weak_terminal.clone();
        let context = context.clone();
        action(&group, name, move || {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let context = context.borrow();
            let text = if name == "copy-link" {
                context.link.as_deref().unwrap_or("")
            } else {
                &context.text
            };
            if !text.is_empty() {
                terminal.display().clipboard().set_text(text);
            }
        });
    }
    {
        let terminal = weak_terminal.clone();
        action(&group, "copy-html", move || {
            if let Some(terminal) = terminal.upgrade() {
                terminal.copy_clipboard_format(vte4::Format::Html);
            }
        });
    }
    {
        let paste_hooks = hooks.clone();
        action(&group, "paste", move || (paste_hooks.paste)());
        let hooks = hooks.clone();
        action(&group, "inspector", move || (hooks.inspector)());
    }
    {
        let terminal = weak_terminal.clone();
        action(&group, "paste-selection", move || {
            if let Some(terminal) = terminal.upgrade() {
                terminal.paste_primary();
            }
        });
        let terminal = weak_terminal.clone();
        action(&group, "paste-escaped", move || {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let weak = terminal.downgrade();
            terminal.display().clipboard().read_text_async(
                None::<&gio::Cancellable>,
                move |result| {
                    if let (Some(terminal), Ok(Some(text))) = (weak.upgrade(), result) {
                        terminal.paste_text(&format!("'{}'", text.replace('\'', "'\\''")));
                    }
                },
            );
        });
        let terminal = weak_terminal.clone();
        action(&group, "select-all", move || {
            if let Some(terminal) = terminal.upgrade() {
                terminal.select_all();
            }
        });
        let terminal = weak_terminal.clone();
        let marks = marks.clone();
        let redraw = gutter.downgrade();
        action(&group, "clear-scrollback", move || {
            marks.borrow_mut().0.clear();
            if let Some(terminal) = terminal.upgrade() {
                terminal.reset(false, true);
            }
            if let Some(redraw) = redraw.upgrade() {
                redraw.queue_draw();
            }
        });
    }
    action(&group, "unavailable", || {});
    enable(&group, "unavailable", false);
    // VTE matches give safe word/URL context without changing the user's text
    // selection or copying anything to CLIPBOARD on a secondary click.
    if let Ok(regex) = vte4::Regex::for_match(r#"[^\s<>"\x27`|;]+"#, 0x00000400) {
        terminal.match_add_regex(&regex, 0);
    }
    let selection_rows = Rc::new(Cell::new(None::<(i64, i64)>));
    let observed_rows = selection_rows.clone();
    let observed_terminal = weak_terminal.clone();
    let observer = gtk::EventControllerLegacy::new();
    observer.set_propagation_phase(gtk::PropagationPhase::Capture);
    observer.connect_event(move |_, event| {
        let Some(terminal) = observed_terminal.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>() {
            if button.button() == 1 {
                if let Some((x, y)) = event.position() {
                    let y = terminal
                        .root()
                        .and_downcast::<gtk::Window>()
                        .and_then(|root| {
                            root.compute_point(
                                &terminal,
                                &gtk::graphene::Point::new(x as f32, y as f32),
                            )
                        })
                        .map_or(y, |point| point.y() as f64);
                    let row = viewport_row(&terminal, y);
                    match event.event_type() {
                        gtk::gdk::EventType::ButtonPress => observed_rows.set(Some((row, row))),
                        gtk::gdk::EventType::ButtonRelease => {
                            if let Some((first, _)) = observed_rows.get() {
                                observed_rows.set(Some((first, row)));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        glib::Propagation::Proceed
    });
    terminal.add_controller(observer);
    let popup: Rc<Popup> = Rc::new({
        let terminal = weak_terminal.clone();
        let popover = popover.clone();
        move |point| {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let selected = terminal
                .text_selected(vte4::Format::Text)
                .map(|text| text.to_string())
                .unwrap_or_default();
            let word = point
                .and_then(|(x, y)| terminal.check_match_at(x, y).0)
                .map(|text| text.to_string())
                .unwrap_or_default();
            let text = if selected.is_empty() { word } else { selected };
            let hyperlink = point
                .and_then(|(x, y)| terminal.check_hyperlink_at(x, y))
                .map(|text| text.to_string());
            let link = hyperlink
                .filter(|link| supported_link(link))
                .or_else(|| supported_link(text.trim()).then(|| text.trim().to_owned()));
            let folder = directory_from_text(&text, (hooks.directory)().as_deref());
            let row = point.map_or_else(
                || terminal.cursor_position().1,
                |(_, y)| viewport_row(&terminal, y),
            );
            let (first, last) = if terminal.has_selection() {
                selection_rows.get().unwrap_or((row, row))
            } else {
                (row, row)
            };
            *context.borrow_mut() = Context {
                text,
                link,
                folder,
                first_row: first.min(last),
                last_row: first.max(last),
            };
            let context = context.borrow();
            enable(&group, "copy", !context.text.is_empty());
            enable(&group, "copy-html", terminal.has_selection());
            enable(
                &group,
                "man",
                man_topic(&context.text).is_some() && glib::find_program_in_path("man").is_some(),
            );
            enable(
                &group,
                "apropos",
                bounded_text(&context.text).is_some()
                    && glib::find_program_in_path("apropos").is_some(),
            );
            enable(&group, "search-web", bounded_text(&context.text).is_some());
            let clipboard_text = terminal
                .display()
                .clipboard()
                .formats()
                .contains_type(String::static_type());
            enable(&group, "paste", clipboard_text);
            enable(&group, "paste-escaped", clipboard_text);
            enable(
                &group,
                "paste-selection",
                terminal
                    .display()
                    .primary_clipboard()
                    .formats()
                    .contains_type(String::static_type()),
            );
            enable(
                &group,
                "unmark",
                marks.borrow().contains(context.first_row, context.last_row),
            );
            for (name, forward, bookmarks) in [
                ("previous-mark", false, false),
                ("next-mark", true, false),
                ("previous-bookmark", false, true),
                ("next-bookmark", true, true),
            ] {
                enable(
                    &group,
                    name,
                    marks
                        .borrow()
                        .next(context.first_row, forward, bookmarks)
                        .is_some(),
                );
            }
            popover.set_menu_model(Some(&menu_model(&context)));
            let (x, y) = point.unwrap_or((
                terminal.width() as f64 / 2.0,
                terminal.height() as f64 / 2.0,
            ));
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    });
    let click = gtk::GestureClick::builder().button(3).build();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let pointer_popup = popup.clone();
    click.connect_released(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        pointer_popup(Some((x, y)));
    });
    terminal.add_controller(click);
    let keyboard = gtk::EventControllerKey::new();
    keyboard.set_propagation_phase(gtk::PropagationPhase::Capture);
    keyboard.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Menu
            || (key == gtk::gdk::Key::F10 && modifiers == gtk::gdk::ModifierType::SHIFT_MASK)
        {
            popup(None);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    terminal.add_controller(keyboard);
    terminal.connect_unrealize(move |_| popover.unparent());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_executable_links_and_control_characters() {
        for link in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ssh://host",
            "https:",
            "https://a/\n",
            "https://a/b c",
        ] {
            assert!(!supported_link(link));
        }
        for link in [
            "https://example.com",
            "HTTP://example.com",
            "mailto:a@example.com",
        ] {
            assert!(supported_link(link));
        }
    }
    #[test]
    fn man_topics_cannot_be_options_or_shell_programs() {
        for topic in [
            "-l",
            "--help",
            "$(id)",
            "ls;touch /tmp/x",
            "foo\nbar",
            "../file",
            "a b",
        ] {
            assert!(man_topic(topic).is_none());
        }
        for topic in ["ls", "git-status", "printf", "systemd.service"] {
            assert_eq!(man_topic(topic), Some(topic));
        }
    }
    #[test]
    fn web_search_encodes_text_as_one_query_value() {
        assert_eq!(
            search_uri("a&b #$(id)"),
            Some("https://www.google.com/search?q=a%26b%20%23%24%28id%29".into())
        );
        assert!(search_uri("").is_none());
        assert!(search_uri(&"x".repeat(4097)).is_none());
    }
    #[test]
    fn folders_are_real_local_directories_and_never_shell_expanded() {
        assert_eq!(directory_from_text("/tmp", None), Some("/tmp".into()));
        assert_eq!(directory_from_text(".", Some("/tmp")), Some("/tmp".into()));
        assert!(directory_from_text("$(mkdir /tmp/no)", Some("/tmp")).is_none());
        assert!(directory_from_text("file://remote/tmp", None).is_none());
        assert!(directory_from_text("/etc/passwd", None).is_none());
    }
    #[test]
    fn marks_upgrade_navigate_remove_and_keep_session_order() {
        let mut marks = Marks::default();
        marks.mark(4, 9, false);
        marks.mark(20, 20, true);
        marks.mark(4, 9, true);
        assert_eq!(marks.0.len(), 2);
        assert_eq!(marks.next(4, true, true), Some(20));
        assert_eq!(marks.next(20, false, false), Some(4));
        assert!(marks.contains(4, 9));
        marks.remove(0, 10);
        assert!(!marks.contains(4, 9));
        assert_eq!(marks.next(20, true, false), None);
    }

    #[test]
    fn automatic_marks_use_the_live_scrollback_page_not_the_viewport() {
        assert_eq!(cursor_scrollback_row(3, 400.0, 24.0), 379);
        assert_eq!(cursor_scrollback_row(-1, 12.0, 24.0), 0);
    }
    #[test]
    fn context_menu_has_platform_limits_and_conditional_targets() {
        let menu = menu_model(&Context::default());
        assert_eq!(menu.n_items(), 4);
        let related = menu.item_link(0, "section").unwrap();
        assert_eq!(related.n_items(), 3);
        let menu = menu_model(&Context {
            link: Some("https://example.com".into()),
            folder: Some("/tmp".into()),
            ..Context::default()
        });
        assert_eq!(menu.item_link(0, "section").unwrap().n_items(), 7);
    }
}
