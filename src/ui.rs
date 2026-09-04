//! Small GTK widgets shared by the application window and settings window.
//!
//! This module deliberately keeps application state out of widgets.  Callers
//! own the profile store and settings, then connect the returned controls to
//! their session model.

use crate::{
    core::{self, SessionId, SessionManager},
    profiles::{
        AskBeforeClosePolicy, BackgroundImageMode, CloseOnExit, CursorShape, KeyMapping,
        ProfileStore, ShellExitAction, TerminalProfile, WindowGroup, WindowGroupEntry,
    },
    settings::{Settings, CURRENT_SCHEMA_VERSION},
    shortcuts::{
        decide_shortcut, decode_key_sequence, parse_key_chord, ShortcutAction, ShortcutInput,
    },
    APPLICATION_ID,
};
use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vte4::prelude::{TerminalExt, TerminalExtManual};

const SETTINGS_PAGE_IDS: [&str; 4] = ["general", "profiles", "window-groups", "encodings"];
const PROFILE_PAGE_IDS: [&str; 6] = ["text", "window", "tab", "shell", "keyboard", "advanced"];
static ACCEPTANCE_CLOSED_SPAWN_RESOLUTIONS: AtomicUsize = AtomicUsize::new(0);

fn compatibility_profile<'a>(
    profiles: &'a ProfileStore,
    active_profile: &str,
    startup_profile: &str,
) -> &'a TerminalProfile {
    profiles
        .profile(active_profile)
        .or_else(|| profiles.profile(startup_profile))
        .unwrap_or_else(|| profiles.selected())
}

/// Run process-session escalation away from GTK's main thread. Holding the
/// application until the worker finishes keeps last-window shutdown from
/// abandoning a pending TERM/KILL sequence.
fn terminate_child_async(identity: core::ChildProcessIdentity) {
    let mut application_hold = gio::Application::default().map(|application| application.hold());
    let (completed_sender, completed_receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("core-terminal-terminate".into())
        .spawn(move || {
            core::terminate_child(identity);
            let _ = completed_sender.send(());
        });

    if let Err(error) = worker {
        application_hold.take();
        eprintln!("Core Terminal: failed to start process cleanup worker: {error}");
        return;
    }

    glib::timeout_add_local(
        Duration::from_millis(10),
        move || match completed_receiver.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                application_hold.take();
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        },
    );
}

#[cfg(test)]
fn settings_page_ids() -> &'static [&'static str; 4] {
    &SETTINGS_PAGE_IDS
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod structural_tests {
    use super::{
        compatibility_profile, resolve_new_tab_profile, resolve_window_profile,
        runtime_profile_requires_reapply, runtime_terminal_settings_changed, settings_page_ids,
        spawn_callback_action, startup_profile_after_deletion, window_group_entry_summary,
        ProfileStore, SessionManager, Settings, SpawnCallbackAction, WindowGroupEntry,
        APPLICATION_ID, PROFILE_PAGE_IDS,
    };

    #[test]
    fn settings_window_has_all_top_level_pages() {
        assert_eq!(
            settings_page_ids(),
            &["general", "profiles", "window-groups", "encodings"]
        );
    }

    #[test]
    fn profiles_window_has_all_subpages() {
        assert_eq!(PROFILE_PAGE_IDS.len(), 6);
        assert_eq!(PROFILE_PAGE_IDS[0], "text");
        assert_eq!(PROFILE_PAGE_IDS[2], "tab");
        assert_eq!(PROFILE_PAGE_IDS[5], "advanced");
    }

    #[test]
    fn an_exit_tombstone_wins_before_any_pid_is_resolved() {
        assert_eq!(
            spawn_callback_action(true, true, false, false),
            SpawnCallbackAction::IgnoreExitedChild
        );
        assert_eq!(
            spawn_callback_action(false, false, false, true),
            SpawnCallbackAction::TerminateClosedTab
        );
        assert_eq!(
            spawn_callback_action(false, false, true, true),
            SpawnCallbackAction::StoreLiveChild
        );
        assert_eq!(
            spawn_callback_action(false, false, true, false),
            SpawnCallbackAction::IgnoreLateCallback
        );
    }

    #[test]
    fn gtk_icon_name_matches_the_packaged_application_id() {
        assert_eq!(APPLICATION_ID, "io.github.ksudo_dev.CoreTerminal");
    }

    #[test]
    fn window_group_rows_explain_launch_order_and_saved_values() {
        let entry = WindowGroupEntry {
            profile: "Homebrew".into(),
            working_directory: Some("/srv/project".into()),
            columns: 100,
            rows: 32,
        };
        assert_eq!(
            window_group_entry_summary(1, &entry),
            "Tab 2: Homebrew — /srv/project — 100×32"
        );
        let spec = super::TabLaunchSpec::from_window_group_entry(entry);
        assert_eq!(spec.profile_name, "Homebrew");
        assert_eq!(spec.working_directory.as_deref(), Some("/srv/project"));
        assert_eq!(spec.size, Some((100, 32)));
    }

    #[test]
    fn initial_window_profile_is_independent_from_new_tab_policy() {
        let profiles = ProfileStore::defaults();
        let settings = Settings {
            startup_profile: "Homebrew".into(),
            new_window_profile: "Pro".into(),
            new_tab_profile: "Ocean".into(),
            ..Settings::default()
        };
        assert_eq!(
            resolve_window_profile(&settings, &profiles, false),
            "Homebrew"
        );
        assert_eq!(resolve_window_profile(&settings, &profiles, true), "Pro");
    }

    #[test]
    fn default_new_window_policy_uses_startup_profile() {
        let profiles = ProfileStore::defaults();
        let settings = Settings {
            startup_profile: "Ocean".into(),
            new_window_profile: "default".into(),
            ..Settings::default()
        };
        assert_eq!(resolve_window_profile(&settings, &profiles, true), "Ocean");
    }

    #[test]
    fn missing_explicit_new_window_profile_falls_back_to_startup() {
        let mut profiles = ProfileStore::defaults();
        profiles.set_default("Pro").unwrap();
        let settings = Settings {
            selected_profile: "Homebrew".into(),
            startup_profile: "Ocean".into(),
            new_window_profile: "Missing profile".into(),
            ..Settings::default()
        };
        assert_eq!(resolve_window_profile(&settings, &profiles, true), "Ocean");
        assert_eq!(profiles.default_profile_name(), "Pro");
    }

    #[test]
    fn same_new_tab_policy_uses_the_active_tabs_profile() {
        let profiles = ProfileStore::defaults();
        let settings = Settings {
            selected_profile: "Homebrew".into(),
            startup_profile: "Homebrew".into(),
            new_tab_profile: "same".into(),
            ..Settings::default()
        };
        let mut sessions = SessionManager::empty();
        sessions.open_tab("Ocean", Some("/tmp/project"));
        assert_eq!(
            resolve_new_tab_profile(&settings, &sessions, &profiles),
            "Ocean"
        );
    }

    #[test]
    fn explicit_new_tab_profile_wins_and_invalid_values_fall_back_to_active() {
        let profiles = ProfileStore::defaults();
        let mut sessions = SessionManager::empty();
        sessions.open_tab("Ocean", None);
        let mut settings = Settings {
            new_tab_profile: "Pro".into(),
            ..Settings::default()
        };
        assert_eq!(
            resolve_new_tab_profile(&settings, &sessions, &profiles),
            "Pro"
        );
        settings.new_tab_profile = "Missing profile".into();
        assert_eq!(
            resolve_new_tab_profile(&settings, &sessions, &profiles),
            "Ocean"
        );
    }

    #[test]
    fn startup_only_changes_do_not_require_live_terminal_reconfiguration() {
        let before = Settings::default();
        let mut after = before.clone();
        after.startup_profile = "Ocean".into();
        after.new_window_profile = "Pro".into();
        assert!(!runtime_terminal_settings_changed(&before, &after));
        after.scroll_on_input = !before.scroll_on_input;
        assert!(runtime_terminal_settings_changed(&before, &after));
    }

    #[test]
    fn reassigned_tabs_always_receive_their_fallback_profiles() {
        assert!(runtime_profile_requires_reapply(false, false, true));
        assert!(runtime_profile_requires_reapply(false, true, false));
        assert!(runtime_profile_requires_reapply(true, false, false));
        assert!(!runtime_profile_requires_reapply(false, false, false));
    }

    #[test]
    fn deleting_profiles_preserves_an_unrelated_startup_choice() {
        assert_eq!(
            startup_profile_after_deletion(Some("Ocean"), "Custom", "Homebrew"),
            "Ocean"
        );
        assert_eq!(
            startup_profile_after_deletion(Some("Custom"), "Custom", "Homebrew"),
            "Homebrew"
        );
    }

    #[test]
    fn compatibility_profile_prefers_active_then_startup_then_default() {
        let mut profiles = ProfileStore::defaults();
        profiles.set_default("Pro").unwrap();
        assert_eq!(
            compatibility_profile(&profiles, "Ocean", "Homebrew").name,
            "Ocean"
        );
        assert_eq!(
            compatibility_profile(&profiles, "Missing", "Homebrew").name,
            "Homebrew"
        );
        assert_eq!(
            compatibility_profile(&profiles, "Missing", "Also missing").name,
            "Pro"
        );
    }

    #[test]
    fn profile_reader_preserves_permanently_unavailable_fields() {
        let source = include_str!("ui.rs");
        let start = source
            .rfind("fn read_profile_widgets(")
            .expect("read_profile_widgets must remain present");
        let end = source[start..]
            .find("fn load_profile_widgets(")
            .map(|offset| start + offset)
            .expect("load_profile_widgets must follow read_profile_widgets");
        let reader = &source[start..end];

        let overwritten = [
            "\"profile-antialias\"",
            "\"profile-use-bold-fonts\"",
            "\"profile-use-ansi\"",
            "\"profile-dynamic-colors\"",
            "\"profile-tab-show-ctrl-key\"",
            "\"profile-title-show-tty\"",
            "\"profile-title-show-ctrl-key\"",
            "\"profile-smooth-resize\"",
            "\"profile-restore-rows\"",
            "\"profile-restore-rows-limit\"",
            "\"profile-restore-bookmark\"",
            "\"profile-alt-scroll\"",
            "\"profile-keypad\"",
        ]
        .into_iter()
        .filter(|name| reader.contains(name))
        .collect::<Vec<_>>();
        assert!(
            overwritten.is_empty(),
            "reading the forced UI state for {overwritten:?} would destroy imported profile \
             metadata"
        );
    }

    #[test]
    fn initial_profile_controls_are_seeded_from_the_selected_profile() {
        let source = include_str!("ui.rs");
        let start = source
            .rfind("impl SettingsControls {")
            .expect("SettingsControls implementation must remain present");
        let end = source[start..]
            .find("fn named_check(")
            .map(|offset| start + offset)
            .expect("named_check must follow SettingsControls");
        let constructor = &source[start..end];

        for expected in [
            "font.set_text(&selected_defaults.font);",
            "font_size.set_value(selected_defaults.font_size);",
            "cursor_shape.set_selected(match selected_defaults.cursor_shape",
            "check(\"Blink cursor\", selected_defaults.cursor_blink)",
            "scrollback.set_value(selected_defaults.scrollback_lines as f64);",
            "terminal_type.set_text(&selected_defaults.terminal_type);",
        ] {
            assert!(
                constructor.contains(expected),
                "profile-owned initial control must use selected profile: {expected}"
            );
        }

        for stale_global_seed in [
            "font.set_text(&settings.font);",
            "font_size.set_value(settings.font_size);",
            "cursor_shape.set_selected(match settings.cursor_shape",
            "check(\"Blink cursor\", settings.cursor_blink)",
            "scrollback.set_value(settings.scrollback_lines as f64);",
            "terminal_type.set_text(&settings.terminal_type);",
        ] {
            assert!(
                !constructor.contains(stale_global_seed),
                "profile editor must not seed a profile-owned control from the global snapshot: \
                 {stale_global_seed}"
            );
        }
    }

    #[test]
    fn profile_loader_keeps_fixed_controls_truthful() {
        let source = include_str!("ui.rs");
        let start = source
            .rfind("fn load_profile_widgets(")
            .expect("load_profile_widgets must remain present");
        let end = source[start..]
            .find("fn sync_profile_control_sensitivity(")
            .map(|offset| start + offset)
            .expect("sensitivity synchronization must follow profile loading");
        let loader = &source[start..end];

        for name in [
            "\"profile-antialias\"",
            "\"profile-use-bold-fonts\"",
            "\"profile-use-ansi\"",
            "\"profile-dynamic-colors\"",
            "\"profile-smooth-resize\"",
            "\"profile-alt-scroll\"",
            "\"profile-keypad\"",
            "\"profile-tab-show-ctrl-key\"",
            "\"profile-title-show-tty\"",
            "\"profile-title-show-ctrl-key\"",
            "\"profile-restore-rows\"",
        ] {
            assert!(
                !loader.contains(name),
                "fixed control must keep its effective state after profile switches: {name}"
            );
        }
    }
}

pub fn install_style(display: &gtk::gdk::Display) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".core-profile-selector { min-width: 150px; }\
         .core-settings-sidebar { background: alpha(currentColor, 0.04); padding: 12px; }\
         .core-profile-row { min-height: 40px; }\
         .core-profile-row-label { padding: 8px 10px; }\
         .core-profile-action { min-width: 0; min-height: 36px; padding: 4px 8px; }\
         .core-settings-pane { padding: 24px; }\
         .core-settings-title { font-size: 1.35em; font-weight: 700; }\
         .core-settings-section { font-weight: 700; margin-top: 8px; }\
         .core-settings-action { min-width: 96px; min-height: 36px; }\
         .core-settings-tab { min-height: 34px; padding: 0 14px; }\
         vte.core-visual-bell { background-color: alpha(@warning_color, 0.28); }",
    );
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Build a native GTK title bar. The compositor/theme owns the real window
/// controls, so the application never draws a second set of imitation buttons.
pub fn build_header_bar() -> gtk::HeaderBar {
    let bar = gtk::HeaderBar::new();
    bar.set_show_title_buttons(true);
    bar
}

pub fn profile_selector(store: &ProfileStore, active_name: &str) -> gtk::DropDown {
    let names = store.names().collect::<Vec<_>>();
    let model = gtk::StringList::new(&names);
    let dropdown = gtk::DropDown::new(Some(model), None::<&gtk::Expression>);
    dropdown.add_css_class("core-profile-selector");
    if let Some(index) = names.iter().position(|name| *name == active_name) {
        dropdown.set_selected(index as u32);
    }
    dropdown
}

/// Merge user-owned profiles/groups over immutable project defaults.
fn load_user_profiles() -> ProfileStore {
    ProfileStore::load_user_or_defaults()
}

fn save_user_profiles(store: &ProfileStore) {
    if let Some(path) = ProfileStore::config_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = store.save_to_path(path);
    }
}

pub fn connect_profile_selector<F>(dropdown: &gtk::DropDown, on_changed: F)
where
    F: Fn(String) + 'static,
{
    dropdown.connect_selected_item_notify(move |dropdown: &gtk::DropDown| {
        let Some(item) = dropdown.selected_item() else {
            return;
        };
        let Ok(item) = item.downcast::<gtk::StringObject>() else {
            return;
        };
        on_changed(item.string().to_string());
    });
}

/// Show the settings window. The layout follows the familiar macOS Terminal
/// organization while using native GTK pages and controls: General, Profiles,
/// Window Groups, and Encodings. The callback receives values only after Save.
#[allow(deprecated)]
pub fn show_settings<F, L>(
    parent: &gtk::Window,
    settings: &Settings,
    profiles: ProfileStore,
    on_save: F,
    on_launch_group: L,
) -> gtk::Window
where
    F: Fn(Settings, ProfileStore) + 'static,
    L: Fn(WindowGroup) + 'static,
{
    debug_assert_eq!(SETTINGS_PAGE_IDS.len(), 4);
    let window = gtk::Window::builder()
        .title("Core Terminal Settings")
        // Do not grab keyboard/pointer focus: KVM users need to switch away
        // from settings without becoming stranded in this window.
        .modal(false)
        .transient_for(parent)
        .default_width(1120)
        .default_height(780)
        .build();
    // The Profiles page has a fixed navigation column and six visible tabs.
    // Keep enough room for both instead of allowing GTK to collapse either
    // side into an unusable sliver on first launch.
    window.set_size_request(960, 680);
    enforce_non_modal(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let titlebar = gtk::HeaderBar::new();
    titlebar.set_show_title_buttons(true);
    let title = gtk::Label::new(Some("Core Terminal Settings"));
    title.add_css_class("core-settings-title");
    titlebar.set_title_widget(Some(&title));
    window.set_titlebar(Some(&titlebar));

    let top_stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let top_switcher = gtk::StackSwitcher::new();
    top_switcher.set_stack(Some(&top_stack));
    top_switcher.set_halign(gtk::Align::Center);
    top_switcher.add_css_class("core-settings-tab");
    top_switcher.set_margin_top(10);
    top_switcher.set_margin_bottom(10);
    root.append(&top_switcher);
    // The stack is the main content region. Keeping it in the root (rather
    // than only attaching it to the switcher) is essential: StackSwitcher
    // renders navigation buttons, never the selected page itself.
    root.append(&top_stack);

    let profile_names = profiles.names().map(str::to_owned).collect::<Vec<_>>();
    let profile_store = Rc::new(RefCell::new(profiles));
    let launch_group: Rc<dyn Fn(WindowGroup)> = Rc::new(on_launch_group);
    let controls = SettingsControls::build(
        settings,
        &profile_names,
        &top_stack,
        profile_store.clone(),
        launch_group,
    );
    root.append(&controls.footer);
    window.set_child(Some(&root));

    let initial = settings.clone();
    let initial_focus = controls.startup_profile.clone();
    let import_button = controls.profile_import.clone();
    let import_startup = controls.startup_profile.clone();
    let import_parent = parent.clone();
    let import_store = profile_store.clone();
    let import_list = controls.profile_list.clone();
    let import_new_window = controls.new_window_profile.clone();
    let import_new_tab = controls.new_tab_profile.clone();
    let import_group_profile = controls.window_group_profile.clone();
    import_button.connect_clicked(move |_| {
        let chooser = gtk::FileChooserNative::builder()
            .title("Import Profile")
            .accept_label("Import")
            .cancel_label("Cancel")
            .transient_for(&import_parent)
            .action(gtk::FileChooserAction::Open)
            .build();
        let filter = gtk::FileFilter::new();
        filter.add_pattern("*.terminal");
        filter.set_name(Some("Terminal profiles (*.terminal)"));
        chooser.set_filter(&filter);
        let startup = import_startup.clone();
        let import_store = import_store.clone();
        let import_list = import_list.clone();
        let new_window = import_new_window.clone();
        let new_tab = import_new_tab.clone();
        let group_profile = import_group_profile.clone();
        let error_parent = import_parent.clone();
        chooser.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                let Some(path) = chooser.file().and_then(|file| file.path()) else {
                    show_settings_error(
                        &error_parent,
                        "Import failed",
                        "No profile file was selected.",
                    );
                    return;
                };
                match crate::profiles::import_terminal_plist_from_path(&path) {
                    Ok(imported) => {
                        let imported_name = imported.profile.name.clone();
                        if let Err(error) = import_store.borrow_mut().add_profile(imported.profile)
                        {
                            show_settings_error(&error_parent, "Import failed", error.to_string());
                            return;
                        }
                        import_list.append(&profile_list_row(&imported_name));
                        if let Some(row) = import_list
                            .row_at_index(import_list.observe_children().n_items() as i32 - 1)
                        {
                            import_list.select_row(Some(&row));
                        }
                        let model = startup.model().and_downcast::<gtk::StringList>();
                        if let Some(model) = &model {
                            if (0..model.n_items()).all(|index| {
                                model.string(index).as_deref() != Some(imported_name.as_str())
                            }) {
                                model.append(&imported_name);
                            }
                        }
                        append_dropdown_value(&new_window, &imported_name);
                        append_dropdown_value(&new_tab, &imported_name);
                        append_dropdown_value(&group_profile, &imported_name);
                        if !imported.fallbacks.is_empty() {
                            show_settings_error(
                                &error_parent,
                                "Profile imported with fallbacks",
                                imported.fallbacks.join(", "),
                            );
                        }
                    }
                    Err(error) => {
                        show_settings_error(&error_parent, "Import failed", error.to_string());
                    }
                }
            }
        });
        chooser.show();
    });

    let export_button = controls.profile_export.clone();
    let export_selection = controls.profile_selection.clone();
    let export_parent = parent.clone();
    let export_store = profile_store.clone();
    export_button.connect_clicked(move |_| {
        let chooser = gtk::FileChooserNative::builder()
            .title("Export Profile")
            .accept_label("Export")
            .cancel_label("Cancel")
            .transient_for(&export_parent)
            .action(gtk::FileChooserAction::Save)
            .build();
        chooser.set_current_name("Core Terminal Profile.terminal");
        let selection = export_selection.clone();
        let export_store = export_store.clone();
        let error_parent = export_parent.clone();
        chooser.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                let Some(path) = chooser.file().and_then(|file| file.path()) else {
                    show_settings_error(
                        &error_parent,
                        "Export failed",
                        "No destination file was selected.",
                    );
                    return;
                };
                let name = selection.borrow().clone();
                if name.is_empty() {
                    show_settings_error(&error_parent, "Export failed", "No profile is selected.");
                    return;
                }
                let Some(profile) = export_store.borrow().profile(&name).cloned() else {
                    show_settings_error(
                        &error_parent,
                        "Export failed",
                        format!("Profile '{name}' was not found."),
                    );
                    return;
                };
                if let Err(error) = crate::profiles::export_terminal_plist_to_path(&profile, &path)
                {
                    show_settings_error(&error_parent, "Export failed", error.to_string());
                }
            }
        });
        chooser.show();
    });

    // Profile mutations are routed through ProfileStore. Built-ins remain
    // protected by the backend; custom profiles can be created and cloned
    // directly from this editor without a hidden side channel.
    let add_button = controls.profile_add.clone();
    let add_startup = controls.startup_profile.clone();
    let add_new_window = controls.new_window_profile.clone();
    let add_new_tab = controls.new_tab_profile.clone();
    let add_group_profile = controls.window_group_profile.clone();
    let add_store = controls.profile_store.clone();
    let add_list = controls.profile_list.clone();
    add_button.connect_clicked(move |_| {
        let mut profile = TerminalProfile::homebrew();
        let name = unique_profile_name(&add_store.borrow(), "Custom Profile");
        profile.name = name.clone();
        if add_store.borrow_mut().add_profile(profile).is_ok() {
            if let Some(model) = add_startup.model().and_downcast::<gtk::StringList>() {
                model.append(&name);
            }
            append_dropdown_value(&add_new_window, &name);
            append_dropdown_value(&add_new_tab, &name);
            append_dropdown_value(&add_group_profile, &name);
            add_list.append(&profile_list_row(&name));
            if let Some(row) =
                add_list.row_at_index(add_list.observe_children().n_items() as i32 - 1)
            {
                add_list.select_row(Some(&row));
            }
        }
    });
    let duplicate_button = controls.profile_duplicate.clone();
    let duplicate_startup = controls.startup_profile.clone();
    let duplicate_new_window = controls.new_window_profile.clone();
    let duplicate_new_tab = controls.new_tab_profile.clone();
    let duplicate_group_profile = controls.window_group_profile.clone();
    let duplicate_selection = controls.profile_selection.clone();
    let duplicate_store = controls.profile_store.clone();
    let duplicate_list = controls.profile_list.clone();
    duplicate_button.connect_clicked(move |_| {
        let source = duplicate_selection.borrow().clone();
        if source.is_empty() {
            return;
        }
        let name = unique_profile_name(&duplicate_store.borrow(), &format!("Copy of {source}"));
        if duplicate_store
            .borrow_mut()
            .duplicate_profile(&source, &name)
            .is_ok()
        {
            if let Some(model) = duplicate_startup.model().and_downcast::<gtk::StringList>() {
                model.append(&name);
            }
            append_dropdown_value(&duplicate_new_window, &name);
            append_dropdown_value(&duplicate_new_tab, &name);
            append_dropdown_value(&duplicate_group_profile, &name);
            duplicate_list.append(&profile_list_row(&name));
            if let Some(row) =
                duplicate_list.row_at_index(duplicate_list.observe_children().n_items() as i32 - 1)
            {
                duplicate_list.select_row(Some(&row));
            }
        }
    });
    let delete_button = controls.profile_delete.clone();
    let delete_startup = controls.startup_profile.clone();
    let delete_new_window = controls.new_window_profile.clone();
    let delete_new_tab = controls.new_tab_profile.clone();
    let delete_group_profile = controls.window_group_profile.clone();
    let delete_selection = controls.profile_selection.clone();
    let delete_store = controls.profile_store.clone();
    let delete_list = controls.profile_list.clone();
    let delete_commit_window_group = controls.commit_window_group.clone();
    let delete_error_parent = parent.clone();
    delete_button.connect_clicked(move |_| {
        let name = delete_selection.borrow().clone();
        if name.is_empty() {
            return;
        }
        if let Err(error) = delete_commit_window_group() {
            show_settings_error(
                &delete_error_parent,
                "Profile could not be deleted",
                format!("Save or correct the current window-group draft first: {error}"),
            );
            return;
        }
        let startup_before_delete = dropdown_text(&delete_startup);
        match delete_store.borrow_mut().delete_profile(&name) {
            Ok(_) => {
                let default_after_delete = delete_store.borrow().selected_name().to_owned();
                if let Some(model) = delete_startup.model().and_downcast::<gtk::StringList>() {
                    if let Some(index) = string_list_position(&model, &name) {
                        model.remove(index);
                        let desired = startup_profile_after_deletion(
                            startup_before_delete.as_deref(),
                            &name,
                            &default_after_delete,
                        );
                        let fallback_index = index
                            .saturating_sub(1)
                            .min(model.n_items().saturating_sub(1));
                        delete_startup.set_selected(
                            string_list_position(&model, &desired).unwrap_or(fallback_index),
                        );
                        if let Some(row) = list_box_row_with_label(&delete_list, &name) {
                            delete_list.remove(&row);
                        }
                    }
                }
                remove_dropdown_value(&delete_new_window, &name);
                remove_dropdown_value(&delete_new_tab, &name);
                remove_dropdown_value(&delete_group_profile, &name);
            }
            Err(error) => {
                show_settings_error(
                    &delete_error_parent,
                    "Profile could not be deleted",
                    error.to_string(),
                );
            }
        }
    });

    let default_button = controls.profile_default.clone();
    let default_startup = controls.startup_profile.clone();
    let default_selection = controls.profile_selection.clone();
    let default_store = controls.profile_store.clone();
    default_button.connect_clicked(move |_| {
        let name = default_selection.borrow().clone();
        if default_store.borrow_mut().set_default(&name).is_ok() {
            if let Some(model) = default_startup.model().and_downcast::<gtk::StringList>() {
                if let Some(index) = (0..model.n_items())
                    .find(|index| model.string(*index).as_deref() == Some(name.as_str()))
                {
                    default_startup.set_selected(index);
                }
            }
        }
    });

    let reset_button = controls.profile_reset.clone();
    let reset_selection = controls.profile_selection.clone();
    let reset_store = controls.profile_store.clone();
    let reset_stack = controls.profile_stack.clone();
    reset_button.connect_clicked(move |_| {
        let name = reset_selection.borrow().clone();
        if reset_store.borrow_mut().reset_overrides(&name).is_err() {
            return;
        }
        if let Some(profile) = reset_store.borrow().profile(&name).cloned() {
            load_profile_widgets(&reset_stack, &profile);
        }
    });
    let save_button = controls.save_button.clone();
    let cancel_button = controls.cancel_button.clone();
    let save_window = window.clone();
    save_button.connect_clicked(move |_| {
        if let Err(error) = (controls.commit_window_group)() {
            show_settings_error(&save_window, "Window group could not be saved", error);
            return;
        }
        let cursor_shape = match controls
            .cursor_shape
            .selected_item()
            .and_then(|item| item.downcast::<gtk::StringObject>().ok())
            .map(|item| item.string().to_string())
            .as_deref()
        {
            Some("I-beam") => CursorShape::IBeam,
            Some("Underline") => CursorShape::Underline,
            _ => CursorShape::Block,
        };
        let startup_profile = controls
            .startup_profile
            .selected_item()
            .and_then(|item| item.downcast::<gtk::StringObject>().ok())
            .map(|item| item.string().to_string())
            .unwrap_or_else(|| initial.selected_profile.clone());
        let mut edited_profiles = controls.profile_store.borrow().clone();
        let editor_profile_name = controls.profile_selection.borrow().clone();
        if let Some(mut profile) = edited_profiles.profile(&editor_profile_name).cloned() {
            profile.font = controls.font.text().to_string();
            profile.font_size = controls.font_size.value();
            profile.cursor_shape = cursor_shape;
            profile.cursor_blink = controls.cursor_blink.is_active();
            profile.scrollback_lines = controls.scrollback.value() as u32;
            profile.shell_command = controls.profile_shell_command.text().to_string();
            profile.run_inside_shell = controls.run_command_inside_shell.is_active();
            profile.ask_before_close_exceptions = controls
                .profile_exceptions
                .text()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect();
            profile.close_on_exit = match controls.profile_close_on_exit.selected() {
                1 => CloseOnExit::Clean,
                2 => CloseOnExit::Error,
                3 => CloseOnExit::Always,
                _ => CloseOnExit::Never,
            };
            profile.ask_before_close_policy = match controls.profile_ask_policy.selected() {
                1 => AskBeforeClosePolicy::Always,
                2 => AskBeforeClosePolicy::NonExempt,
                _ => AskBeforeClosePolicy::Never,
            };
            profile.shell_exit_action = match controls.profile_exit_action.selected() {
                1 => ShellExitAction::Keep,
                2 => ShellExitAction::CloseTab,
                3 => ShellExitAction::CloseWindow,
                _ => ShellExitAction::Ask,
            };
            // These booleans are read-only migration inputs. Current profiles
            // persist the explicit enum controls above.
            profile.close_on_clean_exit = false;
            profile.close_on_error = false;
            profile.ask_before_close = false;
            read_profile_widgets(
                &controls.profile_stack,
                &mut profile,
                &controls.profile_mappings,
            );
            // Built-ins accept edits but remain protected from deletion;
            // custom profiles accept both edits and deletion.
            let _ = edited_profiles.update_profile(profile);
        }
        // These legacy fields remain in Settings for schema compatibility.
        // Keep them aligned with the live session profile even when the user
        // browses a different profile in the editor before pressing Save.
        let compatibility_profile = compatibility_profile(
            &edited_profiles,
            &initial.selected_profile,
            &startup_profile,
        )
        .clone();
        on_save(
            Settings {
                schema_version: CURRENT_SCHEMA_VERSION,
                startup_profile,
                startup_window_group: if controls.use_startup_group.is_active() {
                    dropdown_text(&controls.startup_window_group)
                        .filter(|name| name != "No groups saved")
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                // The settings profile editor and the startup picker are
                // separate concerns. Saving startup preferences must never
                // switch a live terminal session to that profile.
                selected_profile: initial.selected_profile.clone(),
                new_window_profile: dropdown_value(&controls.new_window_profile, "Startup profile"),
                new_tab_profile: dropdown_value(&controls.new_tab_profile, "Same as current tab"),
                new_window_same_directory: controls.new_window_same_directory.is_active(),
                font: compatibility_profile.font.clone(),
                font_size: compatibility_profile.font_size,
                cursor_shape: compatibility_profile.cursor_shape,
                cursor_blink: compatibility_profile.cursor_blink,
                scrollback_lines: compatibility_profile.scrollback_lines,
                window_width: controls.window_width.value() as i32,
                window_height: controls.window_height.value() as i32,
                use_custom_command: controls.use_custom_command.is_active(),
                custom_command: controls.custom_command.text().to_string(),
                shell: controls.shell.text().to_string(),
                // This checkbox belongs to the selected profile. Retain the
                // legacy global fallback until General exposes a separate
                // control for it.
                run_command_inside_shell: initial.run_command_inside_shell,
                // Locale is profile-owned in the editor; retain the global
                // setting until a dedicated global locale control exists.
                locale: initial.locale.clone(),
                set_locale_environment: initial.set_locale_environment,
                new_tab_same_directory: controls.new_tab_same_directory.is_active(),
                ctrl_number_tabs: controls.ctrl_number_tabs.is_active(),
                scroll_on_output: controls.scroll_on_output.is_active(),
                scroll_on_input: controls.scroll_on_input.is_active(),
                audible_bell: controls.audible_bell.is_active(),
                bold_is_bright: controls.bold_is_bright.is_active(),
                // Pointer auto-hide is deliberately never user-configurable: the
                // KVM-safe runtime policy always keeps the pointer visible.
                mouse_autohide: false,
                background_notifications: controls.background_notifications.is_active(),
                terminal_type: compatibility_profile.terminal_type,
            },
            edited_profiles,
        );
        save_window.close();
    });
    let cancel_window = window.clone();
    cancel_button.connect_clicked(move |_| cancel_window.close());
    window.present();
    initial_focus.grab_focus();
    window
}

struct SettingsControls {
    footer: gtk::Box,
    save_button: gtk::Button,
    cancel_button: gtk::Button,
    startup_profile: gtk::DropDown,
    use_startup_group: gtk::CheckButton,
    startup_window_group: gtk::DropDown,
    new_window_profile: gtk::DropDown,
    new_tab_profile: gtk::DropDown,
    font: gtk::Entry,
    font_size: gtk::SpinButton,
    cursor_shape: gtk::DropDown,
    cursor_blink: gtk::CheckButton,
    scrollback: gtk::SpinButton,
    window_width: gtk::SpinButton,
    window_height: gtk::SpinButton,
    use_custom_command: gtk::CheckButton,
    custom_command: gtk::Entry,
    shell: gtk::Entry,
    run_command_inside_shell: gtk::CheckButton,
    profile_shell_command: gtk::Entry,
    profile_close_on_exit: gtk::DropDown,
    profile_ask_policy: gtk::DropDown,
    profile_exceptions: gtk::Entry,
    profile_exit_action: gtk::DropDown,
    new_tab_same_directory: gtk::CheckButton,
    new_window_same_directory: gtk::CheckButton,
    ctrl_number_tabs: gtk::CheckButton,
    scroll_on_output: gtk::CheckButton,
    scroll_on_input: gtk::CheckButton,
    audible_bell: gtk::CheckButton,
    bold_is_bright: gtk::CheckButton,
    // Pointer auto-hide intentionally has no settings control. All VTE
    // terminals are forced to keep the pointer visible for KVM safety.
    background_notifications: gtk::CheckButton,
    profile_add: gtk::Button,
    profile_duplicate: gtk::Button,
    profile_delete: gtk::Button,
    profile_import: gtk::Button,
    profile_export: gtk::Button,
    profile_default: gtk::Button,
    profile_reset: gtk::Button,
    profile_store: Rc<RefCell<ProfileStore>>,
    profile_selection: Rc<RefCell<String>>,
    profile_stack: gtk::Stack,
    /// Canonical mappings stay separate from the display-only GTK model. This
    /// is important because an encoded PTY action is not a human label.
    profile_mappings: Rc<RefCell<Vec<KeyMapping>>>,
    profile_list: gtk::ListBox,
    window_group_profile: gtk::DropDown,
    commit_window_group: Rc<dyn Fn() -> Result<(), String>>,
}

impl SettingsControls {
    fn build(
        settings: &Settings,
        profile_names: &[String],
        top_stack: &gtk::Stack,
        profile_store: Rc<RefCell<ProfileStore>>,
        on_launch_group: Rc<dyn Fn(WindowGroup)>,
    ) -> Self {
        // Profile-owned controls must be seeded from the profile selected in
        // the editor. The global Settings copy is a compatibility snapshot
        // and can legitimately describe the previously active profile.
        let selected_defaults = {
            let store = profile_store.borrow();
            store
                .profile(&settings.selected_profile)
                .cloned()
                .unwrap_or_else(|| store.selected().clone())
        };
        let selected_profile_name = selected_defaults.name.clone();
        let general = gtk::Box::new(gtk::Orientation::Vertical, 18);
        general.add_css_class("core-settings-pane");
        general.set_margin_start(28);
        general.set_margin_end(28);
        general.set_margin_top(20);
        general.set_margin_bottom(20);
        page_heading(
            &general,
            "General",
            "Terminal-wide behavior and startup preferences.",
        );
        let general_grid = form_grid();

        let startup_label = gtk::Label::new(Some("Startup profile"));
        startup_label.set_halign(gtk::Align::Start);
        general_grid.attach(&startup_label, 0, 0, 1, 1);
        let names = profile_names.iter().map(String::as_str).collect::<Vec<_>>();
        let startup =
            gtk::DropDown::new(Some(gtk::StringList::new(&names)), None::<&gtk::Expression>);
        startup.set_widget_name("startup-profile");
        startup.set_selected(
            profile_names
                .iter()
                .position(|name| name == &settings.startup_profile)
                .or_else(|| {
                    profile_names
                        .iter()
                        .position(|name| name == &settings.selected_profile)
                })
                .unwrap_or(0) as u32,
        );
        startup.set_hexpand(true);
        general_grid.attach(&startup, 1, 0, 1, 1);

        let group_names = profile_store
            .borrow()
            .window_groups()
            .iter()
            .map(|group| group.name.clone())
            .collect::<Vec<_>>();
        let startup_group_names = if group_names.is_empty() {
            vec!["No groups saved".to_owned()]
        } else {
            group_names
        };
        let use_startup_group = gtk::CheckButton::with_label("Open a saved window group");
        use_startup_group.set_widget_name("use-startup-window-group");
        use_startup_group.set_active(!settings.startup_window_group.is_empty());
        general_grid.attach(&use_startup_group, 0, 1, 1, 1);
        let startup_group_model = gtk::StringList::new(
            &startup_group_names
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let startup_window_group =
            gtk::DropDown::new(Some(startup_group_model.clone()), None::<&gtk::Expression>);
        startup_window_group.set_widget_name("startup-window-group");
        startup_window_group.set_selected(
            startup_group_names
                .iter()
                .position(|name| *name == settings.startup_window_group)
                .unwrap_or(0) as u32,
        );
        startup_window_group.set_sensitive(
            use_startup_group.is_active() && !profile_store.borrow().window_groups().is_empty(),
        );
        startup_window_group.set_hexpand(true);
        general_grid.attach(&startup_window_group, 1, 1, 1, 1);
        let startup_for_mode = startup.clone();
        let group_for_mode = startup_window_group.clone();
        let group_model_for_mode = startup_group_model.clone();
        startup.set_sensitive(!use_startup_group.is_active());
        use_startup_group.connect_toggled(move |button| {
            startup_for_mode.set_sensitive(!button.is_active());
            let groups_available = !(group_model_for_mode.n_items() == 1
                && group_model_for_mode.string(0).as_deref() == Some("No groups saved"));
            group_for_mode.set_sensitive(button.is_active() && groups_available);
        });

        let shell_label = gtk::Label::new(Some("Shell command"));
        shell_label.set_halign(gtk::Align::Start);
        general_grid.attach(&shell_label, 0, 2, 1, 1);
        let use_custom_command = gtk::CheckButton::with_label("Use custom command");
        use_custom_command.set_active(settings.use_custom_command);
        general_grid.attach(&use_custom_command, 1, 2, 1, 1);
        let custom_command = gtk::Entry::new();
        custom_command.set_widget_name("custom-command");
        custom_command.set_placeholder_text(Some("/bin/bash --login"));
        custom_command.set_text(&settings.custom_command);
        custom_command.set_hexpand(true);
        custom_command.set_sensitive(settings.use_custom_command);
        general_grid.attach(&custom_command, 1, 3, 1, 1);
        let shell_hint = hint_label("When disabled, Core Terminal starts the user's login shell.");
        general_grid.attach(&shell_hint, 1, 4, 1, 1);
        let shell = gtk::Entry::new();
        shell.set_widget_name("login-shell");
        shell.set_placeholder_text(Some("/bin/bash (complete path)"));
        shell.set_text(&settings.shell);
        shell.set_hexpand(true);
        general_grid.attach(&field_label("Login shell"), 0, 5, 1, 1);
        general_grid.attach(&shell, 1, 5, 1, 1);
        let command_for_toggle = custom_command.clone();
        use_custom_command.connect_toggled(move |button| {
            command_for_toggle.set_sensitive(button.is_active());
        });

        let new_tab_same_directory = check(
            "Open new tabs in the current directory",
            settings.new_tab_same_directory,
        );
        general_grid.attach(&new_tab_same_directory, 1, 6, 1, 1);
        let ctrl_number_tabs = check("Enable Ctrl+1–9 tab switching", settings.ctrl_number_tabs);
        ctrl_number_tabs.set_widget_name("ctrl-number-tabs");
        general_grid.attach(&ctrl_number_tabs, 1, 7, 1, 1);
        let profile_policy_names = std::iter::once("Startup profile")
            .chain(profile_names.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let new_window_profile = gtk::DropDown::new(
            Some(gtk::StringList::new(&profile_policy_names)),
            None::<&gtk::Expression>,
        );
        new_window_profile.set_widget_name("new-window-profile");
        let new_window_index = if settings.new_window_profile == "default" {
            0
        } else {
            profile_names
                .iter()
                .position(|name| name == &settings.new_window_profile)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        new_window_profile.set_selected(new_window_index as u32);
        let new_window_label = field_label("New window profile");
        general_grid.attach(&new_window_label, 0, 8, 1, 1);
        general_grid.attach(&new_window_profile, 1, 8, 1, 1);
        let new_tab_policy_names = std::iter::once("Same as current tab")
            .chain(profile_names.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let new_tab_profile = gtk::DropDown::new(
            Some(gtk::StringList::new(&new_tab_policy_names)),
            None::<&gtk::Expression>,
        );
        new_tab_profile.set_widget_name("new-tab-profile");
        let new_tab_index = if settings.new_tab_profile == "same" {
            0
        } else {
            profile_names
                .iter()
                .position(|name| name == &settings.new_tab_profile)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        new_tab_profile.set_selected(new_tab_index as u32);
        let new_tab_label = field_label("New tab profile");
        general_grid.attach(&new_tab_label, 0, 9, 1, 1);
        general_grid.attach(&new_tab_profile, 1, 9, 1, 1);
        let new_window_same_directory = check(
            "Open new windows in the current directory",
            settings.new_window_same_directory,
        );
        general_grid.attach(&new_window_same_directory, 1, 10, 1, 1);
        general.append(&general_grid);
        top_stack.add_titled(&scroll_page(&general), Some("general"), "General");

        let profile_page = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let profile_sidebar = gtk::Box::new(gtk::Orientation::Vertical, 8);
        profile_sidebar.set_widget_name("profile-sidebar");
        profile_sidebar.add_css_class("core-settings-sidebar");
        profile_sidebar.set_size_request(300, -1);
        let profile_heading = gtk::Label::new(Some("Profiles"));
        profile_heading.add_css_class("core-settings-title");
        profile_heading.set_halign(gtk::Align::Start);
        profile_sidebar.append(&profile_heading);
        let profile_list = gtk::ListBox::new();
        profile_list.set_widget_name("profile-list");
        profile_list.set_selection_mode(gtk::SelectionMode::Single);
        profile_list.set_hexpand(true);
        profile_list.set_vexpand(true);
        for (index, name) in profile_names.iter().enumerate() {
            profile_list.append(&profile_list_row(name));
            if name == &selected_profile_name {
                if let Some(row) = profile_list.row_at_index(index as i32) {
                    profile_list.select_row(Some(&row));
                }
            }
        }
        let profile_list_scroll = gtk::ScrolledWindow::new();
        profile_list_scroll.set_widget_name("profile-list-scroll");
        profile_list_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        profile_list_scroll.set_hexpand(true);
        profile_list_scroll.set_vexpand(true);
        profile_list_scroll.set_min_content_height(260);
        profile_list_scroll.set_child(Some(&profile_list));
        profile_sidebar.append(&profile_list_scroll);
        let profile_actions = gtk::Grid::new();
        profile_actions.set_widget_name("profile-actions");
        profile_actions.set_column_homogeneous(true);
        profile_actions.set_column_spacing(6);
        profile_actions.set_row_spacing(6);
        let mut profile_buttons = Vec::new();
        for (index, (label, tooltip, widget_name)) in [
            ("Add", "Add a new profile", "profile-add"),
            (
                "Duplicate",
                "Duplicate the selected profile",
                "profile-duplicate",
            ),
            (
                "Delete",
                "Delete the selected custom profile",
                "profile-delete",
            ),
            (
                "Import",
                "Import a project profile from a .terminal file",
                "profile-import",
            ),
            (
                "Export",
                "Export the selected profile as a .terminal file",
                "profile-export",
            ),
            (
                "Set Default",
                "Make selected profile the default startup profile",
                "profile-default",
            ),
            ("Reset", "Reset selected profile controls", "profile-reset"),
        ]
        .into_iter()
        .enumerate()
        {
            let button = gtk::Button::with_label(label);
            button.add_css_class("core-profile-action");
            button.set_tooltip_text(Some(tooltip));
            button.set_widget_name(widget_name);
            button.set_hexpand(true);
            profile_buttons.push(button.clone());
            let (column, row, width) = match index {
                0 => (0, 0, 1),
                1 => (1, 0, 1),
                2 => (0, 1, 1),
                6 => (1, 1, 1),
                3 => (0, 2, 1),
                4 => (1, 2, 1),
                _ => (0, 3, 2),
            };
            profile_actions.attach(&button, column, row, width, 1);
        }
        let profile_selection = Rc::new(RefCell::new(selected_profile_name.clone()));
        let profile_selection_for_list = profile_selection.clone();
        profile_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                *profile_selection_for_list.borrow_mut() =
                    profile_list_row_name(row).unwrap_or_default();
            }
        });
        profile_sidebar.append(&profile_actions);
        profile_page.append(&profile_sidebar);

        let profile_content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        profile_content.add_css_class("core-settings-pane");
        profile_content.set_hexpand(true);
        profile_content.set_size_request(650, -1);
        let profile_switcher = gtk::StackSwitcher::new();
        profile_switcher.set_widget_name("profile-page-switcher");
        let profile_stack = gtk::Stack::new();
        profile_stack.set_widget_name("profile-pages");
        profile_stack.set_hexpand(true);
        profile_stack.set_vexpand(true);
        profile_switcher.set_stack(Some(&profile_stack));
        profile_switcher.set_halign(gtk::Align::Fill);
        profile_switcher.set_hexpand(true);
        profile_content.append(&profile_switcher);

        let appearance = gtk::Box::new(gtk::Orientation::Vertical, 16);
        appearance.set_widget_name("text");
        appearance.add_css_class("core-settings-pane");
        page_heading(
            &appearance,
            "Text",
            "Text, cursor, and scrollback for the selected profile.",
        );
        let appearance_grid = form_grid();
        let font_label = field_label("Font");
        appearance_grid.attach(&font_label, 0, 0, 1, 1);
        let font = gtk::Entry::new();
        font.set_widget_name("profile-font");
        font.set_text(&selected_defaults.font);
        font.set_hexpand(true);
        appearance_grid.attach(&font, 1, 0, 1, 1);
        let font_size_label = field_label("Font size");
        appearance_grid.attach(&font_size_label, 0, 1, 1, 1);
        let font_size = gtk::SpinButton::with_range(6.0, 96.0, 1.0);
        font_size.set_widget_name("profile-font-size");
        font_size.set_value(selected_defaults.font_size);
        appearance_grid.attach(&font_size, 1, 1, 1, 1);
        let cursor_label = field_label("Cursor shape");
        appearance_grid.attach(&cursor_label, 0, 2, 1, 1);
        let cursor_shape_names = ["Block", "I-beam", "Underline"];
        let cursor_shape = gtk::DropDown::new(
            Some(gtk::StringList::new(&cursor_shape_names)),
            None::<&gtk::Expression>,
        );
        cursor_shape.set_widget_name("profile-cursor-shape");
        cursor_shape.set_selected(match selected_defaults.cursor_shape {
            CursorShape::Block => 0,
            CursorShape::IBeam => 1,
            CursorShape::Underline => 2,
        });
        appearance_grid.attach(&cursor_shape, 1, 2, 1, 1);
        let cursor_blink = check("Blink cursor", selected_defaults.cursor_blink);
        cursor_blink.set_widget_name("profile-cursor-blink");
        appearance_grid.attach(&cursor_blink, 1, 3, 1, 1);
        let scrollback_label = field_label("Scrollback lines");
        appearance_grid.attach(&scrollback_label, 0, 4, 1, 1);
        let scrollback = gtk::SpinButton::with_range(100.0, 1_000_000.0, 100.0);
        scrollback.set_widget_name("profile-scrollback");
        scrollback.set_value(selected_defaults.scrollback_lines as f64);
        appearance_grid.attach(&scrollback, 1, 4, 1, 1);
        appearance.append(&appearance_grid);
        let color_grid = form_grid();
        for (row, (label, value)) in [
            ("Background color", selected_defaults.background.as_str()),
            ("Foreground color", selected_defaults.foreground.as_str()),
            ("Bold color", selected_defaults.bold_color.as_str()),
            ("Selection color", selected_defaults.selection.as_str()),
            ("Cursor color", selected_defaults.cursor.as_str()),
        ]
        .into_iter()
        .enumerate()
        {
            let label_widget = field_label(label);
            color_grid.attach(&label_widget, 0, row as i32, 1, 1);
            let button = gtk::ColorDialogButton::new(None);
            button.set_widget_name(match row {
                0 => "profile-background-color",
                1 => "profile-foreground-color",
                2 => "profile-bold-color",
                3 => "profile-selection-color",
                _ => "profile-cursor-color",
            });
            if let Ok(color) = gtk::gdk::RGBA::parse(value) {
                button.set_rgba(&color);
            }
            button.set_size_request(120, 36);
            color_grid.attach(&button, 1, row as i32, 1, 1);
        }
        appearance.append(&color_grid);
        let alpha = gtk::SpinButton::with_range(0.0, 1.0, 0.05);
        alpha.set_widget_name("profile-background-alpha");
        alpha.set_value(selected_defaults.background_alpha);
        alpha.set_digits(2);
        let alpha_grid = form_grid();
        alpha_grid.attach(&field_label("Background opacity"), 0, 0, 1, 1);
        alpha_grid.attach(&alpha, 1, 0, 1, 1);
        appearance.append(&alpha_grid);
        appearance.append(&renderer_owned_check(
            "Antialias text (always enabled by VTE)",
            "profile-antialias",
        ));
        appearance.append(&renderer_owned_check(
            "Use bold fonts (managed by VTE)",
            "profile-use-bold-fonts",
        ));
        appearance.append(&named_check(
            "Blink text",
            selected_defaults.text_blink,
            "profile-text-blink",
        ));
        appearance.append(&renderer_owned_check(
            "Display ANSI colors (always enabled by VTE)",
            "profile-use-ansi",
        ));
        appearance.append(&named_check(
            "Bright ANSI colors",
            selected_defaults.bold_is_bright,
            "profile-ansi-bright",
        ));
        appearance.append(&renderer_owned_check(
            "Allow dynamic foreground and background colors (managed by VTE)",
            "profile-dynamic-colors",
        ));
        let palette = gtk::FlowBox::new();
        palette.set_widget_name("text-palette");
        palette.set_selection_mode(gtk::SelectionMode::None);
        for (index, value) in selected_defaults.ansi_palette.iter().enumerate() {
            let button = gtk::ColorDialogButton::new(None);
            button.set_widget_name(&format!("profile-palette-{index}"));
            if let Ok(color) = gtk::gdk::RGBA::parse(value) {
                button.set_rgba(&color);
            }
            button.set_tooltip_text(Some(&format!("ANSI palette color {}", index + 1)));
            button.set_size_request(42, 36);
            palette.insert(&button, -1);
        }
        appearance.append(&palette);
        appearance.append(&hint_label(
            "Colors, selection, and rendering are applied by VTE from the selected profile.",
        ));
        profile_stack.add_titled(&scroll_page(&appearance), Some("text"), "Text");

        let tab_page = gtk::Box::new(gtk::Orientation::Vertical, 16);
        tab_page.set_widget_name("tab");
        tab_page.add_css_class("core-settings-pane");
        page_heading(
            &tab_page,
            "Tab",
            "Shell-provided titles and current-directory reporting are kept visible.",
        );
        let tab_grid = form_grid();
        for (row, (label, active, name)) in [
            (
                "Show profile in tab title",
                selected_defaults.tab_title_show_profile,
                "profile-tab-show-profile",
            ),
            (
                "Show shell in tab title",
                selected_defaults.tab_title_show_shell,
                "profile-tab-show-shell",
            ),
            (
                "Show directory in tab title",
                selected_defaults.tab_title_show_directory,
                "profile-tab-show-directory",
            ),
            (
                "Show job in tab title",
                selected_defaults.tab_title_show_job,
                "profile-tab-show-job",
            ),
            (
                "Use custom tab title",
                !selected_defaults.custom_tab_title.is_empty(),
                "profile-tab-custom",
            ),
            (
                "Show activity indicator",
                selected_defaults.tab_title_show_activity,
                "profile-tab-activity",
            ),
            (
                "Show other items with a custom title",
                selected_defaults.tab_title_show_other_items,
                "profile-tab-show-other-items",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            tab_grid.attach(&named_check(label, active, name), 0, row as i32, 2, 1);
        }
        let custom_tab_title = gtk::Entry::new();
        custom_tab_title.set_widget_name("profile-custom-tab-title");
        custom_tab_title.set_text(&selected_defaults.custom_tab_title);
        custom_tab_title.set_placeholder_text(Some("Custom title (when enabled)"));
        custom_tab_title.set_hexpand(true);
        tab_grid.attach(&field_label("Custom title"), 0, 7, 1, 1);
        tab_grid.attach(&custom_tab_title, 1, 7, 1, 1);
        tab_page.append(&tab_grid);
        tab_page.append(&hint_label("Tab labels are generated from VTE's title and current-directory signals. This avoids shell integration scripts while retaining useful tab context."));
        tab_page.append(&hint_label(
            "Use General for new-tab directory behavior and Ctrl+1–9 tab switching.",
        ));
        let tab_extra = form_grid();
        for (row, (label, active, name)) in [
            (
                "Show process in tab title",
                selected_defaults.tab_title_show_process,
                "profile-tab-show-process",
            ),
            (
                "Show arguments in tab title",
                selected_defaults.tab_title_show_arguments,
                "profile-tab-show-arguments",
            ),
            (
                "Show path in tab title",
                selected_defaults.tab_title_show_path,
                "profile-tab-show-path",
            ),
            (
                "Show dimensions in tab title",
                selected_defaults.tab_title_show_dimensions,
                "profile-tab-show-dimensions",
            ),
            (
                "Show Ctrl key in tab title",
                selected_defaults.tab_title_show_ctrl_key,
                "profile-tab-show-ctrl-key",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let button = if name == "profile-tab-show-ctrl-key" {
                unavailable_check(label, name)
            } else {
                named_check(label, active, name)
            };
            if name == "profile-tab-show-ctrl-key" {
                button.set_tooltip_text(Some(
                    "The current VTE/Wayland API does not expose this macOS title component.",
                ));
            }
            tab_extra.attach(&button, 0, row as i32, 2, 1);
        }
        tab_page.append(&tab_extra);
        profile_stack.add_titled(&scroll_page(&tab_page), Some("tab"), "Tab");

        let window_page = profile_page_with_checks(
            "Window",
            "Terminal dimensions, titles, background, and scrollback.",
            &[],
        );
        window_page.0.set_widget_name("window");
        let window_grid = form_grid();
        let window_title = gtk::Entry::new();
        window_title.set_widget_name("profile-window-title");
        window_title.set_text(&selected_defaults.custom_window_title);
        window_title.set_placeholder_text(Some("Core Terminal"));
        window_title.set_hexpand(true);
        window_grid.attach(&field_label("Window title"), 0, 0, 1, 1);
        window_grid.attach(&window_title, 1, 0, 1, 1);
        let background_image = gtk::Entry::new();
        background_image.set_widget_name("profile-background-image");
        background_image.set_placeholder_text(Some("Optional background image path"));
        background_image.set_text(
            selected_defaults
                .background_image_path
                .as_deref()
                .unwrap_or(""),
        );
        background_image.set_hexpand(true);
        window_grid.attach(&field_label("Background image"), 0, 1, 1, 1);
        window_grid.attach(&background_image, 1, 1, 1, 1);
        let background_mode = gtk::DropDown::new(
            Some(gtk::StringList::new(&["Tile", "Scale", "Center"])),
            None::<&gtk::Expression>,
        );
        background_mode.set_widget_name("profile-background-mode");
        background_mode.set_selected(match selected_defaults.background_image_mode {
            BackgroundImageMode::Tile => 0,
            BackgroundImageMode::Scale => 1,
            BackgroundImageMode::Center => 2,
        });
        window_grid.attach(&field_label("Image mode"), 0, 2, 1, 1);
        window_grid.attach(&background_mode, 1, 2, 1, 1);
        for (row, (label, active, name)) in [
            (
                "Show profile in window title",
                selected_defaults.title_show_profile,
                "profile-title-show-profile",
            ),
            (
                "Show shell in window title",
                selected_defaults.title_show_shell,
                "profile-title-show-shell",
            ),
            (
                "Show directory in window title",
                selected_defaults.title_show_directory,
                "profile-title-show-directory",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            window_grid.attach(&named_check(label, active, name), 0, row as i32 + 3, 2, 1);
        }
        for (row, (label, value)) in [
            ("Columns", selected_defaults.columns),
            ("Rows", selected_defaults.rows),
        ]
        .into_iter()
        .enumerate()
        {
            let spin = gtk::SpinButton::with_range(1.0, 1_000.0, 1.0);
            spin.set_value(value as f64);
            if label == "Columns" {
                spin.set_widget_name("profile-columns");
            } else {
                spin.set_widget_name("profile-rows");
            }
            window_grid.attach(&field_label(label), 0, row as i32 + 6, 1, 1);
            window_grid.attach(&spin, 1, row as i32 + 6, 1, 1);
        }
        window_page.0.append(&window_grid);
        window_page.0.append(&renderer_owned_check(
            "Smooth window resizing (managed by the desktop compositor)",
            "profile-smooth-resize",
        ));
        window_page.0.append(&named_check(
            "Unlimited scrollback",
            selected_defaults.scrollback_unlimited,
            "profile-unlimited-scrollback",
        ));
        window_page.0.append(&unavailable_check(
            "Restore text after logout (live process state cannot be restored)",
            "profile-restore-rows",
        ));
        window_page.0.append(&unavailable_check(
            "Show live terminal contents in the dock (not portable across Linux docks)",
            "profile-minimized-dock-contents",
        ));
        let restore_limit = gtk::SpinButton::with_range(1.0, 1_000_000.0, 100.0);
        restore_limit.set_widget_name("profile-restore-rows-limit");
        restore_limit.set_value(selected_defaults.restore_rows_limit as f64);
        restore_limit.set_sensitive(false);
        restore_limit.set_tooltip_text(Some(
            "Live terminal text cannot be restored after logout on this platform.",
        ));
        window_page.0.append(&field_label("Restore rows limit"));
        window_page.0.append(&restore_limit);
        let restore_bookmark = gtk::Entry::new();
        restore_bookmark.set_widget_name("profile-restore-bookmark");
        restore_bookmark.set_text(&selected_defaults.restore_rows_bookmark);
        restore_bookmark.set_placeholder_text(Some("Optional bookmark"));
        restore_bookmark.set_hexpand(true);
        restore_bookmark.set_sensitive(false);
        restore_bookmark.set_tooltip_text(Some(
            "Live terminal text cannot be restored after logout on this platform.",
        ));
        window_page.0.append(&field_label("Restore bookmark"));
        window_page.0.append(&restore_bookmark);
        let scrollback_limit = gtk::SpinButton::with_range(100.0, 1_000_000.0, 100.0);
        scrollback_limit.set_widget_name("profile-scrollback-limit");
        scrollback_limit.set_value(selected_defaults.scrollback_limit as f64);
        window_page
            .0
            .append(&field_label("Bounded scrollback limit"));
        window_page.0.append(&scrollback_limit);
        window_page.0.append(&hint_label(
            "Columns and rows are the actual VTE terminal dimensions for this profile.",
        ));
        let window_extra = form_grid();
        for (row, (label, active, name)) in [
            (
                "Show working directory in title",
                selected_defaults.title_show_working_directory,
                "profile-title-show-working-directory",
            ),
            (
                "Show path in title",
                selected_defaults.title_show_path,
                "profile-title-show-path",
            ),
            (
                "Show TTY in title",
                selected_defaults.title_show_tty,
                "profile-title-show-tty",
            ),
            (
                "Show process in title",
                selected_defaults.title_show_process,
                "profile-title-show-process",
            ),
            (
                "Show arguments in title",
                selected_defaults.title_show_arguments,
                "profile-title-show-arguments",
            ),
            (
                "Show dimensions in title",
                selected_defaults.title_show_dimensions,
                "profile-title-show-dimensions",
            ),
            (
                "Show Ctrl key in title",
                selected_defaults.title_show_ctrl_key,
                "profile-title-show-ctrl-key",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let unavailable =
                name == "profile-title-show-tty" || name == "profile-title-show-ctrl-key";
            let button = if unavailable {
                unavailable_check(label, name)
            } else {
                named_check(label, active, name)
            };
            if unavailable {
                button.set_tooltip_text(Some(
                    "The current VTE/Wayland API does not expose this macOS title component.",
                ));
            }
            window_extra.attach(&button, 0, row as i32, 2, 1);
        }
        window_page.0.append(&window_extra);
        profile_stack.add_titled(&scroll_page(&window_page.0), Some("window"), "Window");
        let shell_page = profile_page_with_checks(
            "Shell",
            "Startup command and process-exit behavior for this profile.",
            &[],
        );
        shell_page.0.set_widget_name("shell");
        let run_command = gtk::Entry::new();
        run_command.set_widget_name("profile-shell-command");
        run_command.set_text(&selected_defaults.shell_command);
        run_command.set_placeholder_text(Some("/bin/bash --login -c 'command'"));
        run_command.set_hexpand(true);
        run_command.update_property(&[
            gtk::accessible::Property::Label("Run command"),
            gtk::accessible::Property::Description(
                "Command started for this profile. A blank value inherits the General startup command.",
            ),
        ]);
        shell_page.0.append(&field_label("Run command"));
        shell_page.0.append(&run_command);
        let run_command_inside_shell = check(
            "Run command inside login shell",
            selected_defaults.run_inside_shell,
        );
        run_command_inside_shell.set_widget_name("profile-run-inside-shell");
        run_command_inside_shell.set_sensitive(!selected_defaults.shell_command.trim().is_empty());
        run_command_inside_shell.set_tooltip_text(Some(
            "Applies to this profile's Run command. A blank command uses the General startup command and its shell mode.",
        ));
        let run_mode_for_command = run_command_inside_shell.clone();
        run_command.connect_changed(move |entry| {
            run_mode_for_command.set_sensitive(!entry.text().trim().is_empty());
        });
        shell_page.0.append(&run_command_inside_shell);
        shell_page.0.append(&hint_label(
            "A blank profile command inherits the General startup command. Shell and command changes apply to new tabs.",
        ));
        let profile_shell = gtk::Entry::new();
        profile_shell.set_widget_name("profile-shell");
        profile_shell.set_text(&selected_defaults.shell);
        profile_shell.set_placeholder_text(Some("Optional complete shell path"));
        profile_shell.set_hexpand(true);
        profile_shell.update_property(&[
            gtk::accessible::Property::Label("Login shell"),
            gtk::accessible::Property::Description(
                "Optional absolute shell path used for newly opened tabs.",
            ),
        ]);
        shell_page.0.append(&field_label("Login shell"));
        shell_page.0.append(&profile_shell);
        let close_on_exit = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Never automatically close",
                "After a clean exit",
                "After an error exit",
                "After any exit",
            ])),
            None::<&gtk::Expression>,
        );
        close_on_exit.set_widget_name("profile-close-on-exit");
        close_on_exit.update_property(&[
            gtk::accessible::Property::Label("Automatically close tab"),
            gtk::accessible::Property::Description(
                "Select which child exit status automatically closes the tab before the fallback action is evaluated.",
            ),
        ]);
        close_on_exit.set_selected(match selected_defaults.close_on_exit {
            CloseOnExit::Never => 0,
            CloseOnExit::Clean => 1,
            CloseOnExit::Error => 2,
            CloseOnExit::Always => 3,
        });
        shell_page.0.append(&field_label("Automatically close tab"));
        shell_page.0.append(&close_on_exit);
        let ask_policy = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Never ask",
                "Always ask",
                "Ask for non-exempt processes",
            ])),
            None::<&gtk::Expression>,
        );
        ask_policy.set_widget_name("profile-ask-policy");
        ask_policy.update_property(&[
            gtk::accessible::Property::Label(
                "Ask before terminating a running process",
            ),
            gtk::accessible::Property::Description(
                "Controls confirmation when manually closing a tab or window with a running process.",
            ),
        ]);
        ask_policy.set_selected(match selected_defaults.ask_before_close_policy {
            AskBeforeClosePolicy::Never => 0,
            AskBeforeClosePolicy::Always => 1,
            AskBeforeClosePolicy::NonExempt => 2,
        });
        shell_page
            .0
            .append(&field_label("Ask before terminating a running process"));
        shell_page.0.append(&ask_policy);
        let exceptions = gtk::Entry::new();
        exceptions.set_widget_name("profile-exceptions");
        exceptions.set_placeholder_text(Some("bash, screen, tmux"));
        exceptions.set_text(&selected_defaults.ask_before_close_exceptions.join(", "));
        exceptions.set_hexpand(true);
        exceptions.update_property(&[
            gtk::accessible::Property::Label("Processes that do not require confirmation"),
            gtk::accessible::Property::Description(
                "Comma-separated executable basenames matched exactly and case-sensitively.",
            ),
        ]);
        exceptions.set_sensitive(
            selected_defaults.ask_before_close_policy == AskBeforeClosePolicy::NonExempt,
        );
        exceptions.set_tooltip_text(Some(
            "Comma-separated executable basenames. Matching is exact and case-sensitive.",
        ));
        shell_page
            .0
            .append(&field_label("Processes that do not require confirmation"));
        shell_page.0.append(&exceptions);
        shell_page.0.append(&hint_label(
            "This policy applies when closing a tab or window that still has a running process. Unknown or unverified processes are never exempt.",
        ));
        let exceptions_for_policy = exceptions.clone();
        ask_policy.connect_selected_notify(move |dropdown| {
            exceptions_for_policy.set_sensitive(dropdown.selected() == 2);
        });
        let exit_policy = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Ask after exit",
                "Keep tab open",
                "Close tab",
                "Close window",
            ])),
            None::<&gtk::Expression>,
        );
        exit_policy.set_widget_name("shell-exit-policy");
        exit_policy.update_property(&[
            gtk::accessible::Property::Label("When automatic close does not apply"),
            gtk::accessible::Property::Description(
                "Action used after the child exits when the automatic close rule does not match.",
            ),
        ]);
        exit_policy.set_selected(match selected_defaults.shell_exit_action {
            ShellExitAction::Ask => 0,
            ShellExitAction::Keep => 1,
            ShellExitAction::CloseTab => 2,
            ShellExitAction::CloseWindow => 3,
        });
        shell_page
            .0
            .append(&field_label("When automatic close does not apply"));
        shell_page.0.append(&exit_policy);
        exit_policy.set_sensitive(selected_defaults.close_on_exit != CloseOnExit::Always);
        exit_policy.set_tooltip_text(Some(
            "Used only when the automatic close rule above does not match the child's exit status.",
        ));
        let fallback_for_close = exit_policy.clone();
        close_on_exit.connect_selected_notify(move |dropdown| {
            fallback_for_close.set_sensitive(dropdown.selected() != 3);
        });
        shell_page.0.append(&hint_label(
            "The automatic rule is evaluated first. “Ask after exit” is separate from confirmation before terminating a process.",
        ));
        profile_stack.add_titled(&scroll_page(&shell_page.0), Some("shell"), "Shell");
        let keyboard_page = profile_page_with_checks(
            "Keyboard",
            "Keyboard and pointer behavior for this profile.",
            &[],
        );
        keyboard_page.0.set_widget_name("keyboard");
        let option_meta = named_check(
            "Option/Alt acts as Meta",
            selected_defaults.option_as_meta,
            "profile-option-meta",
        );
        keyboard_page.0.append(&option_meta);
        let alt_scroll = renderer_owned_check(
            "Alternate-screen scrolling (managed by VTE)",
            "profile-alt-scroll",
        );
        keyboard_page.0.append(&alt_scroll);
        let mapping_hint = hint_label(
            "Key mappings (enter chords such as Ctrl+Shift+Right and an encoded PTY sequence)",
        );
        keyboard_page.0.append(&mapping_hint);
        // Standard Ctrl+C/Ctrl+V behavior is implemented by the terminal
        // shortcut layer. Do not synthesize mappings for those keys: doing so
        // would bypass selection-aware copy and clipboard paste.
        let key_mappings = selected_defaults.key_mappings.clone();
        let mapping_strings = key_mappings.iter().map(mapping_display).collect::<Vec<_>>();
        let mapping_refs = mapping_strings
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mappings = gtk::StringList::new(&mapping_refs);
        let mapping_state = Rc::new(RefCell::new(key_mappings));
        let mapping_factory = gtk::SignalListItemFactory::new();
        mapping_factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            item.set_child(Some(&gtk::Label::new(None)));
        });
        mapping_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(value) = item.item().and_downcast::<gtk::StringObject>() else {
                return;
            };
            label.set_text(&value.string());
        });
        let mapping_view = gtk::ListView::new(
            Some(gtk::SingleSelection::new(Some(mappings.clone()))),
            Some(mapping_factory),
        );
        mapping_view.set_widget_name("keyboard-mappings");
        mapping_view.set_vexpand(true);
        mapping_view.set_size_request(-1, 120);
        keyboard_page.0.append(&mapping_view);
        let mapping_actions = gtk::Grid::new();
        mapping_actions.set_widget_name("keyboard-mapping-actions");
        mapping_actions.set_column_homogeneous(true);
        mapping_actions.set_column_spacing(8);
        let mapping_key = gtk::Entry::new();
        mapping_key.set_widget_name("keyboard-mapping-key");
        mapping_key.set_placeholder_text(Some("Key or label, e.g. Right"));
        mapping_key.set_hexpand(true);
        let mapping_action = gtk::Entry::new();
        mapping_action.set_widget_name("keyboard-mapping-action");
        mapping_action.set_placeholder_text(Some("Encoded action, e.g. \\e[1;5C"));
        mapping_action.set_hexpand(true);
        keyboard_page.0.append(&mapping_key);
        keyboard_page.0.append(&mapping_action);
        let add_mapping = gtk::Button::with_label("Add mapping");
        let edit_mapping = gtk::Button::with_label("Edit mapping");
        let remove_mapping = gtk::Button::with_label("Remove mapping");
        for (column, button) in [&add_mapping, &edit_mapping, &remove_mapping]
            .into_iter()
            .enumerate()
        {
            button.set_hexpand(true);
            mapping_actions.attach(button, column as i32, 0, 1, 1);
        }
        let mappings_for_add = mappings.clone();
        let state_for_add = mapping_state.clone();
        let key_for_add = mapping_key.clone();
        let action_for_add = mapping_action.clone();
        add_mapping.connect_clicked(move |_| {
            let chord = key_for_add.text().to_string();
            let action = action_for_add.text().to_string();
            if let (Ok((key, modifiers)), Ok(_)) =
                (parse_key_chord(&chord), decode_key_sequence(&action))
            {
                let mapping = KeyMapping {
                    key,
                    modifiers,
                    action: action.clone(),
                };
                mappings_for_add.append(&mapping_display(&mapping));
                state_for_add.borrow_mut().push(mapping);
                key_for_add.set_text("");
                action_for_add.set_text("");
            }
        });
        let mappings_for_edit = mappings.clone();
        let state_for_edit = mapping_state.clone();
        let key_for_edit = mapping_key.clone();
        let action_for_edit = mapping_action.clone();
        let selection_for_edit = mapping_view.model().and_downcast::<gtk::SingleSelection>();
        edit_mapping.connect_clicked(move |_| {
            let Some(selection) = &selection_for_edit else {
                return;
            };
            let Some(index) = selection.selected().checked_sub(0) else {
                return;
            };
            let chord = key_for_edit.text().to_string();
            let action = action_for_edit.text().to_string();
            if let (Ok((key, modifiers)), Ok(_)) =
                (parse_key_chord(&chord), decode_key_sequence(&action))
            {
                let mapping = KeyMapping {
                    key,
                    modifiers,
                    action,
                };
                if let Some(existing) = state_for_edit.borrow_mut().get_mut(index as usize) {
                    *existing = mapping.clone();
                    let display = mapping_display(&mapping);
                    mappings_for_edit.splice(index, 1, &[display.as_str()]);
                    selection.set_selected(index);
                }
            } else if let Some(mapping) = state_for_edit.borrow().get(index as usize) {
                key_for_edit.set_text(&mapping_chord(mapping));
                action_for_edit.set_text(&mapping.action);
            }
        });
        let mappings_for_remove = mappings.clone();
        let state_for_remove = mapping_state.clone();
        let selection_for_remove = mapping_view.model().and_downcast::<gtk::SingleSelection>();
        remove_mapping.connect_clicked(move |_| {
            if let Some(selection) = &selection_for_remove {
                let index = selection.selected();
                if index != gtk::INVALID_LIST_POSITION {
                    if (index as usize) < state_for_remove.borrow().len() {
                        state_for_remove.borrow_mut().remove(index as usize);
                    }
                    mappings_for_remove.remove(index);
                }
            }
        });
        keyboard_page.0.append(&mapping_actions);
        keyboard_page.0.append(&hint_label(
            "Mappings are validated as bounded PTY byte sequences and saved with this profile.",
        ));
        keyboard_page.0.append(&hint_label("Pointer auto-hide is disabled unconditionally for reliable KVM and screenshot focus transitions."));
        profile_stack.add_titled(&scroll_page(&keyboard_page.0), Some("keyboard"), "Keyboard");

        let advanced = gtk::Box::new(gtk::Orientation::Vertical, 16);
        advanced.set_widget_name("advanced");
        advanced.add_css_class("core-settings-pane");
        page_heading(
            &advanced,
            "Advanced",
            "Protocol identity and VTE-owned terminal behavior.",
        );
        let advanced_grid = form_grid();
        let terminal_type_label = field_label("TERM value");
        advanced_grid.attach(&terminal_type_label, 0, 0, 1, 1);
        let terminal_type = gtk::Entry::new();
        terminal_type.set_widget_name("advanced-terminal-type");
        terminal_type.set_text(&selected_defaults.terminal_type);
        terminal_type.set_placeholder_text(Some("xterm-256color"));
        terminal_type.set_hexpand(true);
        advanced_grid.attach(&terminal_type, 1, 0, 1, 1);
        advanced.append(&advanced_grid);
        for (label, active, name) in [
            (
                "Delete sends Ctrl-H",
                selected_defaults.delete_sends_control_h,
                "profile-delete-control-h",
            ),
            (
                "Escape non-ASCII input",
                selected_defaults.escape_non_ascii,
                "profile-escape-nonascii",
            ),
            (
                "Paste newlines as carriage return",
                selected_defaults.paste_newlines_as_cr,
                "profile-paste-cr",
            ),
            (
                "Application keypad mode",
                selected_defaults.application_keypad,
                "profile-keypad",
            ),
            (
                "Scroll on input",
                selected_defaults.scroll_on_input,
                "profile-scroll-input",
            ),
            (
                "Audible bell",
                selected_defaults.audible_bell,
                "profile-audible-bell",
            ),
            (
                "Visual bell",
                selected_defaults.visual_bell,
                "profile-visual-bell",
            ),
            (
                "Only show visual bell when audible bell is off",
                selected_defaults.visual_bell_only_if_muted,
                "profile-visual-bell-only-muted",
            ),
            (
                "Badge app and window icons with a desktop notification",
                selected_defaults.background_notifications,
                "profile-background-notifications",
            ),
            (
                "Request urgent background attention with a desktop notification",
                selected_defaults.urgency_hint,
                "profile-urgency",
            ),
        ] {
            let button = if name == "profile-keypad" {
                renderer_owned_check(label, name)
            } else {
                named_check(label, active, name)
            };
            advanced.append(&button);
        }
        advanced.append(&unavailable_check(
            "Continue requesting attention until focused (the desktop owns notification lifetime)",
            "profile-continue-urgency",
        ));
        let encoding_grid = form_grid();
        encoding_grid.attach(&field_label("Text encoding"), 0, 0, 1, 1);
        let profile_encoding = gtk::DropDown::new(
            Some(gtk::StringList::new(&["Unicode (UTF-8), supplied by VTE"])),
            None::<&gtk::Expression>,
        );
        profile_encoding.set_widget_name("profile-encoding");
        profile_encoding.set_sensitive(false);
        profile_encoding.set_hexpand(true);
        profile_encoding.set_tooltip_text(Some(
            "Modern VTE exposes UTF-8 only and does not provide a per-session legacy encoding selector.",
        ));
        encoding_grid.attach(&profile_encoding, 1, 0, 1, 1);
        advanced.append(&encoding_grid);
        let locale = gtk::Entry::new();
        locale.set_widget_name("profile-locale");
        locale.set_placeholder_text(Some("System locale (optional)"));
        locale.set_text(&selected_defaults.locale);
        locale.set_hexpand(true);
        advanced.append(&locale);
        advanced.append(&named_check(
            "Set locale environment for child",
            selected_defaults.set_locale_environment,
            "profile-set-locale",
        ));
        let character_grid = form_grid();
        character_grid.attach(&field_label("Ambiguous character width"), 0, 0, 1, 1);
        let ambiguous = gtk::DropDown::new(
            Some(gtk::StringList::new(&["Narrow", "Wide"])),
            None::<&gtk::Expression>,
        );
        ambiguous.set_widget_name("profile-ambiguous-width");
        ambiguous.set_selected((selected_defaults.ambiguous_width.saturating_sub(1)) as u32);
        ambiguous.set_hexpand(true);
        character_grid.attach(&ambiguous, 1, 0, 1, 1);
        advanced.append(&character_grid);
        advanced.append(&hint_label(
            "Modern VTE owns escape-sequence parsing, PTY integration, and UTF-8 handling.",
        ));
        profile_stack.add_titled(&scroll_page(&advanced), Some("advanced"), "Advanced");
        profile_content.append(&profile_stack);
        let reload_stack = profile_stack.clone();
        let reload_store = profile_store.clone();
        let editor_current = Rc::new(RefCell::new(selected_profile_name));
        let editor_current_for_reload = editor_current.clone();
        let reload_mappings = mappings.clone();
        let reload_mapping_state = mapping_state.clone();
        profile_list.connect_row_selected(move |_, row| {
            let Some(name) = row.and_then(profile_list_row_name) else {
                return;
            };
            let previous = editor_current_for_reload.borrow().clone();
            let previous_profile = { reload_store.borrow().profile(&previous).cloned() };
            if let Some(mut profile) = previous_profile {
                read_profile_widgets(&reload_stack, &mut profile, &reload_mapping_state);
                let _ = reload_store.borrow_mut().update_profile(profile);
            }
            *editor_current_for_reload.borrow_mut() = name.clone();
            let selected_profile = { reload_store.borrow().profile(&name).cloned() };
            if let Some(profile) = selected_profile {
                load_profile_widgets(&reload_stack, &profile);
                let mut state = reload_mapping_state.borrow_mut();
                *state = profile.key_mappings.clone();
                let display = state.iter().map(mapping_display).collect::<Vec<_>>();
                let refs = display.iter().map(String::as_str).collect::<Vec<_>>();
                reload_mappings.splice(0, reload_mappings.n_items(), &refs);
            }
        });
        profile_page.append(&profile_content);
        top_stack.add_titled(&profile_page, Some("profiles"), "Profiles");

        let window_groups = gtk::Box::new(gtk::Orientation::Vertical, 18);
        window_groups.set_widget_name("window-groups-page");
        window_groups.add_css_class("core-settings-pane");
        page_heading(
            &window_groups,
            "Window Groups",
            "Window size and group behavior for native Linux windows.",
        );
        let window_grid = form_grid();
        let width_label = field_label("Window width");
        window_grid.attach(&width_label, 0, 0, 1, 1);
        let window_width = gtk::SpinButton::with_range(320.0, 8_000.0, 10.0);
        window_width.set_value(settings.window_width as f64);
        window_grid.attach(&window_width, 1, 0, 1, 1);
        let height_label = field_label("Window height");
        window_grid.attach(&height_label, 0, 1, 1, 1);
        let window_height = gtk::SpinButton::with_range(240.0, 8_000.0, 10.0);
        window_height.set_value(settings.window_height as f64);
        window_grid.attach(&window_height, 1, 1, 1, 1);
        window_groups.append(&window_grid);
        let background_notifications = check(
            "Notify when a background terminal needs attention",
            settings.background_notifications,
        );
        window_groups.append(&background_notifications);
        let group_store = profile_store.clone();
        let selected_group = Rc::new(RefCell::new(None::<String>));
        let group_entries = Rc::new(RefCell::new(Vec::<WindowGroupEntry>::new()));
        let selected_group_entry = Rc::new(RefCell::new(None::<usize>));
        let group_form = form_grid();
        let group_name = gtk::Entry::new();
        group_name.set_widget_name("window-group-name");
        group_name.set_placeholder_text(Some("Work terminals"));
        group_name.set_hexpand(true);
        group_form.attach(&field_label("Name"), 0, 0, 1, 1);
        group_form.attach(&group_name, 1, 0, 1, 1);
        let group_profile = gtk::DropDown::new(
            Some(gtk::StringList::new(
                &profile_names.iter().map(String::as_str).collect::<Vec<_>>(),
            )),
            None::<&gtk::Expression>,
        );
        group_profile.set_widget_name("window-group-profile");
        group_profile.set_hexpand(true);
        group_form.attach(&field_label("Profile"), 0, 1, 1, 1);
        group_form.attach(&group_profile, 1, 1, 1, 1);
        let group_directory = gtk::Entry::new();
        group_directory.set_widget_name("window-group-directory");
        group_directory.set_placeholder_text(Some("Optional working directory"));
        group_directory.set_hexpand(true);
        group_form.attach(&field_label("Working directory"), 0, 2, 1, 1);
        group_form.attach(&group_directory, 1, 2, 1, 1);
        let group_columns = gtk::SpinButton::with_range(1.0, 1_000.0, 1.0);
        group_columns.set_widget_name("window-group-columns");
        group_columns.set_value(80.0);
        let group_rows = gtk::SpinButton::with_range(1.0, 1_000.0, 1.0);
        group_rows.set_widget_name("window-group-rows");
        group_rows.set_value(24.0);
        group_form.attach(&field_label("Columns"), 0, 3, 1, 1);
        group_form.attach(&group_columns, 1, 3, 1, 1);
        group_form.attach(&field_label("Rows"), 0, 4, 1, 1);
        group_form.attach(&group_rows, 1, 4, 1, 1);
        window_groups.append(&group_form);
        let entry_heading = gtk::Label::new(Some("Tabs in selected group"));
        entry_heading.add_css_class("core-settings-section");
        entry_heading.set_halign(gtk::Align::Start);
        window_groups.append(&entry_heading);
        window_groups.append(&hint_label(
            "Each row launches as one tab, in the order shown. Select a row to edit its profile, directory, and terminal size above.",
        ));
        let group_entry_list = gtk::ListBox::new();
        group_entry_list.set_widget_name("window-group-entries");
        group_entry_list.set_selection_mode(gtk::SelectionMode::Single);
        group_entry_list.set_vexpand(true);
        let group_entry_scroll = gtk::ScrolledWindow::new();
        group_entry_scroll.set_widget_name("window-group-entries-scroll");
        group_entry_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        group_entry_scroll.set_min_content_height(150);
        group_entry_scroll.set_vexpand(true);
        group_entry_scroll.set_child(Some(&group_entry_list));
        window_groups.append(&group_entry_scroll);
        let entry_actions = gtk::Grid::new();
        entry_actions.set_widget_name("window-group-entry-actions");
        entry_actions.set_column_homogeneous(true);
        entry_actions.set_column_spacing(8);
        let add_entry = gtk::Button::with_label("Add tab");
        add_entry.set_widget_name("window-group-entry-add");
        let remove_entry = gtk::Button::with_label("Remove tab");
        remove_entry.set_widget_name("window-group-entry-remove");
        let move_entry_up = gtk::Button::with_label("Move up");
        move_entry_up.set_widget_name("window-group-entry-up");
        let move_entry_down = gtk::Button::with_label("Move down");
        move_entry_down.set_widget_name("window-group-entry-down");
        for (index, button) in [&add_entry, &remove_entry, &move_entry_up, &move_entry_down]
            .into_iter()
            .enumerate()
        {
            button.set_hexpand(true);
            entry_actions.attach(button, index as i32, 0, 1, 1);
        }
        window_groups.append(&entry_actions);
        let saved_group_heading = gtk::Label::new(Some("Saved groups"));
        saved_group_heading.add_css_class("core-settings-section");
        saved_group_heading.set_halign(gtk::Align::Start);
        window_groups.append(&saved_group_heading);
        let groups = gtk::ListBox::new();
        groups.set_widget_name("window-groups-list");
        groups.set_selection_mode(gtk::SelectionMode::Single);
        for group in group_store.borrow().window_groups() {
            groups.append(&gtk::Label::new(Some(&group.name)));
        }
        let group_actions = gtk::Grid::new();
        group_actions.set_widget_name("window-group-actions");
        group_actions.set_column_homogeneous(true);
        group_actions.set_column_spacing(8);
        group_actions.set_row_spacing(8);
        let add_group = gtk::Button::with_label("Add group");
        add_group.set_widget_name("window-group-add");
        let remove_group = gtk::Button::with_label("Remove group");
        remove_group.set_widget_name("window-group-remove");
        let launch_group = gtk::Button::with_label("Launch selected group");
        launch_group.set_widget_name("window-group-launch");
        for (index, button) in [&add_group, &remove_group, &launch_group]
            .into_iter()
            .enumerate()
        {
            button.set_hexpand(true);
            group_actions.attach(button, (index % 2) as i32, (index / 2) as i32, 1, 1);
        }
        let group_status = hint_label(
            "Settings Save persists every group edit. Switching groups keeps the current draft in this Settings session.",
        );
        group_status.set_widget_name("window-group-status");

        let commit_window_group: Rc<dyn Fn() -> Result<(), String>> = {
            let store = group_store.clone();
            let selected_group = selected_group.clone();
            let entries = group_entries.clone();
            let selected_entry = selected_group_entry.clone();
            let name = group_name.clone();
            let profile = group_profile.clone();
            let directory = group_directory.clone();
            let columns = group_columns.clone();
            let rows = group_rows.clone();
            let list = groups.clone();
            let startup_model = startup_group_model.clone();
            Rc::new(move || {
                let Some(old_name) = selected_group.borrow().clone() else {
                    return Ok(());
                };
                let new_name = name.text().trim().to_owned();
                if new_name.is_empty() {
                    return Err("Window group name cannot be empty.".into());
                }
                let mut edited_entries = entries.borrow().clone();
                if let Some(index) = *selected_entry.borrow() {
                    let current =
                        window_group_entry_from_widgets(&profile, &directory, &columns, &rows)
                            .ok_or_else(|| "Select a profile for this group entry.".to_owned())?;
                    if let Some(entry) = edited_entries.get_mut(index) {
                        *entry = current;
                    }
                }
                if edited_entries.is_empty() {
                    return Err("A window group must contain at least one tab.".into());
                }
                let group = WindowGroup {
                    name: new_name.clone(),
                    entries: edited_entries,
                };
                let result = store.borrow_mut().rename_window_group(&old_name, group);
                result.map_err(|error| error.to_string())?;
                if old_name != new_name {
                    if let Some(row) = list_box_row_with_label(&list, &old_name) {
                        row.set_child(Some(&gtk::Label::new(Some(&new_name))));
                    }
                    if let Some(index) = string_list_position(&startup_model, &old_name) {
                        startup_model.splice(index, 1, &[new_name.as_str()]);
                    }
                }
                *selected_group.borrow_mut() = Some(new_name);
                Ok(())
            })
        };

        let entries_for_select = group_entries.clone();
        let selected_entry_for_select = selected_group_entry.clone();
        let profile_for_entry_select = group_profile.clone();
        let directory_for_entry_select = group_directory.clone();
        let columns_for_entry_select = group_columns.clone();
        let rows_for_entry_select = group_rows.clone();
        group_entry_list.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                *selected_entry_for_select.borrow_mut() = None;
                return;
            };
            let index = row.index().max(0) as usize;
            let Some(entry) = entries_for_select.borrow().get(index).cloned() else {
                *selected_entry_for_select.borrow_mut() = None;
                return;
            };
            *selected_entry_for_select.borrow_mut() = Some(index);
            if let Some(model) = profile_for_entry_select
                .model()
                .and_downcast::<gtk::StringList>()
            {
                if let Some(profile_index) = string_list_position(&model, &entry.profile) {
                    profile_for_entry_select.set_selected(profile_index);
                }
            }
            directory_for_entry_select.set_text(entry.working_directory.as_deref().unwrap_or(""));
            columns_for_entry_select.set_value(entry.columns as f64);
            rows_for_entry_select.set_value(entry.rows as f64);
        });

        let sync_selected_entry: Rc<dyn Fn()> = {
            let entries = group_entries.clone();
            let selected_entry = selected_group_entry.clone();
            let list = group_entry_list.clone();
            let profile = group_profile.clone();
            let directory = group_directory.clone();
            let columns = group_columns.clone();
            let rows = group_rows.clone();
            Rc::new(move || {
                let Some(index) = *selected_entry.borrow() else {
                    return;
                };
                let Some(entry) =
                    window_group_entry_from_widgets(&profile, &directory, &columns, &rows)
                else {
                    return;
                };
                let mut entries = entries.borrow_mut();
                let Some(current) = entries.get_mut(index) else {
                    return;
                };
                *current = entry.clone();
                replace_window_group_entry_summary(&list, index, &entry);
            })
        };
        let sync_for_profile = sync_selected_entry.clone();
        group_profile.connect_selected_item_notify(move |_| sync_for_profile());
        let sync_for_directory = sync_selected_entry.clone();
        group_directory.connect_changed(move |_| sync_for_directory());
        let sync_for_columns = sync_selected_entry.clone();
        group_columns.connect_value_changed(move |_| sync_for_columns());
        let sync_for_rows = sync_selected_entry.clone();
        group_rows.connect_value_changed(move |_| sync_for_rows());

        let selected_group_for_add_entry = selected_group.clone();
        let entries_for_add_entry = group_entries.clone();
        let selected_for_add_entry = selected_group_entry.clone();
        let entry_list_for_add_entry = group_entry_list.clone();
        let profile_for_add_entry = group_profile.clone();
        let directory_for_add_entry = group_directory.clone();
        let columns_for_add_entry = group_columns.clone();
        let rows_for_add_entry = group_rows.clone();
        add_entry.connect_clicked(move |_| {
            if selected_group_for_add_entry.borrow().is_none() {
                return;
            }
            let Some(entry) = window_group_entry_from_widgets(
                &profile_for_add_entry,
                &directory_for_add_entry,
                &columns_for_add_entry,
                &rows_for_add_entry,
            ) else {
                return;
            };
            let selected = {
                let mut entries = entries_for_add_entry.borrow_mut();
                entries.push(entry);
                entries.len() - 1
            };
            *selected_for_add_entry.borrow_mut() = Some(selected);
            let entries = entries_for_add_entry.borrow().clone();
            rebuild_window_group_entry_list(&entry_list_for_add_entry, &entries, Some(selected));
        });

        let entries_for_remove_entry = group_entries.clone();
        let selected_for_remove_entry = selected_group_entry.clone();
        let entry_list_for_remove_entry = group_entry_list.clone();
        remove_entry.connect_clicked(move |_| {
            let Some(index) = *selected_for_remove_entry.borrow() else {
                return;
            };
            let next = {
                let mut entries = entries_for_remove_entry.borrow_mut();
                if entries.len() <= 1 || index >= entries.len() {
                    return;
                }
                entries.remove(index);
                index.min(entries.len() - 1)
            };
            *selected_for_remove_entry.borrow_mut() = Some(next);
            let entries = entries_for_remove_entry.borrow().clone();
            rebuild_window_group_entry_list(&entry_list_for_remove_entry, &entries, Some(next));
        });

        let entries_for_move_up = group_entries.clone();
        let selected_for_move_up = selected_group_entry.clone();
        let entry_list_for_move_up = group_entry_list.clone();
        move_entry_up.connect_clicked(move |_| {
            let Some(index) = *selected_for_move_up.borrow() else {
                return;
            };
            if index == 0 {
                return;
            }
            entries_for_move_up.borrow_mut().swap(index, index - 1);
            *selected_for_move_up.borrow_mut() = Some(index - 1);
            let entries = entries_for_move_up.borrow().clone();
            rebuild_window_group_entry_list(&entry_list_for_move_up, &entries, Some(index - 1));
        });

        let entries_for_move_down = group_entries.clone();
        let selected_for_move_down = selected_group_entry.clone();
        let entry_list_for_move_down = group_entry_list.clone();
        move_entry_down.connect_clicked(move |_| {
            let Some(index) = *selected_for_move_down.borrow() else {
                return;
            };
            let len = entries_for_move_down.borrow().len();
            if index + 1 >= len {
                return;
            }
            entries_for_move_down.borrow_mut().swap(index, index + 1);
            *selected_for_move_down.borrow_mut() = Some(index + 1);
            let entries = entries_for_move_down.borrow().clone();
            rebuild_window_group_entry_list(&entry_list_for_move_down, &entries, Some(index + 1));
        });

        let group_store_for_select = group_store.clone();
        let selected_group_for_select = selected_group.clone();
        let name_for_select = group_name.clone();
        let entries_for_group_select = group_entries.clone();
        let selected_entry_for_group_select = selected_group_entry.clone();
        let entry_list_for_group_select = group_entry_list.clone();
        let commit_for_group_select = commit_window_group.clone();
        let status_for_group_select = group_status.clone();
        let reverting_group_selection = Rc::new(Cell::new(false));
        let reverting_for_group_select = reverting_group_selection.clone();
        groups.connect_row_selected(move |list, row| {
            let Some(label) = row.and_then(|row| row.child().and_downcast::<gtk::Label>()) else {
                return;
            };
            let name = label.text().to_string();
            let old_name = selected_group_for_select.borrow().clone();
            let reverting = reverting_for_group_select.replace(false);
            if !reverting && old_name.as_deref().is_some_and(|old| old != name) {
                if let Err(error) = commit_for_group_select() {
                    status_for_group_select.set_text(&error);
                    if let Some(old_row) = old_name
                        .as_deref()
                        .and_then(|old| list_box_row_with_label(list, old))
                    {
                        reverting_for_group_select.set(true);
                        list.select_row(Some(&old_row));
                    }
                    return;
                }
            }
            let Some(group) = group_store_for_select.borrow().window_group(&name).cloned() else {
                return;
            };
            *selected_group_for_select.borrow_mut() = Some(name);
            name_for_select.set_text(&group.name);
            *selected_entry_for_group_select.borrow_mut() = None;
            *entries_for_group_select.borrow_mut() = group.entries;
            let entries = entries_for_group_select.borrow().clone();
            rebuild_window_group_entry_list(&entry_list_for_group_select, &entries, Some(0));
            status_for_group_select.set_text(
                "Settings Save persists every group edit. Switching groups keeps the current draft in this Settings session.",
            );
        });
        if let Some(row) = groups.row_at_index(0) {
            groups.select_row(Some(&row));
        }
        let group_list_for_add = groups.clone();
        let group_store_for_add = group_store.clone();
        let profile_selection_for_add = profile_selection.clone();
        let commit_for_add = commit_window_group.clone();
        let status_for_add = group_status.clone();
        let startup_model_for_add = startup_group_model.clone();
        let startup_toggle_for_add = use_startup_group.clone();
        let startup_dropdown_for_add = startup_window_group.clone();
        add_group.connect_clicked(move |_| {
            if let Err(error) = commit_for_add() {
                status_for_add.set_text(&error);
                return;
            }
            let mut index = group_store_for_add.borrow().window_groups().len() + 1;
            let name = loop {
                let candidate = format!("Window Group {index}");
                if group_store_for_add
                    .borrow()
                    .window_group(&candidate)
                    .is_none()
                {
                    break candidate;
                }
                index += 1;
            };
            let profile = profile_selection_for_add.borrow().clone();
            let group = WindowGroup {
                name: name.clone(),
                entries: vec![WindowGroupEntry {
                    profile,
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
            };
            if group_store_for_add
                .borrow_mut()
                .add_window_group(group)
                .is_ok()
            {
                group_list_for_add.append(&gtk::Label::new(Some(&name)));
                if startup_model_for_add.n_items() == 1
                    && startup_model_for_add.string(0).as_deref() == Some("No groups saved")
                {
                    startup_model_for_add.remove(0);
                }
                if string_list_position(&startup_model_for_add, &name).is_none() {
                    startup_model_for_add.append(&name);
                }
                startup_dropdown_for_add.set_sensitive(startup_toggle_for_add.is_active());
                if let Some(row) = group_list_for_add
                    .row_at_index(group_list_for_add.observe_children().n_items() as i32 - 1)
                {
                    group_list_for_add.select_row(Some(&row));
                }
            }
        });
        let group_list_for_remove = groups.clone();
        let group_store_for_remove = group_store.clone();
        let selected_for_remove = selected_group.clone();
        let entries_for_remove_group = group_entries.clone();
        let selected_entry_for_remove_group = selected_group_entry.clone();
        let entry_list_for_remove_group = group_entry_list.clone();
        let startup_model_for_remove = startup_group_model.clone();
        let startup_toggle_for_remove = use_startup_group.clone();
        let startup_dropdown_for_remove = startup_window_group.clone();
        let commit_for_remove = commit_window_group.clone();
        let status_for_remove = group_status.clone();
        remove_group.connect_clicked(move |_| {
            if let Err(error) = commit_for_remove() {
                status_for_remove.set_text(&error);
                return;
            }
            let Some(removed_name) = selected_for_remove.borrow().clone() else {
                return;
            };
            let Some(row) = list_box_row_with_label(&group_list_for_remove, &removed_name) else {
                status_for_remove.set_text("The selected window group is no longer available.");
                return;
            };
            let removed_startup_group = dropdown_text(&startup_dropdown_for_remove).as_deref()
                == Some(removed_name.as_str());
            if group_store_for_remove
                .borrow_mut()
                .delete_window_group(&removed_name)
                .is_ok()
            {
                *selected_for_remove.borrow_mut() = None;
                *selected_entry_for_remove_group.borrow_mut() = None;
                entries_for_remove_group.borrow_mut().clear();
                rebuild_window_group_entry_list(&entry_list_for_remove_group, &[], None);
                if let Some(index) = string_list_position(&startup_model_for_remove, &removed_name)
                {
                    startup_model_for_remove.remove(index);
                }
                if removed_startup_group || startup_model_for_remove.n_items() == 0 {
                    startup_toggle_for_remove.set_active(false);
                    startup_dropdown_for_remove.set_sensitive(false);
                }
                if startup_model_for_remove.n_items() == 0 {
                    startup_model_for_remove.append("No groups saved");
                }
                group_list_for_remove.remove(&row);
                if let Some(next) = group_list_for_remove.row_at_index(0) {
                    group_list_for_remove.select_row(Some(&next));
                }
            }
        });
        let launch_callback = on_launch_group.clone();
        let launch_store = group_store.clone();
        let launch_selection = selected_group.clone();
        let commit_for_launch = commit_window_group.clone();
        let status_for_launch = group_status.clone();
        launch_group.connect_clicked(move |_| {
            if let Err(error) = commit_for_launch() {
                status_for_launch.set_text(&error);
                return;
            }
            let Some(name) = launch_selection.borrow().clone() else {
                return;
            };
            if let Some(group) = launch_store.borrow().window_group(&name).cloned() {
                launch_callback(group);
            }
        });
        let groups_scroll = gtk::ScrolledWindow::new();
        groups_scroll.set_widget_name("window-groups-scroll");
        groups_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        groups_scroll.set_min_content_height(120);
        groups_scroll.set_child(Some(&groups));
        window_groups.append(&groups_scroll);
        window_groups.append(&group_actions);
        window_groups.append(&group_status);
        window_groups.append(&hint_label("Core Terminal stores each group's profile, directory, and grid geometry. Launch opens one tab per entry; the Linux compositor owns final window placement and grouping."));
        top_stack.add_titled(
            &scroll_page(&window_groups),
            Some("window-groups"),
            "Window Groups",
        );

        let encoding = gtk::Box::new(gtk::Orientation::Vertical, 18);
        encoding.set_widget_name("encoding-compatibility-list");
        encoding.add_css_class("core-settings-pane");
        page_heading(
            &encoding,
            "Encoding",
            "Text encoding behavior supplied by modern VTE.",
        );
        encoding.append(&hint_label("Core Terminal uses UTF-8 internally through VTE. Terminal programs may negotiate their own protocol behavior, but modern VTE does not expose the legacy per-session encoding menu used by older terminals."));
        encoding.append(&hint_label(
            "Set TERM on Profiles → Advanced when a program needs a specific terminal description.",
        ));
        let compatibility = gtk::ListBox::new();
        compatibility.set_widget_name("encoding-options");
        compatibility.set_selection_mode(gtk::SelectionMode::None);
        for name in [
            "Unicode (UTF-16)",
            "Unicode (UTF-7)",
            "Unicode (UTF-8)",
            "Unicode (UTF-32)",
            "Unicode (UTF-16BE)",
            "Unicode (UTF-16LE)",
            "Unicode (UTF-32BE)",
            "Unicode (UTF-32LE)",
            "Western (Mac OS Roman)",
            "Western (ISO Latin 1)",
            "Western (ISO Latin 3)",
            "Western (ISO Latin 9)",
            "Latin-US (DOS)",
            "Western (DOS Latin 1)",
            "Portuguese (DOS)",
            "Canadian French (DOS)",
            "Western (Windows Latin 1)",
            "Western (ASCII)",
            "Western (Mac Mail)",
            "Western (NextStep)",
            "Western (EBCDIC Latin Core)",
            "Western (EBCDIC Latin 1)",
            "Japanese (Mac OS)",
            "Japanese (Windows, DOS)",
            "Japanese (Shift JIS X0213)",
            "Japanese (ISO 2022-JP)",
        ] {
            let row = gtk::ListBoxRow::new();
            let enabled = name == "Unicode (UTF-8)";
            let button = gtk::CheckButton::with_label(name);
            button.set_active(enabled);
            button.set_sensitive(false);
            button.set_halign(gtk::Align::Start);
            button.set_margin_top(6);
            button.set_margin_bottom(6);
            button.set_margin_start(12);
            button.set_tooltip_text(Some(if enabled {
                "UTF-8 is active and supplied by modern VTE."
            } else {
                "Modern VTE does not expose this legacy encoding."
            }));
            row.set_child(Some(&button));
            compatibility.append(&row);
        }
        encoding.append(&compatibility);
        let encoding_actions = gtk::Grid::new();
        encoding_actions.set_widget_name("encoding-actions");
        encoding_actions.set_column_homogeneous(true);
        encoding_actions.set_column_spacing(8);
        for (column, label) in ["Enable All", "Disable All", "Revert to Defaults"]
            .into_iter()
            .enumerate()
        {
            let button = gtk::Button::with_label(label);
            button.set_sensitive(false);
            button.set_hexpand(true);
            button.set_tooltip_text(Some("Modern VTE fixes terminal input and output to UTF-8."));
            encoding_actions.attach(&button, column as i32, 0, 1, 1);
        }
        encoding.append(&encoding_actions);
        let encoding_frame = gtk::Frame::new(Some("Encoding policy"));
        encoding_frame.set_child(Some(&hint_label("UTF-8 is always enabled internally by modern VTE. Legacy encoding menus are intentionally not exposed.")));
        encoding.append(&encoding_frame);
        top_stack.add_titled(&scroll_page(&encoding), Some("encodings"), "Encodings");

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        footer.set_halign(gtk::Align::End);
        footer.set_margin_top(12);
        footer.set_margin_bottom(16);
        footer.set_margin_end(20);
        let cancel_button = gtk::Button::with_label("Cancel");
        cancel_button.set_widget_name("settings-cancel");
        let save_button = gtk::Button::with_label("Save");
        save_button.set_widget_name("settings-save");
        cancel_button.add_css_class("core-settings-action");
        save_button.add_css_class("core-settings-action");
        save_button.add_css_class("suggested-action");
        footer.append(&cancel_button);
        footer.append(&save_button);

        // Keep the pre-0.2 global fallbacks stable without showing duplicate
        // profile controls. The visible Text and Advanced controls own these
        // behaviors for each profile.
        let legacy_scroll_on_output = check("", settings.scroll_on_output);
        let legacy_scroll_on_input = check("", settings.scroll_on_input);
        let legacy_audible_bell = check("", settings.audible_bell);
        let legacy_bold_is_bright = check("", settings.bold_is_bright);

        Self {
            footer,
            save_button,
            cancel_button,
            startup_profile: startup,
            use_startup_group,
            startup_window_group,
            new_window_profile,
            new_tab_profile,
            font,
            font_size,
            cursor_shape,
            cursor_blink,
            scrollback,
            window_width,
            window_height,
            use_custom_command,
            custom_command,
            shell,
            run_command_inside_shell,
            profile_shell_command: run_command,
            profile_close_on_exit: close_on_exit,
            profile_ask_policy: ask_policy,
            profile_exceptions: exceptions,
            profile_exit_action: exit_policy,
            new_tab_same_directory,
            new_window_same_directory,
            ctrl_number_tabs,
            scroll_on_output: legacy_scroll_on_output,
            scroll_on_input: legacy_scroll_on_input,
            audible_bell: legacy_audible_bell,
            bold_is_bright: legacy_bold_is_bright,
            background_notifications,
            profile_add: profile_buttons[0].clone(),
            profile_duplicate: profile_buttons[1].clone(),
            profile_delete: profile_buttons[2].clone(),
            profile_import: profile_buttons[3].clone(),
            profile_export: profile_buttons[4].clone(),
            profile_default: profile_buttons[5].clone(),
            profile_reset: profile_buttons[6].clone(),
            profile_store,
            profile_selection,
            profile_stack,
            profile_mappings: mapping_state,
            profile_list,
            window_group_profile: group_profile,
            commit_window_group,
        }
    }
}

fn named_check(label: &str, active: bool, name: &str) -> gtk::CheckButton {
    let button = check(label, active);
    button.set_widget_name(name);
    button
}

fn renderer_owned_check(label: &str, name: &str) -> gtk::CheckButton {
    let button = named_check(label, true, name);
    button.set_sensitive(false);
    button.set_tooltip_text(Some(
        "This behavior is always enabled by the installed VTE renderer.",
    ));
    button
}

fn unavailable_check(label: &str, name: &str) -> gtk::CheckButton {
    let button = named_check(label, false, name);
    button.set_sensitive(false);
    button.set_tooltip_text(Some(
        "This macOS behavior has no safe native Wayland equivalent.",
    ));
    button
}

fn mapping_display(mapping: &KeyMapping) -> String {
    format!("{} — {}", mapping_chord(mapping), mapping.action)
}

fn mapping_chord(mapping: &KeyMapping) -> String {
    if mapping.modifiers.is_empty() {
        mapping.key.clone()
    } else {
        format!("{}+{}", mapping.modifiers.join("+"), mapping.key)
    }
}

fn profile_widget(stack: &gtk::Stack, name: &str) -> Option<gtk::Widget> {
    find_widget_by_name(&stack.clone().upcast::<gtk::Widget>(), name)
}

fn profile_check(stack: &gtk::Stack, name: &str) -> Option<bool> {
    profile_widget(stack, name)
        .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
        .map(|button| button.is_active())
}

fn profile_entry(stack: &gtk::Stack, name: &str) -> Option<String> {
    profile_widget(stack, name)
        .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
        .map(|entry| entry.text().to_string())
}

fn profile_spin(stack: &gtk::Stack, name: &str) -> Option<f64> {
    profile_widget(stack, name)
        .and_then(|widget| widget.downcast::<gtk::SpinButton>().ok())
        .map(|spin| spin.value())
}

fn profile_dropdown(stack: &gtk::Stack, name: &str) -> Option<u32> {
    profile_widget(stack, name)
        .and_then(|widget| widget.downcast::<gtk::DropDown>().ok())
        .map(|dropdown| dropdown.selected())
}

fn profile_color(stack: &gtk::Stack, name: &str) -> Option<String> {
    profile_widget(stack, name)
        .and_then(|widget| widget.downcast::<gtk::ColorDialogButton>().ok())
        .map(|button| {
            let color = button.rgba();
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                (color.red().clamp(0.0, 1.0) * 255.0).round() as u8,
                (color.green().clamp(0.0, 1.0) * 255.0).round() as u8,
                (color.blue().clamp(0.0, 1.0) * 255.0).round() as u8,
                (color.alpha().clamp(0.0, 1.0) * 255.0).round() as u8,
            )
        })
}

fn read_profile_widgets(
    stack: &gtk::Stack,
    profile: &mut TerminalProfile,
    mappings: &Rc<RefCell<Vec<KeyMapping>>>,
) {
    if let Some(value) = profile_entry(stack, "profile-font") {
        profile.font = value;
    }
    if let Some(value) = profile_spin(stack, "profile-font-size") {
        profile.font_size = value;
    }
    if let Some(value) = profile_dropdown(stack, "profile-cursor-shape") {
        profile.cursor_shape = match value {
            1 => CursorShape::IBeam,
            2 => CursorShape::Underline,
            _ => CursorShape::Block,
        };
    }
    if let Some(value) = profile_check(stack, "profile-cursor-blink") {
        profile.cursor_blink = value;
    }
    if let Some(value) = profile_spin(stack, "profile-scrollback") {
        profile.scrollback_lines = value as u32;
    }
    for (name, destination) in [
        ("profile-background-color", &mut profile.background),
        ("profile-foreground-color", &mut profile.foreground),
        ("profile-bold-color", &mut profile.bold_color),
        ("profile-selection-color", &mut profile.selection),
        ("profile-cursor-color", &mut profile.cursor),
    ] {
        if let Some(value) = profile_color(stack, name) {
            *destination = value;
        }
    }
    for (name, destination) in [
        (
            "profile-title-show-working-directory",
            &mut profile.title_show_working_directory,
        ),
        ("profile-title-show-path", &mut profile.title_show_path),
        (
            "profile-title-show-process",
            &mut profile.title_show_process,
        ),
        (
            "profile-title-show-arguments",
            &mut profile.title_show_arguments,
        ),
        (
            "profile-title-show-dimensions",
            &mut profile.title_show_dimensions,
        ),
        (
            "profile-tab-show-process",
            &mut profile.tab_title_show_process,
        ),
        (
            "profile-tab-show-arguments",
            &mut profile.tab_title_show_arguments,
        ),
        ("profile-tab-show-path", &mut profile.tab_title_show_path),
        (
            "profile-tab-show-dimensions",
            &mut profile.tab_title_show_dimensions,
        ),
        (
            "profile-tab-show-other-items",
            &mut profile.tab_title_show_other_items,
        ),
    ] {
        if let Some(value) = profile_check(stack, name) {
            *destination = value;
        }
    }
    if let Some(value) = profile_spin(stack, "profile-background-alpha") {
        profile.background_alpha = value;
    }
    let mut palette = Vec::with_capacity(profile.ansi_palette.len());
    for index in 0..profile.ansi_palette.len() {
        let name = format!("profile-palette-{index}");
        palette.push(
            profile_color(stack, &name).unwrap_or_else(|| profile.ansi_palette[index].clone()),
        );
    }
    profile.ansi_palette = palette;
    for (name, destination) in [
        ("profile-text-blink", &mut profile.text_blink),
        ("profile-ansi-bright", &mut profile.bold_is_bright),
        (
            "profile-tab-show-profile",
            &mut profile.tab_title_show_profile,
        ),
        ("profile-tab-show-shell", &mut profile.tab_title_show_shell),
        (
            "profile-tab-show-directory",
            &mut profile.tab_title_show_directory,
        ),
        ("profile-tab-show-job", &mut profile.tab_title_show_job),
        ("profile-tab-activity", &mut profile.tab_title_show_activity),
        (
            "profile-title-show-profile",
            &mut profile.title_show_profile,
        ),
        ("profile-title-show-shell", &mut profile.title_show_shell),
        (
            "profile-title-show-directory",
            &mut profile.title_show_directory,
        ),
        (
            "profile-unlimited-scrollback",
            &mut profile.scrollback_unlimited,
        ),
        ("profile-run-inside-shell", &mut profile.run_inside_shell),
        ("profile-option-meta", &mut profile.option_as_meta),
        (
            "profile-delete-control-h",
            &mut profile.delete_sends_control_h,
        ),
        ("profile-escape-nonascii", &mut profile.escape_non_ascii),
        ("profile-paste-cr", &mut profile.paste_newlines_as_cr),
        ("profile-scroll-input", &mut profile.scroll_on_input),
        ("profile-audible-bell", &mut profile.audible_bell),
        ("profile-visual-bell", &mut profile.visual_bell),
        (
            "profile-visual-bell-only-muted",
            &mut profile.visual_bell_only_if_muted,
        ),
        (
            "profile-background-notifications",
            &mut profile.background_notifications,
        ),
        ("profile-urgency", &mut profile.urgency_hint),
        ("profile-set-locale", &mut profile.set_locale_environment),
    ] {
        if let Some(value) = profile_check(stack, name) {
            *destination = value;
        }
    }
    if let Some(value) = profile_entry(stack, "profile-window-title") {
        profile.custom_window_title = value;
    }
    if let Some(value) = profile_entry(stack, "profile-background-image") {
        profile.background_image_path = (!value.trim().is_empty()).then_some(value);
    }
    if let Some(value) = profile_dropdown(stack, "profile-background-mode") {
        profile.background_image_mode = match value {
            1 => BackgroundImageMode::Scale,
            2 => BackgroundImageMode::Center,
            _ => BackgroundImageMode::Tile,
        };
    }
    if let Some(value) = profile_entry(stack, "profile-custom-tab-title") {
        profile.custom_tab_title = value;
    }
    if let Some(value) = profile_entry(stack, "profile-shell-command") {
        profile.shell_command = value;
    }
    if let Some(value) = profile_entry(stack, "profile-shell") {
        profile.shell = value;
    }
    if let Some(value) = profile_entry(stack, "profile-exceptions") {
        profile.ask_before_close_exceptions = value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if let Some(value) = profile_entry(stack, "profile-locale") {
        profile.locale = value;
    }
    if let Some(value) = profile_entry(stack, "advanced-terminal-type") {
        profile.terminal_type = value;
    }
    if let Some(value) = profile_spin(stack, "profile-columns") {
        profile.columns = value as u32;
    }
    if let Some(value) = profile_spin(stack, "profile-rows") {
        profile.rows = value as u32;
    }
    if let Some(value) = profile_spin(stack, "profile-scrollback-limit") {
        profile.scrollback_limit = value as u32;
    }
    if let Some(value) = profile_dropdown(stack, "profile-close-on-exit") {
        profile.close_on_exit = match value {
            1 => CloseOnExit::Clean,
            2 => CloseOnExit::Error,
            3 => CloseOnExit::Always,
            _ => CloseOnExit::Never,
        };
        profile.close_on_clean_exit = false;
        profile.close_on_error = false;
    }
    if let Some(value) = profile_dropdown(stack, "profile-ask-policy") {
        profile.ask_before_close_policy = match value {
            1 => AskBeforeClosePolicy::Always,
            2 => AskBeforeClosePolicy::NonExempt,
            _ => AskBeforeClosePolicy::Never,
        };
    }
    if let Some(value) = profile_dropdown(stack, "shell-exit-policy") {
        profile.shell_exit_action = match value {
            1 => ShellExitAction::Keep,
            2 => ShellExitAction::CloseTab,
            3 => ShellExitAction::CloseWindow,
            _ => ShellExitAction::Ask,
        };
    }
    if let Some(value) = profile_dropdown(stack, "profile-ambiguous-width") {
        profile.ambiguous_width = value.saturating_add(1) as u8;
    }
    profile.tab_title_policy = if profile_check(stack, "profile-tab-custom").unwrap_or(false) {
        crate::profiles::TabTitlePolicy::Custom
    } else {
        crate::profiles::TabTitlePolicy::Components
    };
    // Do not parse the user-facing display strings here. Encoded actions can
    // contain arbitrary separators and must remain byte-for-byte intact.
    profile.key_mappings = mappings.borrow().clone();
}

fn load_profile_widgets(stack: &gtk::Stack, profile: &TerminalProfile) {
    if let Some(widget) =
        profile_widget(stack, "profile-font").and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.font);
    }
    if let Some(widget) = profile_widget(stack, "profile-font-size")
        .and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.font_size);
    }
    if let Some(widget) = profile_widget(stack, "profile-cursor-blink")
        .and_then(|w| w.downcast::<gtk::CheckButton>().ok())
    {
        widget.set_active(profile.cursor_blink);
    }
    if let Some(widget) = profile_widget(stack, "profile-scrollback")
        .and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.scrollback_lines as f64);
    }
    if let Some(widget) = profile_widget(stack, "profile-background-alpha")
        .and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.background_alpha);
    }
    for (name, value) in [
        ("profile-background-color", &profile.background),
        ("profile-foreground-color", &profile.foreground),
        ("profile-bold-color", &profile.bold_color),
        ("profile-selection-color", &profile.selection),
        ("profile-cursor-color", &profile.cursor),
    ] {
        if let (Some(widget), Ok(color)) = (
            profile_widget(stack, name).and_then(|w| w.downcast::<gtk::ColorDialogButton>().ok()),
            gtk::gdk::RGBA::parse(value),
        ) {
            widget.set_rgba(&color);
        }
    }
    if let Some(widget) =
        profile_widget(stack, "profile-window-title").and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.custom_window_title);
    }
    if let Some(widget) = profile_widget(stack, "profile-background-image")
        .and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(profile.background_image_path.as_deref().unwrap_or(""));
    }
    if let Some(widget) = profile_widget(stack, "profile-background-mode")
        .and_then(|w| w.downcast::<gtk::DropDown>().ok())
    {
        widget.set_selected(match profile.background_image_mode {
            BackgroundImageMode::Tile => 0,
            BackgroundImageMode::Scale => 1,
            BackgroundImageMode::Center => 2,
        });
    }
    if let Some(widget) = profile_widget(stack, "profile-custom-tab-title")
        .and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.custom_tab_title);
    }
    if let Some(widget) =
        profile_widget(stack, "profile-shell-command").and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.shell_command);
    }
    if let Some(widget) =
        profile_widget(stack, "profile-shell").and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.shell);
    }
    if let Some(widget) =
        profile_widget(stack, "profile-exceptions").and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.ask_before_close_exceptions.join(", "));
    }
    if let Some(widget) =
        profile_widget(stack, "profile-locale").and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.locale);
    }
    if let Some(widget) = profile_widget(stack, "advanced-terminal-type")
        .and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.terminal_type);
    }
    if let Some(widget) =
        profile_widget(stack, "profile-columns").and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.columns as f64);
    }
    if let Some(widget) =
        profile_widget(stack, "profile-rows").and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.rows as f64);
    }
    if let Some(widget) = profile_widget(stack, "profile-restore-rows-limit")
        .and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.restore_rows_limit as f64);
    }
    if let Some(widget) = profile_widget(stack, "profile-restore-bookmark")
        .and_then(|w| w.downcast::<gtk::Entry>().ok())
    {
        widget.set_text(&profile.restore_rows_bookmark);
    }
    if let Some(widget) = profile_widget(stack, "profile-scrollback-limit")
        .and_then(|w| w.downcast::<gtk::SpinButton>().ok())
    {
        widget.set_value(profile.scrollback_limit as f64);
    }
    if let Some(widget) = profile_widget(stack, "profile-ambiguous-width")
        .and_then(|w| w.downcast::<gtk::DropDown>().ok())
    {
        widget.set_selected(profile.ambiguous_width.saturating_sub(1) as u32);
    }
    if let Some(widget) = profile_widget(stack, "profile-cursor-shape")
        .and_then(|w| w.downcast::<gtk::DropDown>().ok())
    {
        widget.set_selected(match profile.cursor_shape {
            CursorShape::Block => 0,
            CursorShape::IBeam => 1,
            CursorShape::Underline => 2,
        });
    }
    if let Some(widget) = profile_widget(stack, "profile-close-on-exit")
        .and_then(|w| w.downcast::<gtk::DropDown>().ok())
    {
        widget.set_selected(match profile.close_on_exit {
            CloseOnExit::Never => 0,
            CloseOnExit::Clean => 1,
            CloseOnExit::Error => 2,
            CloseOnExit::Always => 3,
        });
    }
    if let Some(widget) =
        profile_widget(stack, "profile-ask-policy").and_then(|w| w.downcast::<gtk::DropDown>().ok())
    {
        widget.set_selected(match profile.ask_before_close_policy {
            AskBeforeClosePolicy::Never => 0,
            AskBeforeClosePolicy::Always => 1,
            AskBeforeClosePolicy::NonExempt => 2,
        });
    }
    if let Some(widget) =
        profile_widget(stack, "shell-exit-policy").and_then(|w| w.downcast::<gtk::DropDown>().ok())
    {
        widget.set_selected(match profile.shell_exit_action {
            ShellExitAction::Ask => 0,
            ShellExitAction::Keep => 1,
            ShellExitAction::CloseTab => 2,
            ShellExitAction::CloseWindow => 3,
        });
    }
    for index in 0..profile.ansi_palette.len() {
        let name = format!("profile-palette-{index}");
        if let (Some(widget), Ok(color)) = (
            profile_widget(stack, &name).and_then(|w| w.downcast::<gtk::ColorDialogButton>().ok()),
            gtk::gdk::RGBA::parse(&profile.ansi_palette[index]),
        ) {
            widget.set_rgba(&color);
        }
    }
    for (name, value) in [
        ("profile-text-blink", profile.text_blink),
        ("profile-ansi-bright", profile.bold_is_bright),
        ("profile-tab-show-profile", profile.tab_title_show_profile),
        ("profile-tab-show-shell", profile.tab_title_show_shell),
        (
            "profile-tab-show-directory",
            profile.tab_title_show_directory,
        ),
        ("profile-tab-show-job", profile.tab_title_show_job),
        (
            "profile-tab-custom",
            profile.tab_title_policy == crate::profiles::TabTitlePolicy::Custom,
        ),
        ("profile-tab-activity", profile.tab_title_show_activity),
        ("profile-tab-show-process", profile.tab_title_show_process),
        (
            "profile-tab-show-arguments",
            profile.tab_title_show_arguments,
        ),
        ("profile-tab-show-path", profile.tab_title_show_path),
        (
            "profile-tab-show-dimensions",
            profile.tab_title_show_dimensions,
        ),
        (
            "profile-tab-show-other-items",
            profile.tab_title_show_other_items,
        ),
        ("profile-title-show-profile", profile.title_show_profile),
        ("profile-title-show-shell", profile.title_show_shell),
        ("profile-title-show-directory", profile.title_show_directory),
        (
            "profile-title-show-working-directory",
            profile.title_show_working_directory,
        ),
        ("profile-title-show-path", profile.title_show_path),
        ("profile-title-show-process", profile.title_show_process),
        ("profile-title-show-arguments", profile.title_show_arguments),
        (
            "profile-title-show-dimensions",
            profile.title_show_dimensions,
        ),
        ("profile-unlimited-scrollback", profile.scrollback_unlimited),
        ("profile-run-inside-shell", profile.run_inside_shell),
        ("profile-option-meta", profile.option_as_meta),
        ("profile-delete-control-h", profile.delete_sends_control_h),
        ("profile-escape-nonascii", profile.escape_non_ascii),
        ("profile-paste-cr", profile.paste_newlines_as_cr),
        ("profile-scroll-input", profile.scroll_on_input),
        ("profile-audible-bell", profile.audible_bell),
        ("profile-visual-bell", profile.visual_bell),
        (
            "profile-visual-bell-only-muted",
            profile.visual_bell_only_if_muted,
        ),
        (
            "profile-background-notifications",
            profile.background_notifications,
        ),
        ("profile-urgency", profile.urgency_hint),
        ("profile-set-locale", profile.set_locale_environment),
    ] {
        if let Some(widget) =
            profile_widget(stack, name).and_then(|w| w.downcast::<gtk::CheckButton>().ok())
        {
            widget.set_active(value);
        }
    }
    sync_profile_control_sensitivity(stack, profile);
}

fn sync_profile_control_sensitivity(stack: &gtk::Stack, profile: &TerminalProfile) {
    if let Some(widget) = profile_widget(stack, "profile-run-inside-shell")
        .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
    {
        widget.set_sensitive(!profile.shell_command.trim().is_empty());
    }
    if let Some(widget) = profile_widget(stack, "profile-exceptions")
        .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
    {
        widget.set_sensitive(profile.ask_before_close_policy == AskBeforeClosePolicy::NonExempt);
    }
    if let Some(widget) = profile_widget(stack, "shell-exit-policy")
        .and_then(|widget| widget.downcast::<gtk::DropDown>().ok())
    {
        widget.set_sensitive(profile.close_on_exit != CloseOnExit::Always);
    }
}

fn form_grid() -> gtk::Grid {
    let grid = gtk::Grid::new();
    grid.set_column_spacing(18);
    grid.set_row_spacing(12);
    grid
}

fn profile_list_row(name: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("core-profile-row");
    row.set_selectable(true);
    row.set_activatable(true);
    row.set_tooltip_text(Some(name));

    let label = gtk::Label::new(Some(name));
    label.add_css_class("core-profile-row-label");
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_tooltip_text(Some(name));
    row.set_child(Some(&label));
    row
}

fn profile_list_row_name(row: &gtk::ListBoxRow) -> Option<String> {
    row.child()
        .and_downcast::<gtk::Label>()
        .map(|label| label.text().to_string())
        .filter(|name| !name.is_empty())
}

fn list_box_row_with_label(list: &gtk::ListBox, text: &str) -> Option<gtk::ListBoxRow> {
    let mut child = list.first_child();
    while let Some(widget) = child {
        let next = widget.next_sibling();
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
            if row
                .child()
                .and_downcast::<gtk::Label>()
                .is_some_and(|label| label.text() == text)
            {
                return Some(row.clone());
            }
        }
        child = next;
    }
    None
}

fn scroll_page<W: IsA<gtk::Widget>>(child: &W) -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::new();
    // Settings pages may grow vertically, but horizontal wheel input must
    // never slide navigation labels or controls out of view. Layouts wrap or
    // establish their own minimum width instead.
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);
    scroller.set_child(Some(child));
    scroller
}

fn dropdown_value(dropdown: &gtk::DropDown, fallback: &str) -> String {
    let value = dropdown
        .selected_item()
        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
        .map(|item| item.string().to_string())
        .unwrap_or_else(|| fallback.to_owned());
    match value.as_str() {
        "Default profile" | "Startup profile" => "default".into(),
        "Same as startup" | "Same as current tab" => "same".into(),
        _ => value,
    }
}

fn dropdown_text(dropdown: &gtk::DropDown) -> Option<String> {
    dropdown
        .selected_item()
        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
        .map(|item| item.string().to_string())
}

fn string_list_position(model: &gtk::StringList, value: &str) -> Option<u32> {
    (0..model.n_items()).find(|index| model.string(*index).as_deref() == Some(value))
}

fn startup_profile_after_deletion(
    startup_before_delete: Option<&str>,
    deleted_profile: &str,
    default_after_delete: &str,
) -> String {
    startup_before_delete
        .filter(|name| *name != deleted_profile)
        .unwrap_or(default_after_delete)
        .to_owned()
}

fn append_dropdown_value(dropdown: &gtk::DropDown, value: &str) {
    let Some(model) = dropdown.model().and_downcast::<gtk::StringList>() else {
        return;
    };
    if string_list_position(&model, value).is_none() {
        model.append(value);
    }
}

fn unique_profile_name(store: &ProfileStore, prefix: &str) -> String {
    (1_u32..)
        .map(|index| format!("{prefix} {index}"))
        .find(|candidate| store.profile(candidate).is_none())
        .expect("a unique generated profile name is available")
}

fn remove_dropdown_value(dropdown: &gtk::DropDown, value: &str) {
    let Some(model) = dropdown.model().and_downcast::<gtk::StringList>() else {
        return;
    };
    let Some(index) = string_list_position(&model, value) else {
        return;
    };
    let was_selected = dropdown.selected() == index;
    model.remove(index);
    if was_selected {
        dropdown.set_selected(0);
    }
}

fn window_group_entry_from_widgets(
    profile: &gtk::DropDown,
    directory: &gtk::Entry,
    columns: &gtk::SpinButton,
    rows: &gtk::SpinButton,
) -> Option<WindowGroupEntry> {
    Some(WindowGroupEntry {
        profile: dropdown_text(profile)?,
        working_directory: (!directory.text().trim().is_empty())
            .then(|| directory.text().trim().to_owned()),
        columns: columns.value() as u32,
        rows: rows.value() as u32,
    })
}

fn window_group_entry_summary(index: usize, entry: &WindowGroupEntry) -> String {
    let directory = entry
        .working_directory
        .as_deref()
        .unwrap_or("Home directory");
    format!(
        "Tab {}: {} — {} — {}×{}",
        index + 1,
        entry.profile,
        directory,
        entry.columns,
        entry.rows
    )
}

fn window_group_entry_row(index: usize, entry: &WindowGroupEntry) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(true);
    row.set_activatable(true);
    let label = gtk::Label::new(Some(&window_group_entry_summary(index, entry)));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_tooltip_text(Some(&window_group_entry_summary(index, entry)));
    row.set_child(Some(&label));
    row
}

fn rebuild_window_group_entry_list(
    list: &gtk::ListBox,
    entries: &[WindowGroupEntry],
    selected: Option<usize>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for (index, entry) in entries.iter().enumerate() {
        list.append(&window_group_entry_row(index, entry));
    }
    if let Some(row) = selected.and_then(|index| list.row_at_index(index as i32)) {
        list.select_row(Some(&row));
    }
}

fn replace_window_group_entry_summary(list: &gtk::ListBox, index: usize, entry: &WindowGroupEntry) {
    if let Some(row) = list.row_at_index(index as i32) {
        let label = gtk::Label::new(Some(&window_group_entry_summary(index, entry)));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_tooltip_text(Some(&window_group_entry_summary(index, entry)));
        row.set_child(Some(&label));
    }
}

fn field_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label
}

fn check(text: &str, active: bool) -> gtk::CheckButton {
    let button = gtk::CheckButton::with_label(text);
    button.set_active(active);
    button.set_halign(gtk::Align::Start);
    button
}

fn hint_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label
}

fn show_settings_error(parent: &gtk::Window, title: &str, detail: impl AsRef<str>) {
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(false)
        .default_width(440)
        .default_height(160)
        .build();
    enforce_non_modal(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    let label = gtk::Label::new(Some(detail.as_ref()));
    label.set_wrap(true);
    label.set_xalign(0.0);
    content.append(&label);
    let close = gtk::Button::with_label("OK");
    close.set_size_request(96, 36);
    close.set_halign(gtk::Align::End);
    let close_dialog = dialog.clone();
    close.connect_clicked(move |_| close_dialog.close());
    content.append(&close);
    dialog.set_child(Some(&content));
    dialog.present();
}

fn page_heading(page: &gtk::Box, title: &str, subtitle: &str) {
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("core-settings-title");
    heading.set_halign(gtk::Align::Start);
    page.append(&heading);
    page.append(&hint_label(subtitle));
}

fn profile_page_with_checks(
    title: &str,
    subtitle: &str,
    values: &[(&str, bool)],
) -> (gtk::Box, Vec<gtk::CheckButton>) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("core-settings-pane");
    page_heading(&page, title, subtitle);
    let checks = values
        .iter()
        .map(|(label, active)| check(label, *active))
        .collect::<Vec<_>>();
    for button in &checks {
        page.append(button);
    }
    (page, checks)
}

struct UiState {
    profiles: ProfileStore,
    settings: Settings,
    sessions: SessionManager,
    stack: gtk::Stack,
    window: gtk::ApplicationWindow,
    profile_dropdown: gtk::DropDown,
    terminals: HashMap<u64, vte4::Terminal>,
    pending_spawns: HashSet<u64>,
    exited_before_spawn_callbacks: HashSet<u64>,
    child_process_identities: HashMap<u64, core::ChildProcessIdentity>,
    login_shell_identities: HashMap<u64, core::ExecutableIdentity>,
    closing: bool,
    close_prompt_open: bool,
    active_close_request: Option<CloseRequest>,
    pending_close_request: Option<CloseRequest>,
    window_close_authorization: Option<core::ClosePlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseRequest {
    Tab(SessionId),
    Window,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpawnCallbackAction {
    IgnoreExitedChild,
    TerminateClosedTab,
    StoreLiveChild,
    IgnoreLateCallback,
}

fn spawn_callback_action(
    exited_before_callback: bool,
    closing: bool,
    session_exists: bool,
    was_pending: bool,
) -> SpawnCallbackAction {
    if exited_before_callback {
        SpawnCallbackAction::IgnoreExitedChild
    } else if closing || !session_exists {
        SpawnCallbackAction::TerminateClosedTab
    } else if was_pending {
        SpawnCallbackAction::StoreLiveChild
    } else {
        SpawnCallbackAction::IgnoreLateCallback
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TabLaunchSpec {
    profile_name: String,
    working_directory: Option<String>,
    size: Option<(u32, u32)>,
}

impl TabLaunchSpec {
    fn new(profile_name: impl Into<String>, working_directory: Option<String>) -> Self {
        Self {
            profile_name: profile_name.into(),
            working_directory,
            size: None,
        }
    }

    fn from_window_group_entry(entry: WindowGroupEntry) -> Self {
        Self {
            profile_name: entry.profile,
            working_directory: entry.working_directory,
            size: Some((entry.columns, entry.rows)),
        }
    }
}

fn resolve_window_profile(
    settings: &Settings,
    profiles: &ProfileStore,
    new_window: bool,
) -> String {
    let requested = if new_window && settings.new_window_profile != "default" {
        settings.new_window_profile.as_str()
    } else {
        settings.startup_profile.as_str()
    };
    [
        requested,
        settings.startup_profile.as_str(),
        settings.selected_profile.as_str(),
        profiles.selected_name(),
    ]
    .into_iter()
    .find(|name| profiles.profile(name).is_some())
    .unwrap_or(profiles.selected_name())
    .to_owned()
}

fn resolve_new_tab_profile(
    settings: &Settings,
    sessions: &SessionManager,
    profiles: &ProfileStore,
) -> String {
    let configured = (settings.new_tab_profile != "same")
        .then_some(settings.new_tab_profile.as_str())
        .filter(|name| profiles.profile(name).is_some());
    let active = sessions
        .active()
        .map(|tab| tab.profile_name.as_str())
        .filter(|name| profiles.profile(name).is_some());
    configured
        .or(active)
        .or_else(|| {
            profiles
                .profile(&settings.selected_profile)
                .map(|_| settings.selected_profile.as_str())
        })
        .or_else(|| {
            profiles
                .profile(&settings.startup_profile)
                .map(|_| settings.startup_profile.as_str())
        })
        .unwrap_or(profiles.selected_name())
        .to_owned()
}

/// Start the GTK application.  The application keeps terminal/session state
/// in one small reference-counted model; VTE remains responsible for PTYs,
/// rendering, selection, scrollback and terminal protocol handling.
pub fn run(application_id: &str, display_name: &str) {
    let app = gtk::Application::builder()
        .application_id(application_id)
        .build();
    let display_name = display_name.to_owned();
    let activate_name = display_name.clone();
    app.connect_activate(move |app| build_window(app, &activate_name, false));
    // New-window is installed on each window so it can honor that window's
    // current directory policy. The application-level accelerator resolves
    // the action on the active window.
    app.set_accels_for_action("win.new-window", &["<Primary>n"]);
    app.run();
}

fn build_window(app: &gtk::Application, display_name: &str, new_window: bool) {
    build_window_with_directory(app, display_name, new_window, None);
}

fn build_window_with_directory(
    app: &gtk::Application,
    display_name: &str,
    new_window: bool,
    pending_working_directory: Option<String>,
) {
    gtk::Window::set_default_icon_name(APPLICATION_ID);
    let profiles = load_user_profiles();
    let mut settings = Settings::load_user();
    let requested_profile = resolve_window_profile(&settings, &profiles, new_window);
    settings.selected_profile = requested_profile.clone();
    // Materialize first-launch defaults so Homebrew and the initial window
    // geometry can be verified and restored even if the first session ends
    // unexpectedly.
    let _ = settings.save_user();
    let profile_dropdown = profile_selector(&profiles, &requested_profile);
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(display_name)
        .icon_name(APPLICATION_ID)
        .default_width(settings.window_width)
        .default_height(settings.window_height)
        .build();
    if let Some(display) = gtk::gdk::Display::default() {
        install_style(&display);
    }

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .build();
    let state = Rc::new(RefCell::new(UiState {
        profiles,
        settings,
        sessions: SessionManager::empty(),
        stack: stack.clone(),
        window: window.clone(),
        profile_dropdown: profile_dropdown.clone(),
        terminals: HashMap::new(),
        pending_spawns: HashSet::new(),
        exited_before_spawn_callbacks: HashSet::new(),
        child_process_identities: HashMap::new(),
        login_shell_identities: HashMap::new(),
        closing: false,
        close_prompt_open: false,
        active_close_request: None,
        pending_close_request: None,
        window_close_authorization: None,
    }));

    let header = build_header_bar();
    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));

    let icon = project_icon();
    icon.set_pixel_size(24);
    icon.set_tooltip_text(Some("Core Terminal"));
    header.pack_start(&icon);

    let new_tab = gtk::Button::from_icon_name("tab-new-symbolic");
    new_tab.set_tooltip_text(Some("New tab (Ctrl+T)"));
    let new_tab_state = state.clone();
    new_tab.connect_clicked(move |_| open_tab(&new_tab_state));
    header.pack_end(&new_tab);

    let settings_button = gtk::Button::from_icon_name("emblem-system-symbolic");
    settings_button.set_tooltip_text(Some("Settings"));
    let settings_state = state.clone();
    settings_button.connect_clicked(move |_| {
        show_settings_for_state(&settings_state);
    });
    header.pack_end(&settings_button);
    header.pack_end(&profile_dropdown);

    let menu = gio::Menu::new();
    menu.append(Some("New Window"), Some("win.new-window"));
    menu.append(Some("New Tab"), Some("win.new-tab"));
    menu.append(Some("Close Tab"), Some("win.close-tab"));
    menu.append(Some("Next Tab"), Some("win.next-tab"));
    menu.append(Some("Previous Tab"), Some("win.previous-tab"));
    menu.append(Some("Find"), Some("win.search"));
    menu.append(Some("Settings"), Some("win.settings"));
    menu.append(
        Some("Restore Default Profiles"),
        Some("win.restore-profiles"),
    );
    menu.append(Some("About Core Terminal"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Terminal menu")
        .menu_model(&menu)
        .build();
    header.pack_end(&menu_button);

    let profile_state = state.clone();
    connect_profile_selector(&profile_dropdown, move |name| {
        let Ok(mut state) = profile_state.try_borrow_mut() else {
            return;
        };
        let width = state.settings.window_width;
        let height = state.settings.window_height;
        let Some(profile) = state.profiles.profile(&name).cloned() else {
            return;
        };
        // Header profile selection changes only the active profile. Preserve
        // startup, shell, tab, geometry, and notification preferences.
        state.settings.selected_profile = name.clone();
        state.settings.window_width = width;
        state.settings.window_height = height;
        if let Some(session) = state.sessions.active_mut() {
            session.profile_name = name;
        }
        let active = state.sessions.active().and_then(|session| {
            state
                .terminals
                .get(&session.id.get())
                .cloned()
                .map(|terminal| (session.id, terminal))
        });
        if let Some((_, terminal)) = &active {
            apply_profile(terminal, &profile, &state.settings);
        }
        let _ = state.settings.save_user();
        drop(state);
        if let Some((id, terminal)) = active {
            update_tab_title(&profile_state, id, &terminal);
        }
    });

    window.set_titlebar(Some(&header));
    window.set_child(Some(&stack));

    install_window_actions(app, &window, &state);
    let visible_state = state.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        let Some(id) = stack.visible_child_name().and_then(|name| tab_id(&name)) else {
            return;
        };
        let terminal = if let Ok(mut state) = visible_state.try_borrow_mut() {
            if let Some(index) = state.sessions.tabs().iter().position(|tab| tab.id == id) {
                state.sessions.select_tab(index);
            }
            state.terminals.get(&id.get()).cloned()
        } else {
            None
        };
        sync_active_profile_ui(&visible_state);
        if let Some(terminal) = terminal {
            if let Some(page_child) = stack_page_child(stack, &terminal) {
                stack.page(&page_child).set_needs_attention(false);
            }
            update_tab_title(&visible_state, id, &terminal);
        }
    });
    let startup_group = if new_window {
        None
    } else {
        let state = state.borrow();
        (!state.settings.startup_window_group.is_empty())
            .then(|| {
                state
                    .profiles
                    .window_group(&state.settings.startup_window_group)
                    .cloned()
            })
            .flatten()
    };
    if let Some(group) = startup_group {
        launch_window_group(&state, group);
    } else {
        open_tab_with_spec(
            &state,
            TabLaunchSpec::new(requested_profile, pending_working_directory),
        );
    }

    let close_state = state.clone();
    window.connect_close_request(move |_| {
        let (plan, authorization) = {
            let mut state = close_state.borrow_mut();
            if state.closing {
                return glib::Propagation::Proceed;
            }
            let plan = close_plan_for(&state, CloseRequest::Window);
            let authorization = state.window_close_authorization.take();
            (plan, authorization)
        };
        let authorized = plan.blockers.is_empty()
            || authorization
                .as_ref()
                .is_some_and(|confirmed| core::close_authorization_covers(&plan, confirmed));
        if authorized {
            finish_window_close(&close_state);
            return glib::Propagation::Proceed;
        }
        if queue_or_reserve_close_prompt(&close_state, CloseRequest::Window) {
            let prompt_state = close_state.clone();
            glib::idle_add_local_once(move || {
                present_close_confirmation(&prompt_state, CloseRequest::Window, plan);
            });
        }
        glib::Propagation::Stop
    });
    window.present();
    schedule_acceptance_harness(app, &state);
}

/// Drive the same window/session helpers used by buttons and accelerators.
/// This is enabled only by the acceptance-test environment and lets the
/// native GNOME Wayland session verify real VTE PTYs and widget behavior.
fn project_icon() -> gtk::Image {
    let installed_icon = format!("/usr/share/icons/hicolor/64x64/apps/{APPLICATION_ID}.png");
    for candidate in [
        Path::new("data/icons/core-terminal-icon-64.png"),
        Path::new(&installed_icon),
    ] {
        if candidate.is_file() {
            return gtk::Image::from_file(candidate);
        }
    }
    gtk::Image::from_icon_name(APPLICATION_ID)
}

/// Opt-in installed-binary acceptance run.  The harness is intentionally
/// inert unless CORE_TERMINAL_ACCEPTANCE is present, so normal users never
/// get an automated tab/window lifecycle.  It uses the real GTK widget tree
/// and VTE instances created by this module rather than string-only tests.
fn schedule_acceptance_harness(app: &gtk::Application, state: &Rc<RefCell<UiState>>) {
    fn close_prompt_window() -> Option<gtk::Window> {
        gtk::Window::list_toplevels()
            .into_iter()
            .find(|widget| widget.widget_name() == "close-confirmation")
            .and_downcast::<gtk::Window>()
    }

    fn close_prompt_button(name: &str) -> Option<gtk::Button> {
        let window = close_prompt_window()?;
        find_widget_by_name(&window.upcast::<gtk::Widget>(), name).and_downcast::<gtk::Button>()
    }

    fn close_prompt_details_are_bounded() -> bool {
        let Some(window) = close_prompt_window() else {
            return false;
        };
        let Some(scroller) = find_widget_by_name(
            &window.upcast::<gtk::Widget>(),
            "close-confirmation-processes",
        )
        .and_downcast::<gtk::ScrolledWindow>() else {
            return false;
        };
        scroller.max_content_height() == 160
            && scroller.vscrollbar_policy() == gtk::PolicyType::Automatic
    }

    fn drain_pending_events() {
        while glib::MainContext::default().iteration(false) {}
    }

    fn wait_for_condition(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            drain_pending_events();
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[derive(Clone, Debug)]
    struct AcceptanceProcess {
        pid: i32,
        process_group: i32,
        session: i32,
        start_time: u64,
        arguments: Vec<String>,
    }

    #[cfg(target_os = "linux")]
    fn acceptance_process(pid: i32) -> Option<AcceptanceProcess> {
        let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
        let stat = std::str::from_utf8(&stat).ok()?;
        let fields = stat.rsplit_once(") ")?.1;
        let mut fields = fields.split_whitespace();
        let _state = fields.next()?;
        let _parent = fields.next()?;
        let process_group = fields.next()?.parse().ok()?;
        let session = fields.next()?.parse().ok()?;
        for _ in 0..15 {
            fields.next()?;
        }
        let start_time = fields.next()?.parse().ok()?;
        let arguments = std::fs::read(format!("/proc/{pid}/cmdline"))
            .ok()?
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| std::str::from_utf8(argument).ok().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Some(AcceptanceProcess {
            pid,
            process_group,
            session,
            start_time,
            arguments,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn acceptance_process(_pid: i32) -> Option<AcceptanceProcess> {
        None
    }

    fn acceptance_process_has_arguments(
        process: &AcceptanceProcess,
        executable_name: &str,
        argument: &str,
    ) -> bool {
        process.arguments.first().map(String::as_str) == Some(executable_name)
            && process.arguments.get(1).map(String::as_str) == Some(argument)
            && process.arguments.len() == 2
    }

    #[cfg(target_os = "linux")]
    fn acceptance_marker_pids(executable_name: &str, argument: &str) -> Vec<i32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut pids = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(acceptance_process)
            .filter(|process| acceptance_process_has_arguments(process, executable_name, argument))
            .map(|process| process.pid)
            .collect::<Vec<_>>();
        pids.sort_unstable();
        pids
    }

    #[cfg(not(target_os = "linux"))]
    fn acceptance_marker_pids(_executable_name: &str, _argument: &str) -> Vec<i32> {
        Vec::new()
    }

    static STARTED: AtomicBool = AtomicBool::new(false);
    if std::env::var_os("CORE_TERMINAL_ACCEPTANCE").is_none()
        || STARTED.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let weak_app = app.downgrade();
    let state = state.clone();
    glib::timeout_add_local_once(Duration::from_millis(900), move || {
        let Some(app) = weak_app.upgrade() else {
            return;
        };
        let mut confirmation_accepted = false;
        let mut stale_pending_revalidated = false;
        let mut new_window_target_revalidated = false;
        let mut overlapping_window_request_preserved = false;
        let mut state_machine_probe_cleanup = false;
        let close_before_spawn_cleanup = {
            let probe_name = "Acceptance Real Spawn Race";
            let profile_added = state.try_borrow_mut().is_ok_and(|mut state| {
                let Some(mut profile) = state.profiles.profile("Homebrew").cloned() else {
                    return false;
                };
                profile.name = probe_name.into();
                profile.shell_command = "/bin/sleep 37.123".into();
                profile.run_inside_shell = false;
                profile.close_on_exit = CloseOnExit::Never;
                state.profiles.add_profile(profile).is_ok()
            });
            if profile_added {
                let resolutions_before = ACCEPTANCE_CLOSED_SPAWN_RESOLUTIONS.load(Ordering::SeqCst);
                open_tab_with_spec(&state, TabLaunchSpec::new(probe_name, None));
                let pending_id = state.try_borrow().ok().and_then(|state| {
                    let id = state.sessions.active()?.id;
                    state.pending_spawns.contains(&id.get()).then_some(id)
                });
                if let Some(id) = pending_id {
                    // Do not dispatch the main loop between open and close:
                    // this is the real interval before VTE's callback returns.
                    force_close_tab(&state, id);
                    let callback_resolved = wait_for_condition(Duration::from_secs(4), || {
                        ACCEPTANCE_CLOSED_SPAWN_RESOLUTIONS.load(Ordering::SeqCst)
                            > resolutions_before
                    });
                    let clean = state.try_borrow_mut().is_ok_and(|mut state| {
                        let state_clean = state.sessions.tab(id).is_none()
                            && !state.pending_spawns.contains(&id.get())
                            && !state.exited_before_spawn_callbacks.contains(&id.get())
                            && !state.login_shell_identities.contains_key(&id.get())
                            && !state.closing;
                        let _ = state.profiles.delete_profile(probe_name);
                        state_clean && state.profiles.profile(probe_name).is_none()
                    });
                    callback_resolved && clean
                } else {
                    let _ = state.borrow_mut().profiles.delete_profile(probe_name);
                    false
                }
            } else {
                false
            }
        };
        let mut close_prompt_details_bounded = false;
        let brokered_runtime = core::running_in_flatpak();
        let process_cleanup = {
            let probe_name = "Acceptance Background Session";
            let probe_suffix = std::env::var("CORE_TERMINAL_ACCEPTANCE_MARKER_SUFFIX")
                .ok()
                .filter(|suffix| {
                    !suffix.is_empty()
                        && suffix.len() <= 96
                        && suffix
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
                .unwrap_or_else(|| std::process::id().to_string());
            let background_marker = format!("core-terminal-acceptance-bg-{probe_suffix}");
            let foreground_marker = format!("core-terminal-acceptance-fg-{probe_suffix}");
            let probe_script = format!(
                "set -m; (trap '' HUP TERM; exec -a {background_marker} sleep 41.234) & \
                 (trap '' HUP TERM; exec -a {foreground_marker} sleep 40.234); wait"
            );
            let probe_command = format!(
                "/bin/bash --noprofile --norc -c {}",
                glib::shell_quote(&probe_script).to_string_lossy()
            );
            let brokered_ready_path = std::env::var_os("CORE_TERMINAL_ACCEPTANCE_HOST_JOBS_SEEN")
                .map(std::path::PathBuf::from);
            let original_selected = state
                .try_borrow()
                .map(|state| state.settings.selected_profile.clone())
                .unwrap_or_default();
            let profile_added = state.try_borrow_mut().is_ok_and(|mut state| {
                let Some(mut profile) = state.profiles.profile("Homebrew").cloned() else {
                    return false;
                };
                profile.name = probe_name.into();
                profile.shell = "/bin/bash".into();
                profile.shell_command = probe_command.clone();
                profile.run_inside_shell = false;
                profile.ask_before_close_policy = AskBeforeClosePolicy::Always;
                state.profiles.add_profile(profile).is_ok()
            });
            if profile_added {
                open_tab_with_spec(&state, TabLaunchSpec::new(probe_name, None));
                let probe_id = state
                    .try_borrow()
                    .ok()
                    .and_then(|state| state.sessions.active().map(|tab| tab.id));
                let captured_session_processes = Rc::new(RefCell::new(None));
                let session_processes = probe_id.and_then(|id| {
                    let captured_session_processes = captured_session_processes.clone();
                    let observation_timeout = if brokered_runtime {
                        Duration::from_secs(16)
                    } else {
                        Duration::from_secs(4)
                    };
                    wait_for_condition(observation_timeout, || {
                        let Ok(state) = state.try_borrow() else {
                            return false;
                        };
                        let process = close_plan_for(&state, CloseRequest::Tab(id))
                            .blockers
                            .first()
                            .map(|blocker| blocker.process.clone());
                        let Some(process) = process else {
                            return false;
                        };
                        let Some(child_pid) = process.child_pid else {
                            return false;
                        };
                        if brokered_runtime {
                            let token_matches = state
                                .child_process_identities
                                .get(&id.get())
                                .is_some_and(|identity| identity.pid().0 == child_pid);
                            let Some(proxy) = acceptance_process(child_pid) else {
                                return false;
                            };
                            let proxy_name_matches = proxy
                                .arguments
                                .first()
                                .and_then(|argument| Path::new(argument).file_name())
                                .and_then(|name| name.to_str())
                                == Some("flatpak-spawn");
                            let separators = proxy
                                .arguments
                                .iter()
                                .enumerate()
                                .filter_map(|(index, argument)| (argument == "--").then_some(index))
                                .collect::<Vec<_>>();
                            let proxy_arguments_match =
                                separators.first().is_some_and(|&separator| {
                                    separators.len() == 1
                                        && proxy.arguments[..separator]
                                            .iter()
                                            .filter(|argument| argument.as_str() == "--host")
                                            .count()
                                            == 1
                                        && proxy.arguments[..separator]
                                            .iter()
                                            .filter(|argument| argument.as_str() == "--watch-bus")
                                            .count()
                                            == 1
                                        && proxy.arguments[..separator]
                                            .iter()
                                            .filter(|argument| {
                                                argument.as_str() == "--forward-fd=3"
                                            })
                                            .count()
                                            == 1
                                        && proxy.arguments[separator + 1..]
                                            == [
                                                "/proc/self/fd/3",
                                                "/bin/bash",
                                                "--noprofile",
                                                "--norc",
                                                "-c",
                                                probe_script.as_str(),
                                            ]
                                });
                            let host_started = brokered_ready_path
                                .as_ref()
                                .is_some_and(|path| path.is_file());
                            if token_matches
                                && proxy_name_matches
                                && proxy_arguments_match
                                && host_started
                            {
                                *captured_session_processes.borrow_mut() = Some(vec![proxy]);
                                return true;
                            }
                            return false;
                        }
                        let (Some(child_pid), Some(foreground_pgid), Some(pids)) = (
                            Some(child_pid),
                            process.foreground_pgid,
                            process.session_processes,
                        ) else {
                            return false;
                        };
                        let members = pids
                            .iter()
                            .filter_map(|pid| acceptance_process(*pid))
                            .collect::<Vec<_>>();
                        let child = members.iter().find(|member| member.pid == child_pid);
                        let background = members.iter().find(|member| {
                            acceptance_process_has_arguments(member, &background_marker, "41.234")
                        });
                        let foreground = members.iter().find(|member| {
                            acceptance_process_has_arguments(member, &foreground_marker, "40.234")
                        });
                        let roles_are_exact = child.zip(background).zip(foreground).is_some_and(
                            |((child, background), foreground)| {
                                child.pid != background.pid
                                    && child.pid != foreground.pid
                                    && background.pid != foreground.pid
                                    && child.session == child.pid
                                    && child.arguments.first().map(String::as_str)
                                        == Some("/bin/bash")
                                    && child.arguments.get(1).map(String::as_str)
                                        == Some("--noprofile")
                                    && child.arguments.get(2).map(String::as_str) == Some("--norc")
                                    && child.arguments.get(3).map(String::as_str) == Some("-c")
                                    && child.arguments.get(4).map(String::as_str)
                                        == Some(probe_script.as_str())
                                    && child.session == background.session
                                    && child.session == foreground.session
                                    && background.process_group != foreground.process_group
                                    && foreground.process_group == foreground_pgid
                            },
                        );
                        if roles_are_exact {
                            *captured_session_processes.borrow_mut() = Some(members);
                            true
                        } else {
                            false
                        }
                    })
                    .then(|| captured_session_processes.borrow().clone())
                    .flatten()
                });
                let prompted = probe_id.is_some_and(|id| {
                    close_tab(&state, id);
                    drain_pending_events();
                    close_prompt_details_bounded = close_prompt_details_are_bounded();
                    close_prompt_button("close-confirmation-accept")
                        .is_some_and(|button| button.label().as_deref() == Some("Close Tab"))
                        && state.try_borrow().is_ok_and(|state| {
                            state.active_close_request == Some(CloseRequest::Tab(id))
                        })
                });
                let close_accepted = prompted
                    && probe_id.is_some_and(|id| {
                        wait_for_condition(Duration::from_secs(4), || {
                            if state
                                .try_borrow()
                                .is_ok_and(|state| state.sessions.tab(id).is_none())
                            {
                                return true;
                            }
                            let expected_prompt = state.try_borrow().is_ok_and(|state| {
                                state.close_prompt_open
                                    && state.active_close_request == Some(CloseRequest::Tab(id))
                            });
                            if expected_prompt {
                                if let Some(accept) =
                                    close_prompt_button("close-confirmation-accept")
                                {
                                    if accept.label().as_deref() == Some("Close Tab") {
                                        accept.emit_clicked();
                                        drain_pending_events();
                                    }
                                }
                            }
                            false
                        })
                    });
                let processes_gone = session_processes.as_ref().is_some_and(|processes| {
                    wait_for_condition(Duration::from_secs(4), || {
                        processes.iter().all(|expected| {
                            acceptance_process(expected.pid).is_none_or(|current| {
                                current.start_time != expected.start_time
                                    || current.session != expected.session
                            })
                        }) && acceptance_marker_pids(&background_marker, "41.234").is_empty()
                            && acceptance_marker_pids(&foreground_marker, "40.234").is_empty()
                    })
                });
                let model_clean = probe_id.is_some_and(|id| {
                    state
                        .try_borrow()
                        .is_ok_and(|state| state.sessions.tab(id).is_none())
                });
                if let Some(id) = probe_id {
                    let prompt_belongs_to_probe = state.try_borrow().is_ok_and(|state| {
                        state.close_prompt_open
                            && state.active_close_request == Some(CloseRequest::Tab(id))
                    });
                    if prompt_belongs_to_probe {
                        if let Some(cancel) = close_prompt_button("close-confirmation-cancel") {
                            cancel.emit_clicked();
                            drain_pending_events();
                        }
                    }
                    if state
                        .try_borrow()
                        .is_ok_and(|state| state.sessions.tab(id).is_some())
                    {
                        force_close_tab(&state, id);
                    }
                }
                if let Some(processes) = &session_processes {
                    for expected in processes {
                        let _ = core::kill_process_if_exact(
                            expected.pid,
                            expected.start_time,
                            expected.session,
                            expected.process_group,
                        );
                    }
                }
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.settings.selected_profile = original_selected;
                    let _ = state.profiles.delete_profile(probe_name);
                }
                sync_active_profile_ui(&state);
                prompted && close_accepted && processes_gone && model_clean
            } else {
                false
            }
        };
        let background_session_cleanup = !brokered_runtime && process_cleanup;
        let brokered_proxy_cleanup = brokered_runtime && process_cleanup;
        let state_machine_probe = {
            let mut state = state.borrow_mut();
            let active = state.sessions.active().map(|tab| {
                (
                    tab.id,
                    tab.profile_name.clone(),
                    tab.child_pid,
                    state.sessions.tabs().len(),
                )
            });
            active.and_then(
                |(protected_id, profile_name, protected_pid, original_session_count)| {
                    let template = state.profiles.profile(&profile_name).cloned()?;
                    let original_selected_profile = state.settings.selected_profile.clone();
                    let probe_name = "Acceptance Close State Machine".to_owned();
                    let mut profile = template;
                    profile.name = probe_name.clone();
                    profile.shell_command = "/bin/sleep 30".into();
                    profile.run_inside_shell = false;
                    profile.ask_before_close = false;
                    profile.ask_before_close_policy = AskBeforeClosePolicy::Always;
                    state.profiles.add_profile(profile).ok()?;
                    Some((
                        protected_id,
                        protected_pid,
                        original_session_count,
                        original_selected_profile,
                        probe_name,
                    ))
                },
            )
        };
        if let Some((
            protected_id,
            protected_pid,
            original_session_count,
            original_selected_profile,
            probe_name,
        )) = state_machine_probe
        {
            open_tab_with_spec(&state, TabLaunchSpec::new(probe_name.clone(), None));
            let probe_id = state
                .try_borrow()
                .ok()
                .and_then(|state| state.sessions.active().map(|tab| tab.id));
            let probe_pid = probe_id.and_then(|probe_id| {
                wait_for_condition(Duration::from_secs(4), || {
                    state.try_borrow().is_ok_and(|state| {
                        !state.pending_spawns.contains(&probe_id.get())
                            && state
                                .sessions
                                .tab(probe_id)
                                .is_some_and(|tab| tab.child_pid.is_some())
                    })
                })
                .then(|| {
                    state
                        .borrow()
                        .sessions
                        .tab(probe_id)
                        .and_then(|tab| tab.child_pid)
                })
                .flatten()
            });

            if let (Some(probe_id), Some(probe_pid)) = (probe_id, probe_pid) {
                // Model the exact interval between VTE creating a tab and its
                // async spawn callback returning a PID. Accepting that stale
                // pending-process confirmation must re-prompt for the live PID.
                state.borrow_mut().pending_spawns.insert(probe_id.get());
                close_tab(&state, probe_id);
                drain_pending_events();
                let pending_prompted = close_prompt_button("close-confirmation-accept")
                    .is_some_and(|button| button.label().as_deref() == Some("Close Tab"))
                    && state.try_borrow().is_ok_and(|state| {
                        state.close_prompt_open
                            && state.active_close_request == Some(CloseRequest::Tab(probe_id))
                            && close_plan_for(&state, CloseRequest::Tab(probe_id))
                                .blockers
                                .iter()
                                .any(|blocker| blocker.process.child_pid.is_none())
                    });
                state.borrow_mut().pending_spawns.remove(&probe_id.get());
                if let Some(accept) = close_prompt_button("close-confirmation-accept") {
                    accept.emit_clicked();
                    drain_pending_events();
                }
                stale_pending_revalidated = pending_prompted
                    && close_prompt_button("close-confirmation-accept")
                        .is_some_and(|button| button.label().as_deref() == Some("Close Tab"))
                    && state.try_borrow().is_ok_and(|state| {
                        state.close_prompt_open
                            && state.active_close_request == Some(CloseRequest::Tab(probe_id))
                            && state.sessions.tab(probe_id).is_some()
                            && close_plan_for(&state, CloseRequest::Tab(probe_id))
                                .blockers
                                .iter()
                                .any(|blocker| blocker.process.child_pid == Some(probe_pid.0))
                    });
                if let Some(cancel) = close_prompt_button("close-confirmation-cancel") {
                    cancel.emit_clicked();
                    drain_pending_events();
                }

                // A later whole-window request must not be lost while the tab
                // confirmation is visible. Cancelling the tab prompt should
                // dispatch and display the queued Window request.
                close_tab(&state, probe_id);
                drain_pending_events();
                let tab_prompted = close_prompt_button("close-confirmation-accept")
                    .is_some_and(|button| button.label().as_deref() == Some("Close Tab"));
                let window = state.borrow().window.clone();
                window.close();
                drain_pending_events();
                let window_queued = state.try_borrow().is_ok_and(|state| {
                    state.close_prompt_open
                        && state.active_close_request == Some(CloseRequest::Tab(probe_id))
                        && state.pending_close_request == Some(CloseRequest::Window)
                });
                if let Some(cancel) = close_prompt_button("close-confirmation-cancel") {
                    cancel.emit_clicked();
                    drain_pending_events();
                }
                let window_prompted = close_prompt_button("close-confirmation-accept")
                    .is_some_and(|button| button.label().as_deref() == Some("Close Window"))
                    && state.try_borrow().is_ok_and(|state| {
                        state.close_prompt_open
                            && state.active_close_request == Some(CloseRequest::Window)
                            && state.pending_close_request.is_none()
                            && state.sessions.tab(probe_id).is_some()
                    });
                let late_target_id = state.borrow_mut().sessions.open_tab(&probe_name, None);
                if let Some(accept) = close_prompt_button("close-confirmation-accept") {
                    accept.emit_clicked();
                    drain_pending_events();
                }
                new_window_target_revalidated = window_prompted
                    && close_prompt_button("close-confirmation-accept")
                        .is_some_and(|button| button.label().as_deref() == Some("Close Window"))
                    && state.try_borrow().is_ok_and(|state| {
                        !state.closing
                            && state.close_prompt_open
                            && state.active_close_request == Some(CloseRequest::Window)
                            && state.sessions.tab(probe_id).is_some()
                            && state.sessions.tab(late_target_id).is_some()
                            && close_plan_for(&state, CloseRequest::Window)
                                .targets
                                .contains(&late_target_id)
                    });
                if let Some(cancel) = close_prompt_button("close-confirmation-cancel") {
                    cancel.emit_clicked();
                    drain_pending_events();
                }
                force_close_tab(&state, late_target_id);
                overlapping_window_request_preserved = tab_prompted
                    && window_queued
                    && new_window_target_revalidated
                    && close_prompt_window().is_none()
                    && state.try_borrow().is_ok_and(|state| {
                        !state.close_prompt_open
                            && state.active_close_request.is_none()
                            && state.pending_close_request.is_none()
                            && state.sessions.tab(probe_id).is_some()
                    });

                // Accept a freshly evaluated live-process confirmation and
                // verify that only the requested tab is terminated.
                close_tab(&state, probe_id);
                drain_pending_events();
                let fresh_prompted = close_prompt_button("close-confirmation-accept")
                    .is_some_and(|button| button.label().as_deref() == Some("Close Tab"));
                if let Some(accept) = close_prompt_button("close-confirmation-accept") {
                    accept.emit_clicked();
                    drain_pending_events();
                }
                confirmation_accepted = fresh_prompted
                    && close_prompt_window().is_none()
                    && state.try_borrow().is_ok_and(|state| {
                        !state.close_prompt_open
                            && state.active_close_request.is_none()
                            && state.pending_close_request.is_none()
                            && state.sessions.tab(probe_id).is_none()
                            && state.sessions.tabs().len() == original_session_count
                            && state.sessions.tab(protected_id).is_some_and(|tab| {
                                protected_pid.is_none() || tab.child_pid == protected_pid
                            })
                    });
            }

            // Failure-path cleanup is deliberately unconditional so a failed
            // assertion cannot leak a prompt, pending request, or sleep process
            // into the remainder of the installed-binary acceptance run.
            if let Ok(mut state) = state.try_borrow_mut() {
                state.pending_close_request = None;
            }
            if let Some(prompt) = close_prompt_window() {
                prompt.close();
                drain_pending_events();
            }
            if let Some(probe_id) = probe_id {
                if state
                    .try_borrow()
                    .is_ok_and(|state| state.sessions.tab(probe_id).is_some())
                {
                    force_close_tab(&state, probe_id);
                }
            }
            if let Ok(mut state) = state.try_borrow_mut() {
                state.close_prompt_open = false;
                state.active_close_request = None;
                state.pending_close_request = None;
                state.settings.selected_profile = original_selected_profile.clone();
                let _ = state.profiles.delete_profile(&probe_name);
            }
            sync_active_profile_ui(&state);
            state_machine_probe_cleanup = close_prompt_window().is_none()
                && state.try_borrow().is_ok_and(|state| {
                    !state.closing
                        && !state.close_prompt_open
                        && state.active_close_request.is_none()
                        && state.pending_close_request.is_none()
                        && state.sessions.tabs().len() == original_session_count
                        && state.sessions.tab(protected_id).is_some_and(|tab| {
                            protected_pid.is_none() || tab.child_pid == protected_pid
                        })
                        && state.profiles.profile(&probe_name).is_none()
                });
        }
        let mut tab_close_prompted = false;
        let mut tab_close_cancelled = false;
        let tab_probe = {
            let mut state = state.borrow_mut();
            let active = state.sessions.active().map(|tab| {
                (
                    tab.id,
                    tab.profile_name.clone(),
                    tab.child_pid,
                    state.sessions.tabs().len(),
                )
            });
            active.and_then(|(id, profile_name, child_pid, session_count)| {
                let original = state.profiles.profile(&profile_name).cloned()?;
                child_pid?;
                let mut guarded = original.clone();
                guarded.ask_before_close = false;
                guarded.ask_before_close_policy = AskBeforeClosePolicy::Always;
                state.profiles.update_profile(guarded).ok()?;
                Some((id, session_count, original))
            })
        };
        if let Some((id, session_count, original_profile)) = tab_probe {
            close_tab(&state, id);
            drain_pending_events();
            let prompt = close_prompt_window();
            tab_close_prompted = prompt.as_ref().is_some_and(|window| {
                !window.is_modal()
                    && close_prompt_button("close-confirmation-cancel")
                        .is_some_and(|button| button.label().as_deref() == Some("Cancel"))
            }) && state.try_borrow().is_ok_and(|state| {
                state.close_prompt_open
                    && state.window.is_sensitive()
                    && state.sessions.tabs().len() == session_count
                    && state.sessions.tab(id).is_some()
            });
            if let Some(cancel) = close_prompt_button("close-confirmation-cancel") {
                cancel.emit_clicked();
                drain_pending_events();
            } else if let Some(prompt) = prompt {
                prompt.close();
                drain_pending_events();
            }
            tab_close_cancelled = state.try_borrow().is_ok_and(|state| {
                !state.close_prompt_open
                    && state.window.is_sensitive()
                    && state.sessions.tabs().len() == session_count
                    && state.sessions.tab(id).is_some()
            }) && close_prompt_window().is_none();
            if let Ok(mut state) = state.try_borrow_mut() {
                let _ = state.profiles.update_profile(original_profile);
            }
        }

        let mut shell_exit_window_prompted = false;
        let mut shell_exit_prompt_cancelled = false;
        let mut exited_pid_cleared = false;
        let mut protected_sibling_preserved = false;
        let mut close_probe_cleanup = false;
        let shell_exit_probe = {
            let mut state = state.borrow_mut();
            let active = state
                .sessions
                .active()
                .map(|tab| (tab.id, tab.profile_name.clone(), tab.child_pid));
            active.and_then(|(protected_id, original_profile_name, protected_pid)| {
                let protected_pid = protected_pid?;
                let original = state.profiles.profile(&original_profile_name).cloned()?;
                let original_selected_profile = state.settings.selected_profile.clone();
                let original_session_count = state.sessions.tabs().len();
                let protected_name = "Acceptance Close Protected".to_owned();
                let exit_name = "Acceptance Close Exit".to_owned();
                let mut protected_profile = original.clone();
                protected_profile.name = protected_name.clone();
                protected_profile.ask_before_close = false;
                protected_profile.ask_before_close_policy = AskBeforeClosePolicy::Always;
                let mut exit_profile = original;
                exit_profile.name = exit_name.clone();
                exit_profile.shell_command = "/bin/true".into();
                exit_profile.run_inside_shell = false;
                exit_profile.close_on_exit = CloseOnExit::Never;
                exit_profile.close_on_clean_exit = false;
                exit_profile.close_on_error = false;
                exit_profile.shell_exit_action = ShellExitAction::CloseWindow;
                state.profiles.add_profile(protected_profile).ok()?;
                if state.profiles.add_profile(exit_profile).is_err() {
                    let _ = state.profiles.delete_profile(&protected_name);
                    return None;
                }
                state.sessions.set_profile(protected_id, &protected_name);
                Some((
                    protected_id,
                    protected_pid,
                    original_profile_name,
                    original_selected_profile,
                    original_session_count,
                    protected_name,
                    exit_name,
                ))
            })
        };
        if let Some((
            protected_id,
            protected_pid,
            original_profile_name,
            original_selected_profile,
            original_session_count,
            protected_name,
            exit_name,
        )) = shell_exit_probe
        {
            open_tab_with_spec(&state, TabLaunchSpec::new(exit_name.clone(), None));
            let exiting_id = state
                .try_borrow()
                .ok()
                .and_then(|state| state.sessions.active().map(|tab| tab.id));
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline
                && state
                    .try_borrow()
                    .map_or(true, |state| !state.close_prompt_open)
            {
                drain_pending_events();
                std::thread::sleep(Duration::from_millis(10));
            }
            drain_pending_events();
            let prompt = close_prompt_window();
            shell_exit_window_prompted = prompt.as_ref().is_some_and(|window| {
                !window.is_modal()
                    && close_prompt_button("close-confirmation-accept")
                        .is_some_and(|button| button.label().as_deref() == Some("Close Window"))
            }) && state
                .try_borrow()
                .is_ok_and(|state| state.close_prompt_open && state.window.is_sensitive());
            if let Some(exiting_id) = exiting_id {
                let state_ref = state.borrow();
                exited_pid_cleared = state_ref
                    .sessions
                    .tab(exiting_id)
                    .is_some_and(|tab| tab.child_pid.is_none());
                protected_sibling_preserved = state_ref
                    .sessions
                    .tab(protected_id)
                    .is_some_and(|tab| tab.child_pid == Some(protected_pid));
            }
            if let Some(cancel) = close_prompt_button("close-confirmation-cancel") {
                cancel.emit_clicked();
                drain_pending_events();
            } else if let Some(prompt) = prompt {
                prompt.close();
                drain_pending_events();
            }
            shell_exit_prompt_cancelled = state.try_borrow().is_ok_and(|state| {
                !state.close_prompt_open
                    && state.window.is_sensitive()
                    && state.sessions.tab(protected_id).is_some()
            }) && close_prompt_window().is_none();
            if let Some(exiting_id) = exiting_id {
                force_close_tab(&state, exiting_id);
            }
            if let Ok(mut state) = state.try_borrow_mut() {
                state
                    .sessions
                    .set_profile(protected_id, &original_profile_name);
                state.settings.selected_profile = original_selected_profile.clone();
                let _ = state.profiles.delete_profile(&exit_name);
                let _ = state.profiles.delete_profile(&protected_name);
            }
            sync_active_profile_ui(&state);
            if let Some(prompt) = close_prompt_window() {
                prompt.close();
                drain_pending_events();
            }
            close_probe_cleanup = state.try_borrow().is_ok_and(|state| {
                !state.close_prompt_open
                    && state.window.is_sensitive()
                    && state.sessions.tabs().len() == original_session_count
                    && state.sessions.tab(protected_id).is_some_and(|tab| {
                        tab.profile_name == original_profile_name
                            && tab.child_pid == Some(protected_pid)
                    })
                    && state.settings.selected_profile == original_selected_profile
                    && state.profiles.profile(&exit_name).is_none()
                    && state.profiles.profile(&protected_name).is_none()
            }) && close_prompt_window().is_none();
        }
        if let Some(prompt) = close_prompt_window() {
            prompt.close();
            drain_pending_events();
        }
        if state
            .try_borrow()
            .is_ok_and(|state| state.close_prompt_open)
        {
            reset_close_prompt(&state);
        }
        // Exercise the same actions used by the header/menu: create, switch,
        // and close a tab before opening the non-modal settings window.
        open_tab(&state);
        switch_tab(&state, false);
        close_current_tab(&state);
        {
            let mut state = state.borrow_mut();
            if state.profiles.profile("Acceptance Profile").is_none() {
                let _ = state
                    .profiles
                    .duplicate_profile("Homebrew", "Acceptance Profile");
            }
            // Keep the selected profile deliberately different from the global
            // compatibility snapshot. This catches stale editor initialization
            // and proves that fixed renderer/platform controls do not overwrite
            // imported profile metadata when the settings window is saved.
            if let Some(mut homebrew) = state.profiles.profile("Homebrew").cloned() {
                homebrew.font = "DejaVu Sans Mono".into();
                homebrew.font_size = 11.0;
                homebrew.cursor_shape = CursorShape::Block;
                homebrew.cursor_blink = true;
                homebrew.scrollback_lines = 9_000;
                homebrew.terminal_type = "homebrew-term".into();
                let _ = state.profiles.update_profile(homebrew);
            }
            if let Some(mut acceptance) = state.profiles.profile("Acceptance Profile").cloned() {
                acceptance.font = "Monospace".into();
                acceptance.font_size = 13.0;
                acceptance.cursor_shape = CursorShape::IBeam;
                acceptance.cursor_blink = false;
                acceptance.scrollback_lines = 20_000;
                acceptance.terminal_type = "acceptance-term".into();
                acceptance.antialias = false;
                acceptance.use_bold_fonts = false;
                acceptance.use_ansi_colors = false;
                acceptance.dynamic_colors = false;
                acceptance.smooth_resize = false;
                acceptance.restore_rows = true;
                acceptance.restore_rows_limit = 77_700;
                acceptance.restore_rows_bookmark = "acceptance-bookmark".into();
                acceptance.title_show_tty = true;
                acceptance.title_show_ctrl_key = true;
                acceptance.tab_title_show_ctrl_key = true;
                acceptance.application_keypad = false;
                acceptance.alternate_screen_scroll = false;
                let _ = state.profiles.update_profile(acceptance);
            }
            let _ = state.profiles.set_default("Pro");
            state.settings.font = "Global Poison Font".into();
            state.settings.font_size = 71.0;
            state.settings.cursor_shape = CursorShape::Underline;
            state.settings.cursor_blink = true;
            state.settings.scrollback_lines = 12_345;
            state.settings.terminal_type = "global-poison-term".into();
            state.settings.selected_profile = "Acceptance Profile".into();
            state.settings.startup_profile = "Acceptance Profile".into();
            if let Some(session) = state.sessions.active_mut() {
                session.profile_name = "Acceptance Profile".into();
            }
            if state.profiles.window_group("Acceptance Group").is_none() {
                let _ = state.profiles.add_window_group(WindowGroup {
                    name: "Acceptance Group".into(),
                    entries: vec![
                        WindowGroupEntry {
                            profile: "Homebrew".into(),
                            working_directory: Some("/tmp".into()),
                            columns: 80,
                            rows: 24,
                        },
                        WindowGroupEntry {
                            profile: "Acceptance Profile".into(),
                            working_directory: None,
                            columns: 100,
                            rows: 30,
                        },
                    ],
                });
            }
            if let Some(model) = state
                .profile_dropdown
                .model()
                .and_downcast::<gtk::StringList>()
            {
                if (0..model.n_items())
                    .all(|index| model.string(index).as_deref() != Some("Acceptance Profile"))
                {
                    model.append("Acceptance Profile");
                }
                if let Some(index) = (0..model.n_items())
                    .find(|index| model.string(*index).as_deref() == Some("Acceptance Profile"))
                {
                    state.profile_dropdown.set_selected(index);
                }
            }
        }
        let settings = show_settings_for_state(&state);
        let (active_before_settings_save, session_count_before_settings_save) = state
            .try_borrow()
            .map(|state| {
                let active = state
                    .sessions
                    .active()
                    .map(|tab| (tab.id, tab.profile_name.clone(), tab.child_pid));
                (active, state.sessions.tabs().len())
            })
            .unwrap_or((None, 0));
        let mut missing = Vec::new();
        let root = settings.child();
        let top_stack = root.as_ref().and_then(|root| {
            let first = root.first_child()?;
            let second = first.next_sibling()?;
            second.downcast::<gtk::Stack>().ok()
        });
        if let Some(stack) = &top_stack {
            stack.set_transition_type(gtk::StackTransitionType::None);
            for id in SETTINGS_PAGE_IDS {
                if stack.child_by_name(id).is_none() {
                    missing.push(id.to_owned());
                }
                stack.set_visible_child_name(id);
                while glib::MainContext::default().iteration(false) {}
            }
            stack.set_visible_child_name("profiles");
            if let Some(profile_pages) = root
                .as_ref()
                .and_then(|root| find_widget_by_name(root, "profile-pages"))
                .and_then(|widget| widget.downcast::<gtk::Stack>().ok())
            {
                profile_pages.set_transition_type(gtk::StackTransitionType::None);
                profile_pages.set_visible_child_name("text");
            }
            settings.queue_allocate();
            // Wayland delivers the next frame allocation asynchronously. Give
            // the compositor a bounded interval while continuing to dispatch
            // GTK events, then inspect the allocation that a user sees.
            let layout_deadline = Instant::now() + Duration::from_millis(350);
            while Instant::now() < layout_deadline {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
            while glib::MainContext::default().iteration(false) {}
        } else {
            missing.push("top-stack".to_owned());
        }
        for id in PROFILE_PAGE_IDS {
            if !widget_tree_has_name(root.as_ref().expect("settings window has a root"), id) {
                missing.push(id.to_owned());
            }
        }
        for id in [
            "startup-profile",
            "use-startup-window-group",
            "startup-window-group",
            "new-window-profile",
            "new-tab-profile",
            "custom-command",
            "profile-background-color",
            "text-palette",
            "profile-columns",
            "profile-custom-tab-title",
            "profile-tab-show-other-items",
            "profile-shell-command",
            "profile-run-inside-shell",
            "profile-shell",
            "profile-close-on-exit",
            "profile-ask-policy",
            "profile-exceptions",
            "shell-exit-policy",
            "keyboard-mappings",
            "keyboard-mapping-key",
            "keyboard-mapping-action",
            "advanced-terminal-type",
            "profile-delete-control-h",
            "profile-visual-bell-only-muted",
            "profile-encoding",
            "profile-ambiguous-width",
            "window-groups-list",
            "window-group-entries",
            "window-group-entry-add",
            "window-group-entry-remove",
            "window-group-entry-up",
            "window-group-entry-down",
            "encoding-compatibility-list",
            "encoding-options",
            "settings-save",
            "settings-cancel",
        ] {
            if !root
                .as_ref()
                .map(|root| widget_tree_has_name(root, id))
                .unwrap_or(false)
            {
                missing.push(id.to_owned());
            }
        }
        let shell_accessibility_metadata = root.as_ref().is_some_and(|root| {
            [
                "profile-shell-command",
                "profile-shell",
                "profile-close-on-exit",
                "profile-ask-policy",
                "profile-exceptions",
                "shell-exit-policy",
            ]
            .iter()
            .all(|name| {
                find_widget_by_name(root, name).is_some_and(|widget| {
                    gtk::test_accessible_has_property(&widget, gtk::AccessibleProperty::Label)
                        && gtk::test_accessible_has_property(
                            &widget,
                            gtk::AccessibleProperty::Description,
                        )
                })
            })
        });
        let mut safe_terminals = true;
        let profile_count = state
            .try_borrow()
            .map(|state| state.profiles.profiles().len())
            .unwrap_or(0);
        if let Ok(state) = state.try_borrow() {
            safe_terminals = state
                .terminals
                .values()
                .all(|terminal| !terminal.is_mouse_autohide());
        }
        let non_modal = !settings.is_modal();
        let settings_width = settings.width();
        let settings_height = settings.height();
        let settings_geometry_usable = settings_width >= 960 && settings_height >= 680;
        let profile_page_not_horizontally_scrolled = top_stack
            .as_ref()
            .and_then(|stack| stack.child_by_name("profiles"))
            .is_some_and(|page| !page.is::<gtk::ScrolledWindow>());
        let sidebar_width = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-sidebar"))
            .map_or(0, |sidebar| sidebar.width());
        let sidebar_geometry_usable = sidebar_width >= 270;
        let profile_tabs_width = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-page-switcher"))
            .map_or(0, |switcher| switcher.width());
        let profile_tabs_usable = profile_tabs_width >= 600;
        let mut minimum_profile_label_width = i32::MAX;
        let profile_labels_readable = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-list"))
            .and_then(|widget| widget.downcast::<gtk::ListBox>().ok())
            .is_some_and(|list| {
                let mut count = 0usize;
                let mut child = list.first_child();
                while let Some(widget) = child {
                    let next = widget.next_sibling();
                    let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() else {
                        return false;
                    };
                    let Some(label) = row.child().and_downcast::<gtk::Label>() else {
                        return false;
                    };
                    if label.text().is_empty() || label.width() < 120 || !label.is_visible() {
                        return false;
                    }
                    minimum_profile_label_width = minimum_profile_label_width.min(label.width());
                    count += 1;
                    child = next;
                }
                count == profile_count && count > 0
            });
        if minimum_profile_label_width == i32::MAX {
            minimum_profile_label_width = 0;
        }
        let mut minimum_profile_action_width = i32::MAX;
        let profile_actions_labeled = root.as_ref().is_some_and(|root| {
            [
                ("profile-add", "Add"),
                ("profile-duplicate", "Duplicate"),
                ("profile-delete", "Delete"),
                ("profile-import", "Import"),
                ("profile-export", "Export"),
                ("profile-default", "Set Default"),
                ("profile-reset", "Reset"),
            ]
            .into_iter()
            .all(|(name, expected)| {
                find_widget_by_name(root, name)
                    .and_then(|widget| widget.downcast::<gtk::Button>().ok())
                    .is_some_and(|button| {
                        minimum_profile_action_width =
                            minimum_profile_action_width.min(button.width());
                        button.label().as_deref() == Some(expected)
                            && button.width() >= 80
                            && button.is_visible()
                    })
            })
        });
        if minimum_profile_action_width == i32::MAX {
            minimum_profile_action_width = 0;
        }
        let profile_font_value = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-font"))
            .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
            .map(|widget| widget.text().to_string());
        let profile_font_loaded = profile_font_value.as_deref() == Some("Monospace");
        let profile_font_size_value = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-font-size"))
            .and_then(|widget| widget.downcast::<gtk::SpinButton>().ok())
            .map(|widget| widget.value());
        let profile_font_size_loaded =
            profile_font_size_value.is_some_and(|value| (value - 13.0).abs() < f64::EPSILON);
        let profile_cursor_shape_value = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-cursor-shape"))
            .and_then(|widget| widget.downcast::<gtk::DropDown>().ok())
            .map(|widget| widget.selected());
        let profile_cursor_shape_loaded = profile_cursor_shape_value == Some(1);
        let profile_cursor_blink_value = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-cursor-blink"))
            .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
            .map(|widget| widget.is_active());
        let profile_cursor_blink_loaded = profile_cursor_blink_value == Some(false);
        let profile_scrollback_value = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "profile-scrollback"))
            .and_then(|widget| widget.downcast::<gtk::SpinButton>().ok())
            .map(|widget| widget.value());
        let profile_scrollback_loaded =
            profile_scrollback_value.is_some_and(|value| (value - 20_000.0).abs() < f64::EPSILON);
        let profile_terminal_type_loaded = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "advanced-terminal-type"))
            .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
            .is_some_and(|widget| widget.text() == "acceptance-term");
        let profile_owned_values_loaded = profile_font_loaded
            && profile_font_size_loaded
            && profile_cursor_shape_loaded
            && profile_cursor_blink_loaded
            && profile_scrollback_loaded
            && profile_terminal_type_loaded;
        let mut window_group_editor_interaction = false;
        let mut shell_sensitivity_logic = false;
        let mut profile_editor_switch_before_save = false;
        let mut profile_switch_values_loaded = false;
        let mut renderer_owned_controls_truthful = false;
        let mut unavailable_controls_truthful = false;
        let global_shell_mode_before_save = state
            .try_borrow()
            .map(|state| state.settings.run_command_inside_shell)
            .unwrap_or(true);
        // A changed value followed by the actual Save button proves the
        // callback path is live; the callback normalizes and persists it.
        if let Some(root) = &root {
            let selected_acceptance_group = find_widget_by_name(root, "window-groups-list")
                .and_then(|widget| widget.downcast::<gtk::ListBox>().ok())
                .is_some_and(|list| {
                    let mut child = list.first_child();
                    while let Some(widget) = child {
                        let next = widget.next_sibling();
                        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                            if row
                                .child()
                                .and_downcast::<gtk::Label>()
                                .is_some_and(|label| label.text() == "Acceptance Group")
                            {
                                list.select_row(Some(row));
                                return true;
                            }
                        }
                        child = next;
                    }
                    false
                });
            if selected_acceptance_group {
                let entry_list = find_widget_by_name(root, "window-group-entries")
                    .and_then(|widget| widget.downcast::<gtk::ListBox>().ok());
                let had_two_entries = entry_list
                    .as_ref()
                    .is_some_and(|list| list.observe_children().n_items() == 2);
                if let Some(add) = find_widget_by_name(root, "window-group-entry-add")
                    .and_then(|widget| widget.downcast::<gtk::Button>().ok())
                {
                    add.emit_clicked();
                }
                if let Some(directory) = find_widget_by_name(root, "window-group-directory")
                    .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
                {
                    directory.set_text("/tmp/core-terminal-third");
                }
                if let Some(move_up) = find_widget_by_name(root, "window-group-entry-up")
                    .and_then(|widget| widget.downcast::<gtk::Button>().ok())
                {
                    move_up.emit_clicked();
                }
                let now_has_three_entries = entry_list
                    .as_ref()
                    .is_some_and(|list| list.observe_children().n_items() == 3);
                let moved_entry_is_second = entry_list
                    .as_ref()
                    .and_then(|list| list.row_at_index(1))
                    .and_then(|row| row.child().and_downcast::<gtk::Label>())
                    .is_some_and(|label| label.text().contains("/tmp/core-terminal-third"));
                window_group_editor_interaction =
                    had_two_entries && now_has_three_entries && moved_entry_is_second;
            }
            if let Some(color) = find_widget_by_name(root, "profile-background-color")
                .and_then(|widget| widget.downcast::<gtk::ColorDialogButton>().ok())
            {
                color.set_rgba(&gtk::gdk::RGBA::new(0.1, 0.2, 0.3, 1.0));
            }
            if let Some(columns) = find_widget_by_name(root, "profile-columns")
                .and_then(|widget| widget.downcast::<gtk::SpinButton>().ok())
            {
                columns.set_value(100.0);
            }
            for name in [
                "profile-tab-activity",
                "profile-tab-show-other-items",
                "profile-option-meta",
                "profile-visual-bell",
                "profile-visual-bell-only-muted",
                "profile-delete-control-h",
            ] {
                if let Some(toggle) = find_widget_by_name(root, name)
                    .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
                {
                    toggle.set_active(true);
                }
            }
            let command = find_widget_by_name(root, "profile-shell-command")
                .and_then(|widget| widget.downcast::<gtk::Entry>().ok());
            let run_inside = find_widget_by_name(root, "profile-run-inside-shell")
                .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok());
            let ask_policy = find_widget_by_name(root, "profile-ask-policy")
                .and_then(|widget| widget.downcast::<gtk::DropDown>().ok());
            let exceptions = find_widget_by_name(root, "profile-exceptions")
                .and_then(|widget| widget.downcast::<gtk::Entry>().ok());
            let close_on_exit = find_widget_by_name(root, "profile-close-on-exit")
                .and_then(|widget| widget.downcast::<gtk::DropDown>().ok());
            let exit_action = find_widget_by_name(root, "shell-exit-policy")
                .and_then(|widget| widget.downcast::<gtk::DropDown>().ok());
            if let (
                Some(command),
                Some(run_inside),
                Some(ask_policy),
                Some(exceptions),
                Some(close_on_exit),
                Some(exit_action),
            ) = (
                command,
                run_inside,
                ask_policy,
                exceptions,
                close_on_exit,
                exit_action,
            ) {
                command.set_text("");
                let blank_command_disables_mode = !run_inside.is_sensitive();
                command.set_text("printf '%s' acceptance");
                let command_enables_mode = run_inside.is_sensitive();
                run_inside.set_active(false);

                ask_policy.set_selected(0);
                let never_disables_exceptions = !exceptions.is_sensitive();
                ask_policy.set_selected(2);
                let non_exempt_enables_exceptions = exceptions.is_sensitive();
                exceptions.set_text("bash, tmux");

                close_on_exit.set_selected(3);
                let always_disables_fallback = !exit_action.is_sensitive();
                close_on_exit.set_selected(1);
                let conditional_enables_fallback = exit_action.is_sensitive();
                exit_action.set_selected(1);

                shell_sensitivity_logic = blank_command_disables_mode
                    && command_enables_mode
                    && never_disables_exceptions
                    && non_exempt_enables_exceptions
                    && always_disables_fallback
                    && conditional_enables_fallback;
            }
            if let Some(shell) = find_widget_by_name(root, "profile-shell")
                .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
            {
                shell.set_text("/bin/bash");
            }
            if let Some(locale) = find_widget_by_name(root, "profile-locale")
                .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
            {
                locale.set_text("en_US.UTF-8");
            }
            if let Some(blink) = find_widget_by_name(root, "profile-text-blink")
                .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
            {
                blink.set_active(false);
            }
            if let Some(width) = find_widget_by_name(root, "profile-ambiguous-width")
                .and_then(|widget| widget.downcast::<gtk::DropDown>().ok())
            {
                width.set_selected(1);
            }
            if let Some(toggle) = find_widget_by_name(root, "ctrl-number-tabs")
                .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
            {
                toggle.set_active(!toggle.is_active());
            }
            if let Some(startup) = find_widget_by_name(root, "startup-profile")
                .and_then(|widget| widget.downcast::<gtk::DropDown>().ok())
            {
                if let Some(model) = startup.model().and_downcast::<gtk::StringList>() {
                    if let Some(index) = string_list_position(&model, "Ocean") {
                        startup.set_selected(index);
                    }
                }
            }
            // Save while a non-active profile is visible. This exercises the
            // editor transition, commits the profile we just changed, and
            // proves the legacy Settings snapshot stays tied to the live
            // Acceptance Profile rather than whichever row is visible.
            profile_editor_switch_before_save = find_widget_by_name(root, "profile-list")
                .and_then(|widget| widget.downcast::<gtk::ListBox>().ok())
                .is_some_and(|list| {
                    let mut child = list.first_child();
                    while let Some(widget) = child {
                        let next = widget.next_sibling();
                        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                            if row
                                .child()
                                .and_downcast::<gtk::Label>()
                                .is_some_and(|label| label.text() == "Homebrew")
                            {
                                list.select_row(Some(row));
                                drain_pending_events();
                                return true;
                            }
                        }
                        child = next;
                    }
                    false
                });
            profile_switch_values_loaded = find_widget_by_name(root, "profile-font")
                .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
                .is_some_and(|widget| widget.text() == "DejaVu Sans Mono")
                && find_widget_by_name(root, "profile-font-size")
                    .and_then(|widget| widget.downcast::<gtk::SpinButton>().ok())
                    .is_some_and(|widget| (widget.value() - 11.0).abs() < f64::EPSILON)
                && find_widget_by_name(root, "profile-cursor-shape")
                    .and_then(|widget| widget.downcast::<gtk::DropDown>().ok())
                    .is_some_and(|widget| widget.selected() == 0)
                && find_widget_by_name(root, "profile-cursor-blink")
                    .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
                    .is_some_and(|widget| widget.is_active())
                && find_widget_by_name(root, "profile-scrollback")
                    .and_then(|widget| widget.downcast::<gtk::SpinButton>().ok())
                    .is_some_and(|widget| (widget.value() - 9_000.0).abs() < f64::EPSILON)
                && find_widget_by_name(root, "advanced-terminal-type")
                    .and_then(|widget| widget.downcast::<gtk::Entry>().ok())
                    .is_some_and(|widget| widget.text() == "homebrew-term");
            renderer_owned_controls_truthful = [
                "profile-antialias",
                "profile-use-bold-fonts",
                "profile-use-ansi",
                "profile-dynamic-colors",
                "profile-smooth-resize",
                "profile-alt-scroll",
                "profile-keypad",
            ]
            .into_iter()
            .all(|name| {
                find_widget_by_name(root, name)
                    .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
                    .is_some_and(|widget| widget.is_active() && !widget.is_sensitive())
            });
            unavailable_controls_truthful = [
                "profile-tab-show-ctrl-key",
                "profile-title-show-tty",
                "profile-title-show-ctrl-key",
                "profile-restore-rows",
            ]
            .into_iter()
            .all(|name| {
                find_widget_by_name(root, name)
                    .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
                    .is_some_and(|widget| !widget.is_active() && !widget.is_sensitive())
            });
            if let Some(save) = find_widget_by_name(root, "settings-save")
                .and_then(|widget| widget.downcast::<gtk::Button>().ok())
            {
                save.emit_clicked();
            }
        }
        let (active_session_preserved, startup_profile_independent, profile_default_preserved) =
            state
                .try_borrow()
                .map(|state| {
                    let active_after = state
                        .sessions
                        .active()
                        .map(|tab| (tab.id, tab.profile_name.clone(), tab.child_pid));
                    let active_identity_preserved =
                        match (&active_before_settings_save, &active_after) {
                            (
                                Some((before_id, before_profile, before_pid)),
                                Some((after_id, after_profile, after_pid)),
                            ) => {
                                before_id == after_id
                                    && before_profile == after_profile
                                    && (before_pid.is_none() || before_pid == after_pid)
                            }
                            _ => false,
                        };
                    (
                        active_identity_preserved
                            && state.sessions.tabs().len() == session_count_before_settings_save,
                        state.settings.startup_profile == "Ocean"
                            && state.settings.selected_profile == "Acceptance Profile",
                        state.profiles.default_profile_name() == "Pro",
                    )
                })
                .unwrap_or((false, false, false));
        open_tab(&state);
        let same_profile_new_tab = state
            .try_borrow()
            .ok()
            .and_then(|state| state.sessions.active().map(|tab| tab.profile_name.clone()))
            .as_deref()
            == Some("Acceptance Profile");
        let profile_file_written = ProfileStore::config_path()
            .map(|path| path.is_file())
            .unwrap_or(false);
        let restored_profiles =
            ProfileStore::config_path().and_then(|path| ProfileStore::load_from_path(path).ok());
        let profile_round_trip = restored_profiles
            .as_ref()
            .and_then(|store| store.profile("Acceptance Profile"))
            .map(|profile| {
                profile.columns == 100
                    && profile.close_on_exit == CloseOnExit::Clean
                    && profile.shell == "/bin/bash"
                    && profile.shell_command == "printf '%s' acceptance"
                    && !profile.run_inside_shell
                    && profile.ask_before_close_policy == AskBeforeClosePolicy::NonExempt
                    && profile.ask_before_close_exceptions == ["bash", "tmux"]
                    && profile.tab_title_show_other_items
                    && profile.option_as_meta
                    && profile.visual_bell
                    && profile.visual_bell_only_if_muted
                    && profile.delete_sends_control_h
                    && !profile.text_blink
                    && profile.ambiguous_width == 2
                    && profile.locale == "en_US.UTF-8"
                    && profile.background == "#1a334dff"
            })
            .unwrap_or(false);
        let non_editable_profile_values_preserved = restored_profiles
            .as_ref()
            .and_then(|store| store.profile("Acceptance Profile"))
            .is_some_and(|profile| {
                !profile.antialias
                    && !profile.use_bold_fonts
                    && !profile.use_ansi_colors
                    && !profile.dynamic_colors
                    && !profile.smooth_resize
                    && profile.restore_rows
                    && profile.restore_rows_limit == 77_700
                    && profile.restore_rows_bookmark == "acceptance-bookmark"
                    && profile.title_show_tty
                    && profile.title_show_ctrl_key
                    && profile.tab_title_show_ctrl_key
                    && !profile.application_keypad
                    && !profile.alternate_screen_scroll
            });
        let compatibility_fields_preserved = {
            let saved = Settings::load_user();
            saved.selected_profile == "Acceptance Profile"
                && saved.font == "Monospace"
                && (saved.font_size - 13.0).abs() < f64::EPSILON
                && saved.cursor_shape == CursorShape::IBeam
                && !saved.cursor_blink
                && saved.scrollback_lines == 20_000
                && saved.terminal_type == "acceptance-term"
        };
        let shell_policy_consolidated = restored_profiles
            .as_ref()
            .and_then(|store| store.profile("Acceptance Profile"))
            .is_some_and(|profile| {
                profile.close_on_exit == CloseOnExit::Clean
                    && profile.shell_exit_action == ShellExitAction::Keep
                    && !profile.close_on_clean_exit
                    && !profile.close_on_error
            });
        let shell_widgets_reloaded = restored_profiles.as_ref().is_some_and(|profiles| {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.profiles = profiles.clone();
                state.settings = Settings::load_user();
            } else {
                return false;
            }
            let reopened = show_settings_for_state(&state);
            drain_pending_events();
            let root = reopened.child();
            let restored = root.as_ref().is_some_and(|root| {
                let command = find_widget_by_name(root, "profile-shell-command")
                    .and_then(|widget| widget.downcast::<gtk::Entry>().ok());
                let run_inside = find_widget_by_name(root, "profile-run-inside-shell")
                    .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok());
                let shell = find_widget_by_name(root, "profile-shell")
                    .and_then(|widget| widget.downcast::<gtk::Entry>().ok());
                let close_on_exit = find_widget_by_name(root, "profile-close-on-exit")
                    .and_then(|widget| widget.downcast::<gtk::DropDown>().ok());
                let ask_policy = find_widget_by_name(root, "profile-ask-policy")
                    .and_then(|widget| widget.downcast::<gtk::DropDown>().ok());
                let exceptions = find_widget_by_name(root, "profile-exceptions")
                    .and_then(|widget| widget.downcast::<gtk::Entry>().ok());
                let exit_action = find_widget_by_name(root, "shell-exit-policy")
                    .and_then(|widget| widget.downcast::<gtk::DropDown>().ok());
                command.is_some_and(|widget| {
                    widget.text() == "printf '%s' acceptance" && widget.is_sensitive()
                }) && run_inside.is_some_and(|widget| !widget.is_active() && widget.is_sensitive())
                    && shell.is_some_and(|widget| widget.text() == "/bin/bash")
                    && close_on_exit.is_some_and(|widget| widget.selected() == 1)
                    && ask_policy.is_some_and(|widget| widget.selected() == 2)
                    && exceptions.is_some_and(|widget| {
                        widget.text() == "bash, tmux" && widget.is_sensitive()
                    })
                    && exit_action
                        .is_some_and(|widget| widget.selected() == 1 && widget.is_sensitive())
            });
            reopened.close();
            drain_pending_events();
            restored
        });
        let global_shell_mode_preserved =
            Settings::load_user().run_command_inside_shell == global_shell_mode_before_save;
        let window_group_round_trip = restored_profiles
            .as_ref()
            .and_then(|store| store.window_group("Acceptance Group"))
            .is_some_and(|group| {
                group.entries.len() == 3
                    && group.entries[1].working_directory.as_deref()
                        == Some("/tmp/core-terminal-third")
            });
        let standard_mappings_present = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "keyboard-mappings"))
            .and_then(|widget| widget.downcast::<gtk::ListView>().ok())
            .and_then(|view| view.model())
            .is_some_and(|model| model.n_items() >= 20);
        let encoding_rows_present = root
            .as_ref()
            .and_then(|root| find_widget_by_name(root, "encoding-options"))
            .and_then(|widget| widget.downcast::<gtk::ListBox>().ok())
            .is_some_and(|list| list.observe_children().n_items() >= 26);
        let runtime_profile_applied = state
            .try_borrow()
            .ok()
            .and_then(|state| active_terminal(&state))
            .is_some_and(|terminal| {
                terminal.delete_binding() == vte4::EraseBinding::AsciiBackspace
                    && terminal.text_blink_mode() == vte4::TextBlinkMode::Never
                    && terminal.cjk_ambiguous_width() == 2
                    && !terminal.is_mouse_autohide()
            });
        let (group_to_launch, sessions_before_group) = state
            .try_borrow_mut()
            .map(|mut state| {
                // Deliberately conflict with the group's profiles: explicit
                // group entries must win over normal new-tab policy.
                state.settings.new_tab_profile = "Ocean".into();
                (
                    state.profiles.window_group("Acceptance Group").cloned(),
                    state.sessions.tabs().len(),
                )
            })
            .unwrap_or((None, 0));
        let group_launch_explicit = group_to_launch.is_some_and(|group| {
            let expected = group.entries.clone();
            launch_window_group(&state, group);
            state.try_borrow().is_ok_and(|state| {
                let launched = state.sessions.tabs().get(sessions_before_group..);
                launched.is_some_and(|launched| {
                    launched.len() == expected.len()
                        && launched.iter().zip(&expected).all(|(tab, entry)| {
                            tab.profile_name == entry.profile
                                && tab.working_directory == entry.working_directory
                        })
                })
            })
        });
        let active_profile_synced_after_close = state
            .try_borrow()
            .ok()
            .and_then(|state| state.sessions.active().map(|tab| tab.id))
            .is_some_and(|id| {
                force_close_tab(&state, id);
                state.try_borrow().is_ok_and(|state| {
                    let Some(active_profile) =
                        state.sessions.active().map(|tab| tab.profile_name.as_str())
                    else {
                        return false;
                    };
                    let selected_name = dropdown_text(&state.profile_dropdown);
                    state.settings.selected_profile == active_profile
                        && selected_name.as_deref() == Some(active_profile)
                })
            });
        let passed = missing.is_empty()
            && safe_terminals
            && non_modal
            && settings_geometry_usable
            && profile_page_not_horizontally_scrolled
            && sidebar_geometry_usable
            && profile_tabs_usable
            && profile_labels_readable
            && profile_actions_labeled
            && profile_count > 0
            && profile_file_written
            && profile_round_trip
            && profile_owned_values_loaded
            && non_editable_profile_values_preserved
            && profile_editor_switch_before_save
            && profile_switch_values_loaded
            && renderer_owned_controls_truthful
            && unavailable_controls_truthful
            && compatibility_fields_preserved
            && shell_policy_consolidated
            && global_shell_mode_preserved
            && shell_sensitivity_logic
            && shell_widgets_reloaded
            && shell_accessibility_metadata
            && window_group_editor_interaction
            && window_group_round_trip
            && standard_mappings_present
            && encoding_rows_present
            && runtime_profile_applied
            && active_session_preserved
            && startup_profile_independent
            && profile_default_preserved
            && same_profile_new_tab
            && group_launch_explicit
            && active_profile_synced_after_close
            && close_before_spawn_cleanup
            && (background_session_cleanup || brokered_proxy_cleanup)
            && close_prompt_details_bounded
            && confirmation_accepted
            && stale_pending_revalidated
            && new_window_target_revalidated
            && overlapping_window_request_preserved
            && state_machine_probe_cleanup
            && tab_close_prompted
            && tab_close_cancelled
            && shell_exit_window_prompted
            && shell_exit_prompt_cancelled
            && exited_pid_cleared
            && protected_sibling_preserved
            && close_probe_cleanup;
        let report = format!(
            "status={} missing={:?} non_modal={} mouse_autohide_disabled={} settings_geometry={}x{} settings_geometry_usable={} profile_page_not_horizontally_scrolled={} sidebar_width={} sidebar_geometry_usable={} profile_tabs_width={} profile_tabs_usable={} minimum_profile_label_width={} profile_labels_readable={} minimum_profile_action_width={} profile_actions_labeled={} profiles={} profile_file_written={} profile_round_trip={} profile_owned_values_loaded={} profile_font_loaded={} profile_font_value={:?} profile_font_size_loaded={} profile_font_size_value={:?} profile_cursor_shape_loaded={} profile_cursor_shape_value={:?} profile_cursor_blink_loaded={} profile_cursor_blink_value={:?} profile_scrollback_loaded={} profile_scrollback_value={:?} profile_terminal_type_loaded={} non_editable_profile_values_preserved={} profile_editor_switch_before_save={} profile_switch_values_loaded={} renderer_owned_controls_truthful={} unavailable_controls_truthful={} compatibility_fields_preserved={} shell_policy_consolidated={} global_shell_mode_preserved={} shell_sensitivity_logic={} shell_widgets_reloaded={} shell_accessibility_metadata={} window_group_editor_interaction={} window_group_round_trip={} standard_mappings_present={} encoding_rows_present={} runtime_profile_applied={} active_session_preserved={} startup_profile_independent={} profile_default_preserved={} same_profile_new_tab={} group_launch_explicit={} active_profile_synced_after_close={} close_before_spawn_cleanup={} background_session_cleanup={} brokered_proxy_cleanup={} close_prompt_details_bounded={} confirmation_accepted={} stale_pending_revalidated={} new_window_target_revalidated={} overlapping_window_request_preserved={} state_machine_probe_cleanup={} tab_close_prompted={} tab_close_cancelled={} shell_exit_window_prompted={} shell_exit_prompt_cancelled={} exited_pid_cleared={} protected_sibling_preserved={} close_probe_cleanup={}\n",
            if passed { "PASS" } else { "FAIL" },
            missing,
            non_modal,
            safe_terminals,
            settings_width,
            settings_height,
            settings_geometry_usable,
            profile_page_not_horizontally_scrolled,
            sidebar_width,
            sidebar_geometry_usable,
            profile_tabs_width,
            profile_tabs_usable,
            minimum_profile_label_width,
            profile_labels_readable,
            minimum_profile_action_width,
            profile_actions_labeled,
            profile_count,
            profile_file_written,
            profile_round_trip,
            profile_owned_values_loaded,
            profile_font_loaded,
            profile_font_value,
            profile_font_size_loaded,
            profile_font_size_value,
            profile_cursor_shape_loaded,
            profile_cursor_shape_value,
            profile_cursor_blink_loaded,
            profile_cursor_blink_value,
            profile_scrollback_loaded,
            profile_scrollback_value,
            profile_terminal_type_loaded,
            non_editable_profile_values_preserved,
            profile_editor_switch_before_save,
            profile_switch_values_loaded,
            renderer_owned_controls_truthful,
            unavailable_controls_truthful,
            compatibility_fields_preserved,
            shell_policy_consolidated,
            global_shell_mode_preserved,
            shell_sensitivity_logic,
            shell_widgets_reloaded,
            shell_accessibility_metadata,
            window_group_editor_interaction,
            window_group_round_trip,
            standard_mappings_present,
            encoding_rows_present,
            runtime_profile_applied,
            active_session_preserved,
            startup_profile_independent,
            profile_default_preserved,
            same_profile_new_tab,
            group_launch_explicit,
            active_profile_synced_after_close,
            close_before_spawn_cleanup,
            background_session_cleanup,
            brokered_proxy_cleanup,
            close_prompt_details_bounded,
            confirmation_accepted,
            stale_pending_revalidated,
            new_window_target_revalidated,
            overlapping_window_request_preserved,
            state_machine_probe_cleanup,
            tab_close_prompted,
            tab_close_cancelled,
            shell_exit_window_prompted,
            shell_exit_prompt_cancelled,
            exited_pid_cleared,
            protected_sibling_preserved,
            close_probe_cleanup,
        );
        if let Some(report_path) =
            std::env::var_os("CORE_TERMINAL_ACCEPTANCE_REPORT").map(std::path::PathBuf::from)
        {
            let write_result = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&report_path)
                .and_then(|mut output| std::io::Write::write_all(&mut output, report.as_bytes()));
            if let Err(error) = write_result {
                eprintln!(
                    "Core Terminal could not create acceptance report {}: {error}",
                    report_path.display()
                );
            }
        } else {
            eprintln!("CORE_TERMINAL_ACCEPTANCE_REPORT is required in acceptance mode");
        }
        settings.close();
        app.quit();
    });
}

fn widget_tree_has_name(root: &gtk::Widget, name: &str) -> bool {
    if root.widget_name() == name {
        return true;
    }
    let mut child = root.first_child();
    while let Some(candidate) = child {
        if widget_tree_has_name(&candidate, name) {
            return true;
        }
        child = candidate.next_sibling();
    }
    false
}

fn find_widget_by_name(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(candidate) = child {
        if let Some(found) = find_widget_by_name(&candidate, name) {
            return Some(found);
        }
        child = candidate.next_sibling();
    }
    None
}

fn install_window_actions(
    app: &gtk::Application,
    window: &gtk::ApplicationWindow,
    state: &Rc<RefCell<UiState>>,
) {
    let new_window = gio::SimpleAction::new("new-window", None);
    let action_state = state.clone();
    let action_app = app.clone();
    new_window.connect_activate(move |_, _| {
        let (display_name, directory) = {
            let state = action_state.borrow();
            (
                state
                    .window
                    .title()
                    .map(|title| title.to_string())
                    .unwrap_or_else(|| "Core Terminal".to_owned()),
                state
                    .settings
                    .new_window_same_directory
                    .then(|| {
                        state
                            .sessions
                            .active()
                            .and_then(|tab| tab.working_directory.clone())
                    })
                    .flatten(),
            )
        };
        build_window_with_directory(&action_app, &display_name, true, directory);
    });
    window.add_action(&new_window);

    let new_tab = gio::SimpleAction::new("new-tab", None);
    let action_state = state.clone();
    new_tab.connect_activate(move |_, _| open_tab(&action_state));
    window.add_action(&new_tab);

    let close = gio::SimpleAction::new("close-tab", None);
    let action_state = state.clone();
    close.connect_activate(move |_, _| close_current_tab(&action_state));
    window.add_action(&close);

    let next = gio::SimpleAction::new("next-tab", None);
    let action_state = state.clone();
    next.connect_activate(move |_, _| switch_tab(&action_state, true));
    window.add_action(&next);

    let previous = gio::SimpleAction::new("previous-tab", None);
    let action_state = state.clone();
    previous.connect_activate(move |_, _| switch_tab(&action_state, false));
    window.add_action(&previous);

    let search = gio::SimpleAction::new("search", None);
    let action_state = state.clone();
    search.connect_activate(move |_, _| show_search(&action_state));
    window.add_action(&search);

    let settings = gio::SimpleAction::new("settings", None);
    let action_state = state.clone();
    settings.connect_activate(move |_, _| {
        show_settings_for_state(&action_state);
    });
    window.add_action(&settings);

    let restore = gio::SimpleAction::new("restore-profiles", None);
    let action_state = state.clone();
    restore.connect_activate(move |_, _| restore_default_profiles(&action_state));
    window.add_action(&restore);

    let about = gio::SimpleAction::new("about", None);
    let parent = window.clone();
    about.connect_activate(move |_, _| show_about(&parent));
    window.add_action(&about);

    app.set_accels_for_action("win.new-tab", &["<Primary>t"]);
    app.set_accels_for_action("win.close-tab", &["<Primary>w"]);
    app.set_accels_for_action("win.search", &["<Primary>f"]);
    app.set_accels_for_action("win.settings", &["<Primary>comma"]);
    app.set_accels_for_action(
        "win.next-tab",
        &["<Primary>Page_Down", "<Primary><Shift>bracketright"],
    );
    app.set_accels_for_action(
        "win.previous-tab",
        &["<Primary>Page_Up", "<Primary><Shift>bracketleft"],
    );
}

fn show_settings_for_state(state: &Rc<RefCell<UiState>>) -> gtk::Window {
    let (parent, mut settings, profiles) = {
        let state = state.borrow();
        (
            state.window.clone().upcast::<gtk::Window>(),
            state.settings.clone(),
            state.profiles.clone(),
        )
    };
    if let Some(active_profile) = state
        .borrow()
        .sessions
        .active()
        .map(|tab| tab.profile_name.clone())
        .filter(|name| profiles.profile(name).is_some())
    {
        settings.selected_profile = active_profile.clone();
    }
    let save_state = state.clone();
    let launch_state = state.clone();
    show_settings(
        &parent,
        &settings,
        profiles,
        move |new_settings, profiles| {
            let mut state = save_state.borrow_mut();
            let mut new_settings = new_settings.normalize();
            let old_settings = state.settings.clone();
            let old_profiles = state.profiles.clone();
            let global_runtime_changed =
                runtime_terminal_settings_changed(&old_settings, &new_settings);
            state.profiles = profiles;
            let fallback_profile = if state
                .profiles
                .profile(&new_settings.startup_profile)
                .is_some()
            {
                new_settings.startup_profile.clone()
            } else {
                state.profiles.selected_name().to_owned()
            };
            let assignments = state
                .sessions
                .tabs()
                .iter()
                .map(|tab| (tab.id, tab.profile_name.clone()))
                .collect::<Vec<_>>();
            let mut reassigned_sessions = Vec::new();
            for (id, profile_name) in assignments {
                if state.profiles.profile(&profile_name).is_none() {
                    state.sessions.set_profile(id, fallback_profile.clone());
                    reassigned_sessions.push(id.get());
                }
            }
            let active_profile = state
                .sessions
                .active()
                .map(|tab| tab.profile_name.clone())
                .filter(|name| state.profiles.profile(name).is_some())
                .unwrap_or_else(|| fallback_profile.clone());
            new_settings.selected_profile = active_profile.clone();
            state.settings = new_settings.clone();
            if let Some(index) = state
                .profiles
                .names()
                .position(|name| name == active_profile)
            {
                state.profile_dropdown.set_selected(index as u32);
            }
            let _ = state.settings.save_user();
            save_user_profiles(&state.profiles);
            let terminals = state
                .sessions
                .tabs()
                .iter()
                .filter_map(|tab| {
                    let profile = state.profiles.profile(&tab.profile_name)?.clone();
                    let profile_changed = old_profiles.profile(&tab.profile_name) != Some(&profile);
                    let assignment_changed = reassigned_sessions.contains(&tab.id.get());
                    if !runtime_profile_requires_reapply(
                        global_runtime_changed,
                        profile_changed,
                        assignment_changed,
                    ) {
                        return None;
                    }
                    Some((tab.id, state.terminals.get(&tab.id.get())?.clone(), profile))
                })
                .collect::<Vec<_>>();
            drop(state);
            for (id, terminal, profile) in terminals {
                reapply_profile_without_resize(&terminal, &profile, &new_settings);
                update_tab_title(&save_state, id, &terminal);
            }
        },
        move |group| launch_window_group(&launch_state, group),
    )
}

/// Launch every entry in a saved group through the explicit tab/PTY path.
/// Group entries never mutate or depend on normal new-tab preferences.
fn launch_window_group(state: &Rc<RefCell<UiState>>, group: WindowGroup) {
    for entry in group.entries {
        if state.borrow().profiles.profile(&entry.profile).is_none() {
            continue;
        }
        open_tab_with_spec(state, TabLaunchSpec::from_window_group_entry(entry));
    }
}

fn restore_default_profiles(state: &Rc<RefCell<UiState>>) {
    let mut state = state.borrow_mut();
    state.profiles.restore_defaults();
    let profile = state.profiles.selected().clone();
    let width = state.settings.window_width;
    let height = state.settings.window_height;
    let profile_settings = Settings::from_profile(&profile);
    // Restoring profiles must not erase unrelated global preferences.
    state.settings.selected_profile = profile.name.clone();
    state.settings.startup_profile = profile.name.clone();
    state.settings.font = profile_settings.font;
    state.settings.font_size = profile_settings.font_size;
    state.settings.cursor_shape = profile_settings.cursor_shape;
    state.settings.cursor_blink = profile_settings.cursor_blink;
    state.settings.scrollback_lines = profile_settings.scrollback_lines;
    state.settings.window_width = width;
    state.settings.window_height = height;
    if let Some(model) = state
        .profile_dropdown
        .model()
        .and_downcast::<gtk::StringList>()
    {
        let names = state
            .profiles
            .names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        model.splice(0, model.n_items(), &name_refs);
        state.profile_dropdown.set_selected(
            names
                .iter()
                .position(|name| name == &profile.name)
                .unwrap_or(0) as u32,
        );
    }
    if let Some(session) = state.sessions.active_mut() {
        session.profile_name = profile.name.clone();
    }
    if let Some(terminal) = active_terminal(&state) {
        apply_profile(&terminal, &profile, &state.settings);
    }
    let _ = state.settings.save_user();
    save_user_profiles(&state.profiles);
}

#[allow(deprecated)]
fn show_about(parent: &gtk::ApplicationWindow) {
    let dialog = gtk::AboutDialog::builder()
        .transient_for(parent)
        .modal(false)
        .program_name("Core Terminal")
        .version(env!("CARGO_PKG_VERSION"))
        .license_type(gtk::License::Gpl30)
        .comments(
            "An independent GTK4/VTE terminal for Linux. Not affiliated with or endorsed by Apple.",
        )
        .logo_icon_name(APPLICATION_ID)
        .build();
    enforce_non_modal(&dialog);
    dialog.present();
}

#[allow(deprecated)]
fn show_search(state: &Rc<RefCell<UiState>>) {
    let (parent, terminal) = {
        let state = state.borrow();
        (state.window.clone(), active_terminal(&state))
    };
    let Some(terminal) = terminal else {
        return;
    };
    let dialog = gtk::Dialog::builder()
        .title("Find in Terminal")
        .transient_for(&parent)
        .destroy_with_parent(true)
        .modal(false)
        .build();
    enforce_non_modal(&dialog);
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Find Next", gtk::ResponseType::Accept);
    let entry = gtk::Entry::builder()
        .placeholder_text("Search text or regular expression")
        .activates_default(true)
        .build();
    dialog.set_default_response(gtk::ResponseType::Accept);
    dialog.content_area().append(&entry);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let Ok(regex) = vte4::Regex::for_search(entry.text().as_str(), 0) {
                terminal.search_set_regex(Some(&regex), 0);
                terminal.search_set_wrap_around(true);
                terminal.search_find_next();
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn active_terminal(state: &UiState) -> Option<vte4::Terminal> {
    state
        .sessions
        .active()
        .and_then(|tab| state.terminals.get(&tab.id.get()))
        .cloned()
}

fn sync_active_profile_ui(state: &Rc<RefCell<UiState>>) {
    let Some((dropdown, index)) = (|| {
        let mut state = state.try_borrow_mut().ok()?;
        let profile_name = state.sessions.active()?.profile_name.clone();
        state.profiles.profile(&profile_name)?;
        state.settings.selected_profile = profile_name.clone();
        let index = state
            .profiles
            .names()
            .position(|name| name == profile_name)? as u32;
        Some((state.profile_dropdown.clone(), index))
    })() else {
        return;
    };
    if dropdown.selected() != index {
        dropdown.set_selected(index);
    }
}

fn switch_tab(state: &Rc<RefCell<UiState>>, next: bool) {
    let mut state_mut = state.borrow_mut();
    let id = if next {
        state_mut.sessions.next_tab().map(|tab| tab.id)
    } else {
        state_mut.sessions.previous_tab().map(|tab| tab.id)
    };
    if let Some(id) = id {
        let stack = state_mut.stack.clone();
        drop(state_mut);
        stack.set_visible_child_name(&format!("tab-{}", id.get()));
        sync_active_profile_ui(state);
    }
}

fn switch_tab_index(state: &Rc<RefCell<UiState>>, index: usize) {
    let mut state_mut = state.borrow_mut();
    let Some(tab) = state_mut.sessions.tabs().get(index) else {
        return;
    };
    let id = tab.id;
    state_mut.sessions.select_tab(index);
    let stack = state_mut.stack.clone();
    drop(state_mut);
    stack.set_visible_child_name(&format!("tab-{}", id.get()));
    sync_active_profile_ui(state);
}

fn close_current_tab(state: &Rc<RefCell<UiState>>) {
    let id = {
        let state = state.borrow();
        state
            .stack
            .visible_child_name()
            .and_then(|name| tab_id(&name))
            .or_else(|| state.sessions.active().map(|tab| tab.id))
    };
    if let Some(id) = id {
        close_tab(state, id);
    }
}

fn tab_id(name: &str) -> Option<SessionId> {
    let value = name.strip_prefix("tab-")?.parse::<u64>().ok()?;
    Some(SessionId::from(value))
}

#[allow(deprecated)]
fn open_tab(state: &Rc<RefCell<UiState>>) {
    let spec = {
        let state = state.borrow();
        let profile_name =
            resolve_new_tab_profile(&state.settings, &state.sessions, &state.profiles);
        let working_directory = state
            .settings
            .new_tab_same_directory
            .then(|| {
                state
                    .sessions
                    .active()
                    .and_then(|tab| tab.working_directory.clone())
            })
            .flatten();
        TabLaunchSpec::new(profile_name, working_directory)
    };
    open_tab_with_spec(state, spec);
}

#[allow(deprecated)]
fn open_tab_with_spec(state: &Rc<RefCell<UiState>>, spec: TabLaunchSpec) {
    if state.borrow().closing {
        return;
    }
    let (id, terminal, spawn_options) = {
        let mut state_mut = state.borrow_mut();
        let profile_name = if state_mut.profiles.profile(&spec.profile_name).is_some() {
            spec.profile_name
        } else {
            resolve_new_tab_profile(
                &state_mut.settings,
                &state_mut.sessions,
                &state_mut.profiles,
            )
        };
        let working_directory = spec.working_directory;
        let id = state_mut
            .sessions
            .open_tab(&profile_name, working_directory.as_deref());
        let terminal = vte4::Terminal::new();
        enforce_terminal_input_safety(&terminal);
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        let profile = state_mut.profiles.profile(&profile_name).cloned();
        if let Some(profile) = &profile {
            apply_profile(&terminal, profile, &state_mut.settings);
        }
        if let Some((columns, rows)) = spec.size {
            terminal.set_size(columns as i64, rows as i64);
        }
        // General settings provide the baseline. Profile-owned values take
        // precedence only when the profile actually specifies them, so the
        // global custom command and login shell remain useful for profiles
        // that leave those fields at their defaults.
        let mut spawn_options =
            core::SpawnOptions::from_settings(&state_mut.settings, working_directory.as_deref());
        if let Some(profile) = &profile {
            let profile_options =
                core::SpawnOptions::from_profile(profile, working_directory.as_deref());
            spawn_options.terminal_type = profile_options.terminal_type;
            if !profile.shell.trim().is_empty() {
                spawn_options.shell = profile_options.shell;
            }
            if !profile.shell_command.trim().is_empty() {
                spawn_options.custom_command = profile_options.custom_command;
                spawn_options.run_command_inside_shell = profile_options.run_command_inside_shell;
            }
            if profile.set_locale_environment {
                spawn_options.locale = profile_options.locale;
            }
        }
        let surface = terminal_surface(
            &terminal,
            profile
                .as_ref()
                .and_then(|profile| profile.background_image_path.as_deref()),
            profile
                .as_ref()
                .map(|profile| profile.background_image_mode),
            profile
                .as_ref()
                .map(|profile| profile.background_alpha)
                .unwrap_or(1.0),
        );
        let page_name = format!("tab-{}", id.get());
        state_mut
            .stack
            .add_titled(&surface, Some(&page_name), "Terminal");
        state_mut.stack.set_visible_child_name(&page_name);
        state_mut.terminals.insert(id.get(), terminal.clone());
        state_mut.pending_spawns.insert(id.get());
        if spawn_options.custom_command.is_none() {
            let login_shell = spawn_options
                .shell
                .clone()
                .unwrap_or_else(core::login_shell);
            if let Some(identity) = core::expected_executable_identity(&login_shell) {
                state_mut.login_shell_identities.insert(id.get(), identity);
            }
        }
        (id, terminal, spawn_options)
    };
    sync_active_profile_ui(state);
    connect_terminal_shortcuts(&terminal, state.clone(), id);
    let title_state = state.clone();
    terminal
        .connect_window_title_changed(move |terminal| update_tab_title(&title_state, id, terminal));
    let directory_state = state.clone();
    terminal.connect_current_directory_uri_changed(move |terminal| {
        update_tab_directory(&directory_state, id, terminal);
        update_tab_title(&directory_state, id, terminal);
    });
    let bell_state = state.clone();
    terminal.connect_bell(move |terminal| handle_terminal_bell(&bell_state, id, terminal));
    let activity_state = state.clone();
    terminal
        .connect_contents_changed(move |terminal| mark_tab_activity(&activity_state, id, terminal));
    update_tab_title(state, id, &terminal);
    let exit_state = state.clone();
    terminal.connect_child_exited(move |_, status| {
        notify_background_exit(&exit_state, id, status);
        // The child-exited signal is authoritative. Clear the PID before any
        // close policy runs so an exited tab never appears as a live blocker.
        {
            let mut state = exit_state.borrow_mut();
            if state.pending_spawns.remove(&id.get()) {
                state.exited_before_spawn_callbacks.insert(id.get());
            }
            state.child_process_identities.remove(&id.get());
            state.sessions.clear_child_pid(id);
        }
        let decision = {
            let state = exit_state.borrow();
            state
                .profiles
                .profile(
                    &state
                        .sessions
                        .tab(id)
                        .map(|tab| tab.profile_name.clone())
                        .unwrap_or_default(),
                )
                .map(|profile| core::child_exit_decision(profile, status))
                .unwrap_or(core::ChildExitDecision::CloseTab)
        };
        match decision {
            core::ChildExitDecision::CloseWindow => {
                let window = exit_state.borrow().window.clone();
                window.close();
            }
            core::ChildExitDecision::CloseTab => force_close_tab(&exit_state, id),
            core::ChildExitDecision::Keep => {}
            core::ChildExitDecision::Ask => show_child_exit_prompt(&exit_state, id, status),
        }
    });
    // Connect all lifecycle observers before spawning. A direct command such
    // as `/bin/true` can exit before the next main-loop turn; registering the
    // child-exited handler only after spawn would lose that event.
    let spawn_state = state.clone();
    core::spawn_terminal(&terminal, &spawn_options, move |result| match result {
        Ok(pid) => {
            let mut state = spawn_state.borrow_mut();
            let exited_before_callback = state.exited_before_spawn_callbacks.remove(&id.get());
            let was_pending = state.pending_spawns.remove(&id.get());
            let session_exists = state.sessions.tab(id).is_some();
            match spawn_callback_action(
                exited_before_callback,
                state.closing,
                session_exists,
                was_pending,
            ) {
                SpawnCallbackAction::IgnoreExitedChild => {
                    // VTE already emitted child-exited for this exact spawn.
                    // The returned PID is stale lifecycle bookkeeping and
                    // must never be resolved after the kernel can recycle it.
                }
                SpawnCallbackAction::TerminateClosedTab => {
                    drop(state);
                    if let Some(identity) = core::child_process_identity(pid) {
                        terminate_child_async(identity);
                    }
                    if std::env::var_os("CORE_TERMINAL_ACCEPTANCE").is_some() {
                        ACCEPTANCE_CLOSED_SPAWN_RESOLUTIONS.fetch_add(1, Ordering::SeqCst);
                    }
                }
                SpawnCallbackAction::StoreLiveChild => {
                    state.sessions.set_child_pid(id, Some(pid));
                    if let Some(identity) = core::child_process_identity(pid) {
                        state.child_process_identities.insert(id.get(), identity);
                    }
                }
                SpawnCallbackAction::IgnoreLateCallback => {
                    // A very short-lived command may emit child-exited before
                    // an older profile reaches this callback. Never resurrect
                    // its PID as a live process.
                }
            }
        }
        Err(error) => {
            if let Ok(mut state) = spawn_state.try_borrow_mut() {
                state.pending_spawns.remove(&id.get());
                state.exited_before_spawn_callbacks.remove(&id.get());
            }
            eprintln!("Core Terminal: failed to start tab {id:?}: {error}");
        }
    });
}

fn show_child_exit_prompt(state: &Rc<RefCell<UiState>>, id: SessionId, status: i32) {
    let parent = state.borrow().window.clone();
    let decoded = core::decode_child_exit_status(status);
    let status_text = match (decoded.code, decoded.signal) {
        (Some(code), _) => format!("exit code {code}"),
        (_, Some(signal)) => format!("signal {signal}"),
        _ => "unknown status".to_owned(),
    };
    let dialog = gtk::Window::builder()
        .title("Terminal process finished")
        .transient_for(&parent)
        .destroy_with_parent(true)
        .modal(false)
        .default_width(420)
        .default_height(160)
        .build();
    enforce_non_modal(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    let label = gtk::Label::new(Some(&format!(
        "The shell finished with {status_text}. Keep this tab open?"
    )));
    label.set_wrap(true);
    label.set_xalign(0.0);
    content.append(&label);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let keep = gtk::Button::with_label("Keep Tab");
    let close = gtk::Button::with_label("Close Tab");
    let close_dialog = dialog.clone();
    keep.connect_clicked(move |_| close_dialog.close());
    let close_dialog = dialog.clone();
    let close_state = state.clone();
    close.connect_clicked(move |_| {
        force_close_tab(&close_state, id);
        close_dialog.close();
    });
    actions.append(&keep);
    actions.append(&close);
    content.append(&actions);
    dialog.set_child(Some(&content));
    dialog.present();
}

#[allow(deprecated)]
fn update_tab_directory(state: &Rc<RefCell<UiState>>, id: SessionId, terminal: &vte4::Terminal) {
    let directory = terminal
        .current_directory_uri()
        .and_then(|uri| gio::File::for_uri(&uri).path())
        .map(|path| path.to_string_lossy().into_owned());
    state
        .borrow_mut()
        .sessions
        .set_working_directory(id, directory.as_deref());
}

fn notify_background_exit(state: &Rc<RefCell<UiState>>, id: SessionId, status: i32) {
    let state = state.borrow();
    let (profile_notifications, urgent) = state
        .sessions
        .tab(id)
        .and_then(|tab| state.profiles.profile(&tab.profile_name))
        .map(|profile| {
            (
                profile.background_notifications || profile.urgency_hint,
                profile.urgency_hint,
            )
        })
        .unwrap_or((false, false));
    if (!state.settings.background_notifications && !profile_notifications)
        || state.window.is_active()
    {
        return;
    }
    let Some(application) = state.window.application() else {
        return;
    };
    let notification = gio::Notification::new("Terminal command finished");
    let decoded = core::decode_child_exit_status(status);
    let detail = match (decoded.code, decoded.signal) {
        (Some(code), _) => format!("exit code {code}"),
        (_, Some(signal)) => format!("signal {signal}"),
        _ => "an unknown status".to_owned(),
    };
    notification.set_body(Some(&format!(
        "The background terminal finished with {detail}."
    )));
    if urgent {
        notification.set_priority(gio::NotificationPriority::Urgent);
    }
    application.send_notification(Some("background-command-finished"), &notification);
}

fn handle_terminal_bell(state: &Rc<RefCell<UiState>>, id: SessionId, terminal: &vte4::Terminal) {
    let (visual, notify, urgent) = {
        let state = state.borrow();
        state
            .sessions
            .tab(id)
            .and_then(|tab| state.profiles.profile(&tab.profile_name))
            .map(|profile| {
                let audible = state.settings.audible_bell || profile.audible_bell;
                (
                    profile.visual_bell && (!profile.visual_bell_only_if_muted || !audible),
                    (profile.background_notifications || profile.urgency_hint)
                        && !state.window.is_active(),
                    profile.urgency_hint,
                )
            })
            .unwrap_or((false, false, false))
    };
    if visual {
        terminal.add_css_class("core-visual-bell");
        let terminal = terminal.downgrade();
        glib::timeout_add_local_once(Duration::from_millis(140), move || {
            if let Some(terminal) = terminal.upgrade() {
                terminal.remove_css_class("core-visual-bell");
            }
        });
    }
    if notify {
        let state = state.borrow();
        if let Some(application) = state.window.application() {
            let notification = gio::Notification::new("Terminal bell");
            notification.set_body(Some("A background terminal requested attention."));
            if urgent {
                notification.set_priority(gio::NotificationPriority::Urgent);
            }
            application.send_notification(Some("background-terminal-bell"), &notification);
        }
    }
}

fn mark_tab_activity(state: &Rc<RefCell<UiState>>, id: SessionId, terminal: &vte4::Terminal) {
    let state = state.borrow();
    let enabled = state
        .sessions
        .tab(id)
        .and_then(|tab| state.profiles.profile(&tab.profile_name))
        .is_some_and(|profile| profile.tab_title_show_activity);
    let active = state.sessions.active().is_some_and(|tab| tab.id == id);
    if let Some(page_child) = stack_page_child(&state.stack, terminal) {
        state
            .stack
            .page(&page_child)
            .set_needs_attention(enabled && !active);
    }
}

#[allow(deprecated)]
fn update_tab_title(state: &Rc<RefCell<UiState>>, id: SessionId, terminal: &vte4::Terminal) {
    let reported_title = terminal
        .window_title()
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.to_string());
    let directory = terminal
        .current_directory_uri()
        .and_then(|uri| gio::File::for_uri(&uri).path())
        .map(|path| path.to_string_lossy().into_owned());
    let directory_name = directory.as_deref().and_then(|directory| {
        Path::new(directory)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| Some(directory.to_owned()))
    });
    let (profile, active) = {
        let state = state.borrow();
        let profile = state
            .sessions
            .tab(id)
            .and_then(|tab| state.profiles.profile(&tab.profile_name))
            .cloned();
        let active = state.sessions.active().is_some_and(|tab| tab.id == id);
        (profile, active)
    };
    let Some(profile) = profile else {
        return;
    };
    let shell = if profile.shell.trim().is_empty() {
        core::login_shell()
    } else {
        profile.shell.clone()
    };
    let shell_name = Path::new(&shell)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(shell);

    let mut tab_parts = Vec::new();
    let has_custom_title = profile.tab_title_policy == crate::profiles::TabTitlePolicy::Custom
        && !profile.custom_tab_title.trim().is_empty();
    if has_custom_title {
        push_title_part(&mut tab_parts, profile.custom_tab_title.trim());
    }
    if !has_custom_title || profile.tab_title_show_other_items {
        if profile.tab_title_show_profile {
            push_title_part(&mut tab_parts, &profile.name);
        }
        if profile.tab_title_show_shell {
            push_title_part(&mut tab_parts, &shell_name);
        }
        if profile.tab_title_show_path {
            if let Some(directory) = &directory {
                push_title_part(&mut tab_parts, directory);
            }
        } else if profile.tab_title_show_directory {
            if let Some(directory) = &directory_name {
                push_title_part(&mut tab_parts, directory);
            }
        }
        if profile.tab_title_show_job
            || profile.tab_title_show_process
            || profile.tab_title_show_arguments
        {
            if let Some(title) = &reported_title {
                push_title_part(&mut tab_parts, title);
            }
        }
        if profile.tab_title_show_dimensions {
            push_title_part(
                &mut tab_parts,
                &format!("{}×{}", terminal.column_count(), terminal.row_count()),
            );
        }
    }
    if tab_parts.is_empty() {
        push_title_part(
            &mut tab_parts,
            reported_title
                .as_deref()
                .unwrap_or_else(|| directory_name.as_deref().unwrap_or("Terminal")),
        );
    }
    let tab_title = tab_parts.join(" — ");

    let mut window_parts = Vec::new();
    if !profile.custom_window_title.trim().is_empty() {
        push_title_part(&mut window_parts, profile.custom_window_title.trim());
    }
    if profile.title_show_profile {
        push_title_part(&mut window_parts, &profile.name);
    }
    if profile.title_show_shell {
        push_title_part(&mut window_parts, &shell_name);
    }
    if profile.title_show_path || profile.title_show_working_directory {
        if let Some(directory) = &directory {
            push_title_part(&mut window_parts, directory);
        }
    } else if profile.title_show_directory {
        if let Some(directory) = &directory_name {
            push_title_part(&mut window_parts, directory);
        }
    }
    if profile.title_show_process || profile.title_show_arguments {
        if let Some(title) = &reported_title {
            push_title_part(&mut window_parts, title);
        }
    }
    if profile.title_show_dimensions {
        push_title_part(
            &mut window_parts,
            &format!("{}×{}", terminal.column_count(), terminal.row_count()),
        );
    }
    if window_parts.is_empty() {
        push_title_part(&mut window_parts, "Core Terminal");
    }
    let window_title = window_parts.join(" — ");
    let mut state = state.borrow_mut();
    state.sessions.set_title(id, &tab_title);
    if let Some(terminal) = state.terminals.get(&id.get()) {
        if let Some(page_child) = stack_page_child(&state.stack, terminal) {
            state.stack.page(&page_child).set_title(&tab_title);
        }
    }
    if active {
        state.window.set_title(Some(&window_title));
    }
}

fn push_title_part(parts: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && parts.iter().all(|part| part != value) {
        parts.push(value.to_owned());
    }
}

fn terminal_surface(
    terminal: &vte4::Terminal,
    profile_image: Option<&str>,
    image_mode: Option<BackgroundImageMode>,
    background_alpha: f64,
) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);
    // An optional image is deliberately non-persistent and opt-in. Missing
    // or unreadable files simply leave the profile's color background intact.
    let env_image =
        std::env::var_os("CORE_TERMINAL_BACKGROUND_IMAGE").map(std::path::PathBuf::from);
    let configured = profile_image
        .filter(|path| !path.trim().is_empty())
        .map(Path::new)
        .or(env_image.as_deref());
    if let Some(path) = configured {
        if path.is_file() {
            let picture = gtk::Picture::for_filename(path);
            picture.set_can_shrink(true);
            picture.set_content_fit(match image_mode.unwrap_or(BackgroundImageMode::Scale) {
                BackgroundImageMode::Center => gtk::ContentFit::ScaleDown,
                BackgroundImageMode::Scale => gtk::ContentFit::Contain,
                BackgroundImageMode::Tile => gtk::ContentFit::Cover,
            });
            picture.set_opacity((1.0 - background_alpha).clamp(0.0, 1.0));
            picture.set_hexpand(true);
            picture.set_vexpand(true);
            overlay.set_child(Some(&picture));
        }
    }
    overlay.add_overlay(terminal);
    overlay
}

fn apply_profile(terminal: &vte4::Terminal, profile: &TerminalProfile, settings: &Settings) {
    apply_profile_properties(terminal, profile, settings, true);
}

fn reapply_profile_without_resize(
    terminal: &vte4::Terminal,
    profile: &TerminalProfile,
    settings: &Settings,
) {
    apply_profile_properties(terminal, profile, settings, false);
}

fn runtime_terminal_settings_changed(before: &Settings, after: &Settings) -> bool {
    before.scroll_on_output != after.scroll_on_output
        || before.scroll_on_input != after.scroll_on_input
        || before.audible_bell != after.audible_bell
        || before.bold_is_bright != after.bold_is_bright
}

fn runtime_profile_requires_reapply(
    global_runtime_changed: bool,
    profile_changed: bool,
    assignment_changed: bool,
) -> bool {
    global_runtime_changed || profile_changed || assignment_changed
}

fn apply_profile_properties(
    terminal: &vte4::Terminal,
    profile: &TerminalProfile,
    settings: &Settings,
    apply_grid_size: bool,
) {
    if apply_grid_size {
        terminal.set_size(profile.columns as i64, profile.rows as i64);
    }
    let font = gtk::pango::FontDescription::from_string(&format!(
        "{} {}",
        profile.font, profile.font_size
    ));
    terminal.set_font(Some(&font));
    terminal.set_scrollback_lines(if profile.scrollback_unlimited {
        -1
    } else {
        // Scrollback is profile-owned. `scrollback_lines` is the field edited
        // by the profile settings panel; retain the older limit field only as
        // a compatibility fallback for hand-authored profiles.
        profile
            .scrollback_lines
            .max(profile.scrollback_limit)
            .max(100) as i64
    });
    terminal.set_scroll_on_output(settings.scroll_on_output);
    terminal.set_scroll_on_keystroke(settings.scroll_on_input && profile.scroll_on_input);
    terminal.set_audible_bell(settings.audible_bell || profile.audible_bell);
    terminal.set_bold_is_bright(settings.bold_is_bright && profile.bold_is_bright);
    terminal.set_text_blink_mode(if profile.text_blink {
        vte4::TextBlinkMode::Always
    } else {
        vte4::TextBlinkMode::Never
    });
    terminal.set_cjk_ambiguous_width(i32::from(profile.ambiguous_width.clamp(1, 2)));
    terminal.set_delete_binding(if profile.delete_sends_control_h {
        vte4::EraseBinding::AsciiBackspace
    } else {
        vte4::EraseBinding::Auto
    });
    // Never hide the pointer: screenshot/KVM focus transitions must remain
    // recoverable, regardless of the value found in an older settings file.
    enforce_terminal_input_safety(terminal);
    terminal.set_cursor_blink_mode(if profile.cursor_blink {
        vte4::CursorBlinkMode::On
    } else {
        vte4::CursorBlinkMode::Off
    });
    terminal.set_cursor_shape(match profile.cursor_shape {
        CursorShape::Block => vte4::CursorShape::Block,
        CursorShape::IBeam => vte4::CursorShape::Ibeam,
        CursorShape::Underline => vte4::CursorShape::Underline,
    });
    let foreground = gtk::gdk::RGBA::parse(&profile.foreground)
        .unwrap_or_else(|_| gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
    let mut background = gtk::gdk::RGBA::parse(&profile.background)
        .unwrap_or_else(|_| gtk::gdk::RGBA::new(0.0, 0.0, 0.0, profile.background_alpha as f32));
    background.set_alpha(profile.background_alpha as f32);
    let cursor = gtk::gdk::RGBA::parse(&profile.cursor).unwrap_or(foreground);
    let selection = gtk::gdk::RGBA::parse(&profile.selection).unwrap_or(background);
    let palette_colors = profile
        .ansi_palette
        .iter()
        .filter_map(|value| gtk::gdk::RGBA::parse(value).ok())
        .collect::<Vec<_>>();
    let palette = palette_colors.iter().collect::<Vec<_>>();
    terminal.set_colors(Some(&foreground), Some(&background), &palette);
    if let Ok(bold) = gtk::gdk::RGBA::parse(&profile.bold_color) {
        terminal.set_color_bold(Some(&bold));
    }
    terminal.set_color_cursor(Some(&cursor));
    terminal.set_color_highlight(Some(&selection));
    refresh_terminal_background(terminal, profile);
}

fn refresh_terminal_background(terminal: &vte4::Terminal, profile: &TerminalProfile) {
    let Some(overlay) = terminal
        .parent()
        .and_then(|widget| widget.downcast::<gtk::Overlay>().ok())
    else {
        return;
    };
    overlay.set_child(None::<&gtk::Widget>);
    let env_image =
        std::env::var_os("CORE_TERMINAL_BACKGROUND_IMAGE").map(std::path::PathBuf::from);
    let configured = profile
        .background_image_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(Path::new)
        .or(env_image.as_deref());
    let Some(path) = configured.filter(|path| path.is_file()) else {
        return;
    };
    let picture = gtk::Picture::for_filename(path);
    picture.set_can_shrink(true);
    picture.set_content_fit(match profile.background_image_mode {
        BackgroundImageMode::Center => gtk::ContentFit::ScaleDown,
        BackgroundImageMode::Scale => gtk::ContentFit::Contain,
        BackgroundImageMode::Tile => gtk::ContentFit::Cover,
    });
    picture.set_opacity((1.0 - profile.background_alpha).clamp(0.0, 1.0));
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    overlay.set_child(Some(&picture));
}

fn enforce_terminal_input_safety(terminal: &vte4::Terminal) {
    terminal.set_mouse_autohide(false);
    debug_assert!(!terminal.is_mouse_autohide());
}

fn enforce_non_modal<W: IsA<gtk::Window>>(window: &W) {
    window.set_modal(false);
    debug_assert!(!window.is_modal());
}

fn connect_terminal_shortcuts(
    terminal: &vte4::Terminal,
    state: Rc<RefCell<UiState>>,
    id: SessionId,
) {
    let controller = gtk::EventControllerKey::new();
    let terminal_ref = terminal.clone();
    controller.connect_key_pressed(move |_, keyval, _, modifiers| {
        let key = keyval.to_unicode().unwrap_or_default();
        let key_name = keyval
            .name()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let (custom_mapping, option_as_meta, escape_non_ascii, paste_newlines_as_cr) = {
            let state = state.borrow();
            let profile = state
                .sessions
                .tab(id)
                .and_then(|tab| state.profiles.profile(&tab.profile_name));
            let mapping = profile.and_then(|profile| {
                profile
                    .key_mappings
                    .iter()
                    .find(|mapping| {
                        (mapping.key.eq_ignore_ascii_case(&key_name)
                            || (mapping.key.chars().count() == 1
                                && mapping.key.eq_ignore_ascii_case(&key.to_string())))
                            && mapping_modifiers_match(&mapping.modifiers, modifiers)
                    })
                    .and_then(|mapping| decode_key_sequence(&mapping.action).ok())
            });
            (
                mapping,
                profile.is_some_and(|profile| profile.option_as_meta),
                profile.is_some_and(|profile| profile.escape_non_ascii),
                profile.is_some_and(|profile| profile.paste_newlines_as_cr),
            )
        };
        if let Some(bytes) = custom_mapping {
            terminal_ref.feed_child(&bytes);
            return glib::Propagation::Stop;
        }
        let action = decide_shortcut(ShortcutInput {
            key,
            ctrl: modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK),
            alt: modifiers.contains(gtk::gdk::ModifierType::ALT_MASK),
            shift: modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
            has_selection: terminal_ref.has_selection(),
        });
        match action {
            ShortcutAction::CopySelection => copy_selection(&terminal_ref),
            ShortcutAction::Paste => {
                if paste_newlines_as_cr {
                    paste_clipboard_with_carriage_returns(&terminal_ref);
                } else {
                    terminal_ref.paste_clipboard();
                }
            }
            ShortcutAction::SendControlC => core::send_control_c(&terminal_ref),
            ShortcutAction::SendControlV => core::send_control_v(&terminal_ref),
            ShortcutAction::None => {
                if option_as_meta
                    && key != '\0'
                    && modifiers.contains(gtk::gdk::ModifierType::ALT_MASK)
                    && !modifiers.intersects(
                        gtk::gdk::ModifierType::CONTROL_MASK
                            | gtk::gdk::ModifierType::META_MASK
                            | gtk::gdk::ModifierType::SUPER_MASK,
                    )
                {
                    let mut encoded = [0_u8; 4];
                    terminal_ref.feed_child(&[0x1b]);
                    terminal_ref.feed_child(key.encode_utf8(&mut encoded).as_bytes());
                    return glib::Propagation::Stop;
                }
                if escape_non_ascii
                    && !key.is_ascii()
                    && !modifiers.intersects(
                        gtk::gdk::ModifierType::CONTROL_MASK
                            | gtk::gdk::ModifierType::ALT_MASK
                            | gtk::gdk::ModifierType::META_MASK
                            | gtk::gdk::ModifierType::SUPER_MASK,
                    )
                {
                    let mut encoded = [0_u8; 4];
                    terminal_ref.feed_child(&[0x16]);
                    terminal_ref.feed_child(key.encode_utf8(&mut encoded).as_bytes());
                    return glib::Propagation::Stop;
                }
                if key.is_ascii_digit()
                    && key != '0'
                    && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    && !modifiers.intersects(
                        gtk::gdk::ModifierType::SHIFT_MASK | gtk::gdk::ModifierType::ALT_MASK,
                    )
                    && state.borrow().settings.ctrl_number_tabs
                {
                    switch_tab_index(&state, (key as u8 - b'1') as usize);
                    return glib::Propagation::Stop;
                }
                if key.eq_ignore_ascii_case(&'t')
                    && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    && !modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK)
                {
                    open_tab(&state);
                    return glib::Propagation::Stop;
                }
                if key.eq_ignore_ascii_case(&'w')
                    && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                {
                    close_tab(&state, id);
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
        }
        glib::Propagation::Stop
    });
    terminal.add_controller(controller);
}

fn mapping_modifiers_match(expected: &[String], actual: gtk::gdk::ModifierType) -> bool {
    let mut expected_mask = gtk::gdk::ModifierType::empty();
    for modifier in expected {
        expected_mask |= match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => gtk::gdk::ModifierType::CONTROL_MASK,
            "alt" | "option" => gtk::gdk::ModifierType::ALT_MASK,
            "shift" => gtk::gdk::ModifierType::SHIFT_MASK,
            "meta" | "super" | "command" | "cmd" => {
                gtk::gdk::ModifierType::META_MASK | gtk::gdk::ModifierType::SUPER_MASK
            }
            _ => return false,
        };
    }
    let meta = gtk::gdk::ModifierType::META_MASK | gtk::gdk::ModifierType::SUPER_MASK;
    let relevant = gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::SHIFT_MASK
        | meta;
    let actual = actual & relevant;
    if expected_mask.intersects(meta) {
        let without_meta = expected_mask & !meta;
        actual.contains(without_meta) && actual.intersects(meta) && (actual & !meta) == without_meta
    } else {
        actual == expected_mask
    }
}

fn paste_clipboard_with_carriage_returns(terminal: &vte4::Terminal) {
    let clipboard = terminal.display().clipboard();
    let terminal = terminal.clone();
    clipboard.read_text_async(None::<&gio::Cancellable>, move |result| {
        let Ok(Some(text)) = result else {
            return;
        };
        let normalized = text.replace("\r\n", "\n").replace('\n', "\r");
        terminal.feed_child(normalized.as_bytes());
    });
}

fn copy_selection(terminal: &vte4::Terminal) {
    if let Some(text) = terminal.text_selected(vte4::Format::Text) {
        terminal.display().clipboard().set_text(&text);
    }
}

fn close_tab(state: &Rc<RefCell<UiState>>, id: SessionId) {
    let plan = {
        let state = state.borrow();
        close_plan_for(&state, CloseRequest::Tab(id))
    };
    if plan.targets.is_empty() {
        return;
    }
    if plan.blockers.is_empty() {
        force_close_tab(state, id);
        dispatch_pending_close(state);
    } else if queue_or_reserve_close_prompt(state, CloseRequest::Tab(id)) {
        present_close_confirmation(state, CloseRequest::Tab(id), plan);
    }
}

fn close_plan_for(state: &UiState, request: CloseRequest) -> core::ClosePlan {
    let ids = match request {
        CloseRequest::Tab(id) => vec![id],
        CloseRequest::Window => state.sessions.tabs().iter().map(|tab| tab.id).collect(),
    };
    core::plan_close(ids.into_iter().filter_map(|id| {
        let tab = state.sessions.tab(id)?;
        let process = if state.pending_spawns.contains(&id.get()) {
            Some(core::RunningProcessIdentity::pending())
        } else {
            tab.child_pid.map(|pid| {
                state.child_process_identities.get(&id.get()).map_or_else(
                    || core::RunningProcessIdentity::unverified(pid),
                    |identity| {
                        core::running_process_identity(state.terminals.get(&id.get()), identity)
                    },
                )
            })
        };
        Some(core::CloseCandidate {
            session_id: id,
            process,
            profile: state.profiles.profile(&tab.profile_name),
            expected_login_shell: state.login_shell_identities.get(&id.get()),
        })
    }))
}

fn queue_or_reserve_close_prompt(state: &Rc<RefCell<UiState>>, request: CloseRequest) -> bool {
    let Ok(mut state) = state.try_borrow_mut() else {
        return false;
    };
    if state.close_prompt_open {
        if state.active_close_request != Some(request) {
            state.pending_close_request = Some(match (state.pending_close_request, request) {
                (Some(CloseRequest::Window), _) | (_, CloseRequest::Window) => CloseRequest::Window,
                (_, request) => request,
            });
        }
        return false;
    }
    state.close_prompt_open = true;
    state.active_close_request = Some(request);
    true
}

fn reset_close_prompt(state: &Rc<RefCell<UiState>>) {
    if let Ok(mut state) = state.try_borrow_mut() {
        state.close_prompt_open = false;
        state.active_close_request = None;
    }
}

fn dispatch_pending_close(state: &Rc<RefCell<UiState>>) {
    let pending = state
        .try_borrow_mut()
        .ok()
        .and_then(|mut state| state.pending_close_request.take());
    let Some(request) = pending else {
        return;
    };
    let close_state = state.clone();
    glib::idle_add_local_once(move || match request {
        CloseRequest::Tab(id) => close_tab(&close_state, id),
        CloseRequest::Window => {
            let window = close_state.borrow().window.clone();
            window.close();
        }
    });
}

fn present_close_confirmation(
    state: &Rc<RefCell<UiState>>,
    request: CloseRequest,
    plan: core::ClosePlan,
) {
    let parent = state.borrow().window.clone();
    let (title, close_label, scope) = match request {
        CloseRequest::Tab(_) => ("Close running terminal?", "Close Tab", "this tab"),
        CloseRequest::Window => ("Close running terminals?", "Close Window", "this window"),
    };
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(&parent)
        .destroy_with_parent(true)
        .modal(false)
        .default_width(460)
        .default_height(260)
        .build();
    dialog.set_widget_name("close-confirmation");
    enforce_non_modal(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    let processes = plan
        .blockers
        .iter()
        .map(|blocker| {
            blocker.process.name.clone().unwrap_or_else(|| {
                blocker.process.child_pid.map_or_else(
                    || "a process that is starting".to_owned(),
                    |pid| format!("unknown process (PID {pid})"),
                )
            })
        })
        .collect::<Vec<_>>();
    let process_count = processes.len();
    let process_word = if process_count == 1 {
        "process"
    } else {
        "processes"
    };
    let requirement = if process_count == 1 {
        "requires"
    } else {
        "require"
    };
    let label = gtk::Label::new(Some(&format!(
        "{process_count} {process_word} still running in {scope} {requirement} confirmation. Closing will send termination signals to them."
    )));
    label.set_wrap(true);
    label.set_xalign(0.0);
    content.append(&label);

    let process_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    process_list.set_margin_end(8);
    for process in processes {
        let process_label = gtk::Label::new(Some(&format!("• {process}")));
        process_label.set_wrap(true);
        process_label.set_selectable(true);
        process_label.set_xalign(0.0);
        process_list.append(&process_label);
    }
    let process_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(160)
        .child(&process_list)
        .build();
    process_scroller.set_widget_name("close-confirmation-processes");
    content.append(&process_scroller);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_widget_name("close-confirmation-cancel");
    let close = gtk::Button::with_label(close_label);
    close.set_widget_name("close-confirmation-accept");
    let cancel_dialog = dialog.clone();
    cancel.connect_clicked(move |_| cancel_dialog.close());
    let close_dialog = dialog.clone();
    let close_state = state.clone();
    let confirmed = plan.clone();
    let accepting = Rc::new(Cell::new(false));
    let accepting_close = accepting.clone();
    close.connect_clicked(move |_| {
        accepting_close.set(true);
        reset_close_prompt(&close_state);
        close_dialog.close();
        confirm_close(&close_state, request, confirmed.clone());
    });
    actions.append(&cancel);
    actions.append(&close);
    content.append(&actions);
    dialog.set_child(Some(&content));
    let close_state = state.clone();
    dialog.connect_close_request(move |_| {
        if !accepting.get() {
            reset_close_prompt(&close_state);
            dispatch_pending_close(&close_state);
        }
        glib::Propagation::Proceed
    });
    dialog.present();
}

fn confirm_close(state: &Rc<RefCell<UiState>>, request: CloseRequest, confirmed: core::ClosePlan) {
    let current = {
        let state = state.borrow();
        close_plan_for(&state, request)
    };
    if current.targets.is_empty() {
        dispatch_pending_close(state);
        return;
    }
    if !core::close_authorization_covers(&current, &confirmed) {
        if current.blockers.is_empty() {
            match request {
                CloseRequest::Tab(id) => {
                    force_close_tab(state, id);
                    dispatch_pending_close(state);
                }
                CloseRequest::Window => {
                    let window = state.borrow().window.clone();
                    window.close();
                }
            }
        } else if queue_or_reserve_close_prompt(state, request) {
            present_close_confirmation(state, request, current);
        }
        return;
    }
    match request {
        CloseRequest::Tab(id) => {
            force_close_tab(state, id);
            dispatch_pending_close(state);
        }
        CloseRequest::Window => {
            let window = {
                let mut state = state.borrow_mut();
                state.window_close_authorization = Some(confirmed);
                state.window.clone()
            };
            window.close();
        }
    }
}

fn finish_window_close(state: &Rc<RefCell<UiState>>) {
    let child_identities = {
        let mut state = state.borrow_mut();
        state.closing = true;
        state.close_prompt_open = false;
        state.active_close_request = None;
        state.pending_close_request = None;
        state.settings.window_width = state.window.width().max(320);
        state.settings.window_height = state.window.height().max(240);
        let _ = state.settings.save_user();
        let tab_ids = state
            .sessions
            .tabs()
            .iter()
            .map(|tab| tab.id.get())
            .collect::<Vec<_>>();
        let child_identities = tab_ids
            .into_iter()
            .filter_map(|id| state.child_process_identities.remove(&id))
            .collect::<Vec<_>>();
        state.sessions = SessionManager::empty();
        state.terminals.clear();
        state.pending_spawns.clear();
        state.child_process_identities.clear();
        state.login_shell_identities.clear();
        child_identities
    };
    for identity in child_identities {
        terminate_child_async(identity);
    }
}

fn force_close_tab(state: &Rc<RefCell<UiState>>, id: SessionId) {
    let Ok(mut state_mut) = state.try_borrow_mut() else {
        return;
    };
    if let Some(tab) = state_mut.sessions.close_tab(id) {
        state_mut.pending_spawns.remove(&id.get());
        let child_identity = state_mut.child_process_identities.remove(&id.get());
        state_mut.login_shell_identities.remove(&id.get());
        if tab.child_pid.is_some() {
            if let Some(identity) = child_identity {
                terminate_child_async(identity);
            }
        }
        if let Some(terminal) = state_mut.terminals.remove(&id.get()) {
            if let Some(page_child) = stack_page_child(&state_mut.stack, &terminal) {
                state_mut.stack.remove(&page_child);
            }
        }
    }
    let close_window = state_mut.sessions.is_empty();
    let window = state_mut.window.clone();
    drop(state_mut);
    if close_window {
        window.close();
    } else {
        sync_active_profile_ui(state);
    }
}

fn stack_page_child(stack: &gtk::Stack, terminal: &vte4::Terminal) -> Option<gtk::Widget> {
    let stack_widget = stack.clone().upcast::<gtk::Widget>();
    let mut child = terminal.clone().upcast::<gtk::Widget>();
    while let Some(parent) = child.parent() {
        if parent == stack_widget {
            return Some(child);
        }
        child = parent;
    }
    None
}
