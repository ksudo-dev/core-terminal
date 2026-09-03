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
};
use gtk::{gio, glib, prelude::*};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use vte4::prelude::{TerminalExt, TerminalExtManual};

const SETTINGS_PAGE_IDS: [&str; 4] = ["general", "profiles", "window-groups", "encodings"];
const PROFILE_PAGE_IDS: [&str; 6] = ["text", "window", "tab", "shell", "keyboard", "advanced"];

#[cfg(test)]
fn settings_page_ids() -> &'static [&'static str; 4] {
    &SETTINGS_PAGE_IDS
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod structural_tests {
    use super::{settings_page_ids, PROFILE_PAGE_IDS};

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

pub fn profile_selector(store: &ProfileStore) -> gtk::DropDown {
    let names = store.names().collect::<Vec<_>>();
    let model = gtk::StringList::new(&names);
    let dropdown = gtk::DropDown::new(Some(model), None::<&gtk::Expression>);
    dropdown.add_css_class("core-profile-selector");
    if let Some(index) = names.iter().position(|name| *name == store.selected_name()) {
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
                        import_list.append(&gtk::Label::new(Some(&imported_name)));
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
                        if let Some(model) = &model {
                            if let Some(index) = (0..model.n_items()).find(|index| {
                                model.string(*index).as_deref() == Some(imported_name.as_str())
                            }) {
                                startup.set_selected(index);
                            }
                        }
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
    let add_store = controls.profile_store.clone();
    let add_list = controls.profile_list.clone();
    add_button.connect_clicked(move |_| {
        let index = add_store.borrow().profiles().len() + 1;
        let mut profile = TerminalProfile::homebrew();
        let name = format!("Custom Profile {index}");
        profile.name = name.clone();
        if add_store.borrow_mut().add_profile(profile).is_ok() {
            if let Some(model) = add_startup.model().and_downcast::<gtk::StringList>() {
                model.append(&name);
                add_startup.set_selected(model.n_items().saturating_sub(1));
            }
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
    let duplicate_selection = controls.profile_selection.clone();
    let duplicate_store = controls.profile_store.clone();
    let duplicate_list = controls.profile_list.clone();
    duplicate_button.connect_clicked(move |_| {
        let source = duplicate_selection.borrow().clone();
        if source.is_empty() {
            return;
        }
        let index = duplicate_store.borrow().profiles().len() + 1;
        let name = format!("Copy of {source} {index}");
        if duplicate_store
            .borrow_mut()
            .duplicate_profile(&source, &name)
            .is_ok()
        {
            if let Some(model) = duplicate_startup.model().and_downcast::<gtk::StringList>() {
                model.append(&name);
                duplicate_startup.set_selected(model.n_items().saturating_sub(1));
            }
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
    let delete_selection = controls.profile_selection.clone();
    let delete_store = controls.profile_store.clone();
    let delete_list = controls.profile_list.clone();
    delete_button.connect_clicked(move |_| {
        let name = delete_selection.borrow().clone();
        if name.is_empty() {
            return;
        }
        if delete_store.borrow_mut().delete_profile(&name).is_ok() {
            if let Some(model) = delete_startup.model().and_downcast::<gtk::StringList>() {
                if let Some(index) = (0..model.n_items())
                    .find(|index| model.string(*index).as_deref() == Some(name.as_str()))
                {
                    model.remove(index);
                    delete_startup.set_selected(index.saturating_sub(1));
                    if let Some(row) = delete_list.row_at_index(index as i32) {
                        delete_list.remove(&row);
                    }
                }
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
        let selected_profile = controls
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
                2 => CloseOnExit::Always,
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
            profile.close_on_clean_exit = controls.profile_close_clean.is_active();
            profile.close_on_error = controls.profile_close_error.is_active();
            profile.ask_before_close = controls.profile_ask_close.is_active();
            read_profile_widgets(
                &controls.profile_stack,
                &mut profile,
                &controls.profile_mappings,
            );
            // Built-ins accept edits but remain protected from deletion;
            // custom profiles accept both edits and deletion.
            let _ = edited_profiles.update_profile(profile);
        }
        on_save(
            Settings {
                schema_version: CURRENT_SCHEMA_VERSION,
                startup_profile: selected_profile.clone(),
                startup_window_group: if controls.use_startup_group.is_active() {
                    dropdown_text(&controls.startup_window_group)
                        .filter(|name| name != "No groups saved")
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                selected_profile,
                new_window_profile: dropdown_value(&controls.new_window_profile, "Default profile"),
                new_tab_profile: dropdown_value(&controls.new_tab_profile, "Same as startup"),
                new_window_same_directory: controls.new_window_same_directory.is_active(),
                font: controls.font.text().to_string(),
                font_size: controls.font_size.value(),
                cursor_shape,
                cursor_blink: controls.cursor_blink.is_active(),
                scrollback_lines: controls.scrollback.value() as u32,
                window_width: controls.window_width.value() as i32,
                window_height: controls.window_height.value() as i32,
                use_custom_command: controls.use_custom_command.is_active(),
                custom_command: controls.custom_command.text().to_string(),
                shell: controls.shell.text().to_string(),
                run_command_inside_shell: controls.run_command_inside_shell.is_active(),
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
                terminal_type: controls.terminal_type.text().to_string(),
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
    profile_close_clean: gtk::CheckButton,
    profile_close_error: gtk::CheckButton,
    profile_ask_close: gtk::CheckButton,
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
    terminal_type: gtk::Entry,
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
}

impl SettingsControls {
    fn build(
        settings: &Settings,
        profile_names: &[String],
        top_stack: &gtk::Stack,
        profile_store: Rc<RefCell<ProfileStore>>,
        on_launch_group: Rc<dyn Fn(WindowGroup)>,
    ) -> Self {
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
        let groups_available = !profile_store.borrow().window_groups().is_empty();
        startup.set_sensitive(!use_startup_group.is_active());
        use_startup_group.connect_toggled(move |button| {
            startup_for_mode.set_sensitive(!button.is_active());
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
        let profile_policy_names = profile_names.iter().map(String::as_str).collect::<Vec<_>>();
        let new_window_profile = gtk::DropDown::new(
            Some(gtk::StringList::new(&profile_policy_names)),
            None::<&gtk::Expression>,
        );
        new_window_profile.set_widget_name("new-window-profile");
        let new_window_index = profile_names
            .iter()
            .position(|name| name == &settings.new_window_profile)
            .unwrap_or(0);
        new_window_profile.set_selected(new_window_index as u32);
        let new_window_label = field_label("New window profile");
        general_grid.attach(&new_window_label, 0, 8, 1, 1);
        general_grid.attach(&new_window_profile, 1, 8, 1, 1);
        let new_tab_policy_names = std::iter::once("Same as startup")
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
            if name == &settings.selected_profile {
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
        let profile_selection = Rc::new(RefCell::new(settings.selected_profile.clone()));
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
        font.set_text(&settings.font);
        font.set_hexpand(true);
        appearance_grid.attach(&font, 1, 0, 1, 1);
        let font_size_label = field_label("Font size");
        appearance_grid.attach(&font_size_label, 0, 1, 1, 1);
        let font_size = gtk::SpinButton::with_range(6.0, 96.0, 1.0);
        font_size.set_widget_name("profile-font-size");
        font_size.set_value(settings.font_size);
        appearance_grid.attach(&font_size, 1, 1, 1, 1);
        let cursor_label = field_label("Cursor shape");
        appearance_grid.attach(&cursor_label, 0, 2, 1, 1);
        let cursor_shape_names = ["Block", "I-beam", "Underline"];
        let cursor_shape = gtk::DropDown::new(
            Some(gtk::StringList::new(&cursor_shape_names)),
            None::<&gtk::Expression>,
        );
        cursor_shape.set_widget_name("profile-cursor-shape");
        cursor_shape.set_selected(match settings.cursor_shape {
            CursorShape::Block => 0,
            CursorShape::IBeam => 1,
            CursorShape::Underline => 2,
        });
        appearance_grid.attach(&cursor_shape, 1, 2, 1, 1);
        let cursor_blink = check("Blink cursor", settings.cursor_blink);
        cursor_blink.set_widget_name("profile-cursor-blink");
        appearance_grid.attach(&cursor_blink, 1, 3, 1, 1);
        let scrollback_label = field_label("Scrollback lines");
        appearance_grid.attach(&scrollback_label, 0, 4, 1, 1);
        let scrollback = gtk::SpinButton::with_range(100.0, 1_000_000.0, 100.0);
        scrollback.set_widget_name("profile-scrollback");
        scrollback.set_value(settings.scrollback_lines as f64);
        appearance_grid.attach(&scrollback, 1, 4, 1, 1);
        let selected_defaults = profile_store
            .borrow()
            .profile(&settings.selected_profile)
            .cloned()
            .unwrap_or_else(|| profile_store.borrow().selected().clone());
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
            let button = named_check(label, active, name);
            if name == "profile-tab-show-ctrl-key" {
                button.set_sensitive(false);
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
            "Smooth window resizing (managed by GNOME Wayland)",
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
            "Show live terminal contents in the Dock (not exposed by GNOME Dock)",
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
            let button = named_check(label, active, name);
            if name == "profile-title-show-tty" || name == "profile-title-show-ctrl-key" {
                button.set_sensitive(false);
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
        let run_command_inside_shell = check(
            "Run command inside login shell",
            selected_defaults.run_inside_shell,
        );
        run_command_inside_shell.set_widget_name("profile-run-inside-shell");
        shell_page.0.append(&run_command_inside_shell);
        let run_command = gtk::Entry::new();
        run_command.set_widget_name("profile-shell-command");
        run_command.set_text(&selected_defaults.shell_command);
        run_command.set_placeholder_text(Some("/bin/bash --login -c 'command'"));
        run_command.set_hexpand(true);
        shell_page.0.append(&field_label("Run command"));
        shell_page.0.append(&run_command);
        let profile_shell = gtk::Entry::new();
        profile_shell.set_widget_name("profile-shell");
        profile_shell.set_text(&selected_defaults.shell);
        profile_shell.set_placeholder_text(Some("Optional complete shell path"));
        profile_shell.set_hexpand(true);
        shell_page.0.append(&field_label("Login shell"));
        shell_page.0.append(&profile_shell);
        let close_clean = named_check(
            "Close window after clean exit",
            selected_defaults.close_on_clean_exit,
            "profile-close-clean",
        );
        shell_page.0.append(&close_clean);
        let close_error = named_check(
            "Close window after error",
            selected_defaults.close_on_error,
            "profile-close-error",
        );
        shell_page.0.append(&close_error);
        let ask_close = named_check(
            "Ask before closing running process",
            selected_defaults.ask_before_close,
            "profile-ask-close",
        );
        shell_page.0.append(&ask_close);
        let close_on_exit = gtk::DropDown::new(
            Some(gtk::StringList::new(&["Never", "Clean exit", "Always"])),
            None::<&gtk::Expression>,
        );
        close_on_exit.set_widget_name("profile-close-on-exit");
        close_on_exit.set_selected(match selected_defaults.close_on_exit {
            CloseOnExit::Never => 0,
            CloseOnExit::Clean => 1,
            CloseOnExit::Always => 2,
        });
        shell_page
            .0
            .append(&field_label("Close after command exits"));
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
        ask_policy.set_selected(match selected_defaults.ask_before_close_policy {
            AskBeforeClosePolicy::Never => 0,
            AskBeforeClosePolicy::Always => 1,
            AskBeforeClosePolicy::NonExempt => 2,
        });
        shell_page.0.append(&field_label("Close confirmation"));
        shell_page.0.append(&ask_policy);
        let exit_policy = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Ask",
                "Keep window",
                "Close tab",
                "Close window",
                "Close on clean exit",
            ])),
            None::<&gtk::Expression>,
        );
        exit_policy.set_widget_name("shell-exit-policy");
        exit_policy.set_selected(match selected_defaults.shell_exit_action {
            ShellExitAction::Ask => 0,
            ShellExitAction::Keep => 1,
            ShellExitAction::CloseTab => 2,
            ShellExitAction::CloseWindow => 3,
        });
        shell_page.0.append(&field_label("On shell exit"));
        shell_page.0.append(&exit_policy);
        let exceptions = gtk::Entry::new();
        exceptions.set_widget_name("profile-exceptions");
        exceptions.set_placeholder_text(Some("process-name, another-process"));
        exceptions.set_text(&selected_defaults.ask_before_close_exceptions.join(", "));
        exceptions.set_hexpand(true);
        shell_page.0.append(&exceptions);
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
        terminal_type.set_text(&settings.terminal_type);
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
                "Badge app and window icons with a GNOME notification",
                selected_defaults.background_notifications,
                "profile-background-notifications",
            ),
            (
                "Request urgent background attention with a GNOME notification",
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
            "Continue requesting attention until focused (GNOME owns notification lifetime)",
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
        let editor_current = Rc::new(RefCell::new(settings.selected_profile.clone()));
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
        window_groups.set_widget_name("window-groups-list");
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
        let remove_group = gtk::Button::with_label("Remove group");
        let save_groups = gtk::Button::with_label("Save groups");
        let launch_group = gtk::Button::with_label("Launch selected group");
        for (index, button) in [&add_group, &remove_group, &save_groups, &launch_group]
            .into_iter()
            .enumerate()
        {
            button.set_hexpand(true);
            group_actions.attach(button, (index % 2) as i32, (index / 2) as i32, 1, 1);
        }
        let group_list_for_select = groups.clone();
        let group_store_for_select = group_store.clone();
        let selected_group_for_select = selected_group.clone();
        let profile_names_for_select = profile_names.to_vec();
        let name_for_select = group_name.clone();
        let profile_for_select = group_profile.clone();
        let directory_for_select = group_directory.clone();
        let columns_for_select = group_columns.clone();
        let rows_for_select = group_rows.clone();
        groups.connect_row_selected(move |_, row| {
            let Some(label) = row.and_then(|row| row.child().and_downcast::<gtk::Label>()) else {
                return;
            };
            let name = label.text().to_string();
            let Some(group) = group_store_for_select.borrow().window_group(&name).cloned() else {
                return;
            };
            let Some(entry) = group.entries.first() else {
                return;
            };
            *selected_group_for_select.borrow_mut() = Some(name);
            name_for_select.set_text(&group.name);
            directory_for_select.set_text(entry.working_directory.as_deref().unwrap_or(""));
            columns_for_select.set_value(entry.columns as f64);
            rows_for_select.set_value(entry.rows as f64);
            if let Some(index) = profile_names_for_select
                .iter()
                .position(|candidate| candidate == &entry.profile)
            {
                profile_for_select.set_selected(index as u32);
            }
            group_list_for_select.grab_focus();
        });
        if let Some(row) = groups.row_at_index(0) {
            groups.select_row(Some(&row));
        }
        let group_list_for_add = groups.clone();
        let group_store_for_add = group_store.clone();
        let name_for_add = group_name.clone();
        let profile_for_add = group_profile.clone();
        let directory_for_add = group_directory.clone();
        let columns_for_add = group_columns.clone();
        let rows_for_add = group_rows.clone();
        let selected_for_add = selected_group.clone();
        let startup_model_for_add = startup_group_model.clone();
        let startup_toggle_for_add = use_startup_group.clone();
        let startup_dropdown_for_add = startup_window_group.clone();
        add_group.connect_clicked(move |_| {
            let index = group_store_for_add.borrow().window_groups().len() + 1;
            let name = if name_for_add.text().trim().is_empty() {
                format!("Window Group {index}")
            } else {
                name_for_add.text().trim().to_owned()
            };
            let profile = dropdown_text(&profile_for_add)
                .or_else(|| Some(group_store_for_add.borrow().selected_name().to_owned()))
                .unwrap_or_default();
            let group = WindowGroup {
                name: name.clone(),
                entries: vec![WindowGroupEntry {
                    profile,
                    working_directory: (!directory_for_add.text().trim().is_empty())
                        .then(|| directory_for_add.text().trim().to_owned()),
                    columns: columns_for_add.value() as u32,
                    rows: rows_for_add.value() as u32,
                }],
            };
            if group_store_for_add
                .borrow_mut()
                .add_window_group(group)
                .is_ok()
            {
                group_list_for_add.append(&gtk::Label::new(Some(&name)));
                *selected_for_add.borrow_mut() = Some(name.clone());
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
        let startup_model_for_remove = startup_group_model.clone();
        let startup_toggle_for_remove = use_startup_group.clone();
        let startup_dropdown_for_remove = startup_window_group.clone();
        remove_group.connect_clicked(move |_| {
            let Some(row) = group_list_for_remove.selected_row() else {
                return;
            };
            let Some(label) = row.child().and_downcast::<gtk::Label>() else {
                return;
            };
            if group_store_for_remove
                .borrow_mut()
                .delete_window_group(label.text().as_str())
                .is_ok()
            {
                if let Some(index) =
                    string_list_position(&startup_model_for_remove, label.text().as_str())
                {
                    startup_model_for_remove.remove(index);
                }
                if startup_model_for_remove.n_items() == 0 {
                    startup_model_for_remove.append("No groups saved");
                    startup_toggle_for_remove.set_active(false);
                    startup_dropdown_for_remove.set_sensitive(false);
                }
                group_list_for_remove.remove(&row);
                *selected_for_remove.borrow_mut() = None;
            }
        });
        let group_store_for_save = group_store.clone();
        let selected_for_save = selected_group.clone();
        let name_for_save = group_name.clone();
        let profile_for_save = group_profile.clone();
        let directory_for_save = group_directory.clone();
        let columns_for_save = group_columns.clone();
        let rows_for_save = group_rows.clone();
        let list_for_save = groups.clone();
        let startup_model_for_save = startup_group_model.clone();
        save_groups.connect_clicked(move |_| {
            let Some(old_name) = selected_for_save.borrow().clone() else {
                save_user_profiles(&group_store_for_save.borrow());
                return;
            };
            let new_name = name_for_save.text().trim().to_owned();
            let Some(profile) = dropdown_text(&profile_for_save) else {
                return;
            };
            if new_name.is_empty() {
                return;
            }
            let entry = WindowGroupEntry {
                profile,
                working_directory: (!directory_for_save.text().trim().is_empty())
                    .then(|| directory_for_save.text().trim().to_owned()),
                columns: columns_for_save.value() as u32,
                rows: rows_for_save.value() as u32,
            };
            // Keep additional entries loaded from the profile document. The
            // editor controls the first entry and never silently discards
            // the remaining launch targets.
            let mut entries = group_store_for_save
                .borrow()
                .window_group(&old_name)
                .map(|group| group.entries.clone())
                .unwrap_or_default();
            if entries.is_empty() {
                entries.push(entry);
            } else {
                entries[0] = entry;
            }
            let group = WindowGroup {
                name: new_name.clone(),
                entries,
            };
            let result = if old_name == new_name {
                group_store_for_save.borrow_mut().update_window_group(group)
            } else {
                // ProfileStore intentionally has no implicit rename operation:
                // add the validated new record first, then remove the old one.
                let added = group_store_for_save.borrow_mut().add_window_group(group);
                if added.is_ok() {
                    let _ = group_store_for_save
                        .borrow_mut()
                        .delete_window_group(&old_name);
                }
                added
            };
            if result.is_ok() {
                if let Some(row) = list_for_save.selected_row() {
                    row.set_child(Some(&gtk::Label::new(Some(&new_name))));
                }
                if old_name != new_name {
                    if let Some(index) = string_list_position(&startup_model_for_save, &old_name) {
                        startup_model_for_save.splice(index, 1, &[new_name.as_str()]);
                    }
                }
                *selected_for_save.borrow_mut() = Some(new_name);
            }
            save_user_profiles(&group_store_for_save.borrow());
        });
        let launch_callback = on_launch_group.clone();
        let launch_store = group_store.clone();
        let launch_selection = selected_group.clone();
        launch_group.connect_clicked(move |_| {
            let Some(name) = launch_selection.borrow().clone() else {
                return;
            };
            if let Some(group) = launch_store.borrow().window_group(&name).cloned() {
                launch_callback(group);
            }
        });
        window_groups.append(&groups);
        window_groups.append(&group_actions);
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
            profile_close_clean: close_clean,
            profile_close_error: close_error,
            profile_ask_close: ask_close,
            new_tab_same_directory,
            new_window_same_directory,
            ctrl_number_tabs,
            scroll_on_output: legacy_scroll_on_output,
            scroll_on_input: legacy_scroll_on_input,
            audible_bell: legacy_audible_bell,
            bold_is_bright: legacy_bold_is_bright,
            background_notifications,
            terminal_type,
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
        ("profile-title-show-tty", &mut profile.title_show_tty),
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
            "profile-title-show-ctrl-key",
            &mut profile.title_show_ctrl_key,
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
            "profile-tab-show-ctrl-key",
            &mut profile.tab_title_show_ctrl_key,
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
        ("profile-antialias", &mut profile.antialias),
        ("profile-use-bold-fonts", &mut profile.use_bold_fonts),
        ("profile-text-blink", &mut profile.text_blink),
        ("profile-use-ansi", &mut profile.use_ansi_colors),
        ("profile-dynamic-colors", &mut profile.dynamic_colors),
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
        ("profile-smooth-resize", &mut profile.smooth_resize),
        (
            "profile-unlimited-scrollback",
            &mut profile.scrollback_unlimited,
        ),
        ("profile-restore-rows", &mut profile.restore_rows),
        ("profile-run-inside-shell", &mut profile.run_inside_shell),
        ("profile-close-clean", &mut profile.close_on_clean_exit),
        ("profile-close-error", &mut profile.close_on_error),
        ("profile-ask-close", &mut profile.ask_before_close),
        ("profile-option-meta", &mut profile.option_as_meta),
        ("profile-alt-scroll", &mut profile.alternate_screen_scroll),
        (
            "profile-delete-control-h",
            &mut profile.delete_sends_control_h,
        ),
        ("profile-escape-nonascii", &mut profile.escape_non_ascii),
        ("profile-paste-cr", &mut profile.paste_newlines_as_cr),
        ("profile-keypad", &mut profile.application_keypad),
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
    if let Some(value) = profile_spin(stack, "profile-restore-rows-limit") {
        profile.restore_rows_limit = value as u32;
    }
    if let Some(value) = profile_entry(stack, "profile-restore-bookmark") {
        profile.restore_rows_bookmark = value;
    }
    if let Some(value) = profile_spin(stack, "profile-scrollback-limit") {
        profile.scrollback_limit = value as u32;
    }
    if let Some(value) = profile_dropdown(stack, "profile-close-on-exit") {
        profile.close_on_exit = match value {
            1 => CloseOnExit::Clean,
            2 => CloseOnExit::Always,
            _ => CloseOnExit::Never,
        };
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
            CloseOnExit::Always => 2,
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
        ("profile-antialias", profile.antialias),
        ("profile-use-bold-fonts", profile.use_bold_fonts),
        ("profile-text-blink", profile.text_blink),
        ("profile-use-ansi", profile.use_ansi_colors),
        ("profile-dynamic-colors", profile.dynamic_colors),
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
        ("profile-tab-show-ctrl-key", profile.tab_title_show_ctrl_key),
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
        ("profile-title-show-tty", profile.title_show_tty),
        ("profile-title-show-process", profile.title_show_process),
        ("profile-title-show-arguments", profile.title_show_arguments),
        (
            "profile-title-show-dimensions",
            profile.title_show_dimensions,
        ),
        ("profile-title-show-ctrl-key", profile.title_show_ctrl_key),
        ("profile-smooth-resize", profile.smooth_resize),
        ("profile-unlimited-scrollback", profile.scrollback_unlimited),
        ("profile-restore-rows", profile.restore_rows),
        ("profile-run-inside-shell", profile.run_inside_shell),
        ("profile-close-clean", profile.close_on_clean_exit),
        ("profile-close-error", profile.close_on_error),
        ("profile-ask-close", profile.ask_before_close),
        ("profile-option-meta", profile.option_as_meta),
        ("profile-alt-scroll", profile.alternate_screen_scroll),
        ("profile-delete-control-h", profile.delete_sends_control_h),
        ("profile-escape-nonascii", profile.escape_non_ascii),
        ("profile-paste-cr", profile.paste_newlines_as_cr),
        ("profile-keypad", profile.application_keypad),
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
        "Default profile" => "default".into(),
        "Same as startup" => "same".into(),
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
    pending_working_directory: Option<String>,
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
    gtk::Window::set_default_icon_name("core-terminal");
    let mut profiles = load_user_profiles();
    let mut settings = Settings::load_user();
    let requested_profile = if new_window && settings.new_window_profile != "default" {
        settings.new_window_profile.clone()
    } else {
        settings.startup_profile.clone()
    };
    if profiles.select(&requested_profile) {
        settings.selected_profile = requested_profile;
    } else if !profiles.select(&settings.selected_profile) {
        settings.selected_profile = profiles.selected_name().to_owned();
    }
    // Materialize first-launch defaults so Homebrew and the initial window
    // geometry can be verified and restored even if the first session ends
    // unexpectedly.
    let _ = settings.save_user();
    let profile_dropdown = profile_selector(&profiles);
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(display_name)
        .icon_name("core-terminal")
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
        pending_working_directory,
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
        if !state.profiles.select(&name) {
            return;
        }
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
        open_tab(&state);
    }

    let close_state = state.clone();
    window.connect_close_request(move |_| {
        let mut state = close_state.borrow_mut();
        state.settings.window_width = state.window.width().max(320);
        state.settings.window_height = state.window.height().max(240);
        let _ = state.settings.save_user();
        for tab in state.sessions.tabs() {
            if let Some(pid) = tab.child_pid {
                core::terminate_child(pid);
            }
        }
        glib::Propagation::Proceed
    });
    window.present();
    schedule_acceptance_harness(app, &state);
}

/// Drive the same window/session helpers used by buttons and accelerators.
/// This is enabled only by the acceptance-test environment and lets the
/// native GNOME Wayland session verify real VTE PTYs and widget behavior.
fn project_icon() -> gtk::Image {
    for candidate in [
        "data/icons/core-terminal-icon-64.png",
        "/usr/share/icons/hicolor/64x64/apps/core-terminal.png",
    ] {
        if Path::new(candidate).is_file() {
            return gtk::Image::from_file(candidate);
        }
    }
    gtk::Image::from_icon_name("core-terminal")
}

/// Opt-in installed-binary acceptance run.  The harness is intentionally
/// inert unless CORE_TERMINAL_ACCEPTANCE is present, so normal users never
/// get an automated tab/window lifecycle.  It uses the real GTK widget tree
/// and VTE instances created by this module rather than string-only tests.
fn schedule_acceptance_harness(app: &gtk::Application, state: &Rc<RefCell<UiState>>) {
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
            state.profiles.select("Acceptance Profile");
            state.settings.selected_profile = "Acceptance Profile".into();
            state.settings.startup_profile = "Acceptance Profile".into();
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
            "profile-close-on-exit",
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
        // A changed value followed by the actual Save button proves the
        // callback path is live; the callback normalizes and persists it.
        if let Some(root) = &root {
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
                "profile-close-clean",
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
            if let Some(save) = find_widget_by_name(root, "settings-save")
                .and_then(|widget| widget.downcast::<gtk::Button>().ok())
            {
                save.emit_clicked();
            }
        }
        let profile_file_written = ProfileStore::config_path()
            .map(|path| path.is_file())
            .unwrap_or(false);
        let profile_round_trip = state
            .try_borrow()
            .ok()
            .and_then(|state| state.profiles.profile("Acceptance Profile").cloned())
            .map(|profile| {
                profile.columns == 100
                    && profile.close_on_clean_exit
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
            && standard_mappings_present
            && encoding_rows_present
            && runtime_profile_applied;
        let report_path = std::env::var_os("CORE_TERMINAL_ACCEPTANCE_REPORT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new("/tmp/core-terminal-acceptance.json").to_path_buf());
        let report = format!(
            "status={} missing={:?} non_modal={} mouse_autohide_disabled={} settings_geometry={}x{} settings_geometry_usable={} profile_page_not_horizontally_scrolled={} sidebar_width={} sidebar_geometry_usable={} profile_tabs_width={} profile_tabs_usable={} minimum_profile_label_width={} profile_labels_readable={} minimum_profile_action_width={} profile_actions_labeled={} profiles={} profile_file_written={} profile_round_trip={} standard_mappings_present={} encoding_rows_present={} runtime_profile_applied={}\n",
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
            standard_mappings_present,
            encoding_rows_present,
            runtime_profile_applied,
        );
        let _ = std::fs::write(report_path, report);
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
    let (parent, settings, profiles) = {
        let state = state.borrow();
        (
            state.window.clone().upcast::<gtk::Window>(),
            state.settings.clone(),
            state.profiles.clone(),
        )
    };
    let save_state = state.clone();
    let launch_state = state.clone();
    show_settings(
        &parent,
        &settings,
        profiles,
        move |new_settings, profiles| {
            let mut state = save_state.borrow_mut();
            let mut new_settings = new_settings.normalize();
            state.profiles = profiles;
            if !state.profiles.select(&new_settings.selected_profile) {
                new_settings.selected_profile = state.profiles.selected_name().to_owned();
            }
            state.settings = new_settings;
            let selected_profile = state.settings.selected_profile.clone();
            if let Some(session) = state.sessions.active_mut() {
                session.profile_name = selected_profile;
            }
            if let Some(index) = state
                .profiles
                .names()
                .position(|name| name == state.settings.selected_profile)
            {
                state.profile_dropdown.set_selected(index as u32);
            };
            let profile = state
                .profiles
                .profile(&state.settings.selected_profile)
                .cloned();
            let active = state.sessions.active().and_then(|tab| {
                state
                    .terminals
                    .get(&tab.id.get())
                    .cloned()
                    .map(|terminal| (tab.id, terminal))
            });
            if let (Some((_, terminal)), Some(profile)) = (&active, profile) {
                apply_profile(terminal, &profile, &state.settings);
            }
            let _ = state.settings.save_user();
            save_user_profiles(&state.profiles);
            drop(state);
            if let Some((id, terminal)) = active {
                update_tab_title(&save_state, id, &terminal);
            }
        },
        move |group| launch_window_group(&launch_state, group),
    )
}

/// Launch every entry in a saved group through the normal tab/PTY path. The
/// one-shot directory slot keeps group entries independent from the current
/// tab-directory preference and is consumed by `open_tab` exactly once.
fn launch_window_group(state: &Rc<RefCell<UiState>>, group: WindowGroup) {
    let (old_profile, old_new_tab_profile, old_same_directory) = {
        let state = state.borrow();
        (
            state.settings.selected_profile.clone(),
            state.settings.new_tab_profile.clone(),
            state.settings.new_tab_same_directory,
        )
    };
    for entry in group.entries {
        let valid = {
            let mut state = state.borrow_mut();
            if state.profiles.profile(&entry.profile).is_none() {
                false
            } else {
                state.profiles.select(&entry.profile);
                state.settings.selected_profile = entry.profile.clone();
                state.settings.new_tab_profile = "same".to_owned();
                state.settings.new_tab_same_directory = false;
                state.pending_working_directory = entry.working_directory.clone();
                true
            }
        };
        if !valid {
            continue;
        }
        open_tab(state);
        let mut state = state.borrow_mut();
        if let Some(tab) = state.sessions.active() {
            let id = tab.id;
            state
                .sessions
                .set_working_directory(id, entry.working_directory.as_deref());
            if let Some(terminal) = state.terminals.get(&id.get()) {
                terminal.set_size(entry.columns as i64, entry.rows as i64);
            }
        }
    }
    let mut state = state.borrow_mut();
    state.settings.selected_profile = old_profile.clone();
    state.settings.new_tab_profile = old_new_tab_profile;
    state.settings.new_tab_same_directory = old_same_directory;
    state.profiles.select(&old_profile);
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
        .logo_icon_name("core-terminal")
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

fn switch_tab(state: &Rc<RefCell<UiState>>, next: bool) {
    let mut state = state.borrow_mut();
    let id = if next {
        state.sessions.next_tab().map(|tab| tab.id)
    } else {
        state.sessions.previous_tab().map(|tab| tab.id)
    };
    if let Some(id) = id {
        state
            .stack
            .set_visible_child_name(&format!("tab-{}", id.get()));
    }
}

fn switch_tab_index(state: &Rc<RefCell<UiState>>, index: usize) {
    let mut state = state.borrow_mut();
    let Some(tab) = state.sessions.tabs().get(index) else {
        return;
    };
    let id = tab.id;
    state.sessions.select_tab(index);
    state
        .stack
        .set_visible_child_name(&format!("tab-{}", id.get()));
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
    let (id, terminal, spawn_options) = {
        let mut state_mut = state.borrow_mut();
        let profile_name = if state_mut.settings.new_tab_profile != "same"
            && state_mut
                .profiles
                .profile(&state_mut.settings.new_tab_profile)
                .is_some()
        {
            state_mut.settings.new_tab_profile.clone()
        } else {
            state_mut.profiles.selected_name().to_owned()
        };
        let working_directory = state_mut.pending_working_directory.take().or_else(|| {
            if state_mut.settings.new_tab_same_directory {
                state_mut
                    .sessions
                    .active()
                    .and_then(|tab| tab.working_directory.clone())
            } else {
                None
            }
        });
        let id = state_mut.sessions.open_tab(&profile_name, None);
        let terminal = vte4::Terminal::new();
        enforce_terminal_input_safety(&terminal);
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        let profile = state_mut.profiles.profile(&profile_name).cloned();
        if let Some(profile) = &profile {
            apply_profile(&terminal, profile, &state_mut.settings);
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
        (id, terminal, spawn_options)
    };
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
            core::ChildExitDecision::CloseWindow => exit_state.borrow().window.close(),
            core::ChildExitDecision::CloseTab => force_close_tab(&exit_state, id),
            core::ChildExitDecision::Keep => {
                if let Ok(mut state) = exit_state.try_borrow_mut() {
                    state.sessions.clear_child_pid(id);
                }
            }
            core::ChildExitDecision::Ask => {
                if let Ok(mut state) = exit_state.try_borrow_mut() {
                    state.sessions.clear_child_pid(id);
                }
                show_child_exit_prompt(&exit_state, id, status);
            }
        }
    });
    // Connect all lifecycle observers before spawning. A direct command such
    // as `/bin/true` can exit before the next main-loop turn; registering the
    // child-exited handler only after spawn would lose that event.
    let spawn_state = state.clone();
    core::spawn_terminal(&terminal, &spawn_options, move |result| {
        match result {
            Ok(pid) => match spawn_state.try_borrow_mut() {
                Ok(mut state) if state.sessions.tab(id).is_some() => {
                    state.sessions.set_child_pid(id, Some(pid));
                }
                // The tab may have been closed, or a nested callback may
                // still hold the model borrow. In either case never leave
                // the newly spawned process orphaned.
                _ => core::terminate_child(pid),
            },
            Err(error) => eprintln!("Core Terminal: failed to start tab {id:?}: {error}"),
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
    terminal.set_size(profile.columns as i64, profile.rows as i64);
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
    let should_prompt = {
        let state = state.borrow();
        let Some(tab) = state.sessions.tab(id) else {
            return;
        };
        if tab.child_pid.is_none() {
            false
        } else {
            state
                .profiles
                .profile(&tab.profile_name)
                .map(|profile| core::should_prompt_before_close(profile, None))
                .unwrap_or(false)
        }
    };
    if should_prompt {
        show_running_close_prompt(state, id);
    } else {
        force_close_tab(state, id);
    }
}

fn show_running_close_prompt(state: &Rc<RefCell<UiState>>, id: SessionId) {
    let parent = state.borrow().window.clone();
    let dialog = gtk::Window::builder()
        .title("Close running terminal?")
        .transient_for(&parent)
        .modal(false)
        .default_width(430)
        .default_height(160)
        .build();
    enforce_non_modal(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    let label = gtk::Label::new(Some(
        "A process is still running in this tab. Close it and terminate the process?",
    ));
    label.set_wrap(true);
    label.set_xalign(0.0);
    content.append(&label);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let close = gtk::Button::with_label("Close Tab");
    let cancel_dialog = dialog.clone();
    cancel.connect_clicked(move |_| cancel_dialog.close());
    let close_dialog = dialog.clone();
    let close_state = state.clone();
    close.connect_clicked(move |_| {
        force_close_tab(&close_state, id);
        close_dialog.close();
    });
    actions.append(&cancel);
    actions.append(&close);
    content.append(&actions);
    dialog.set_child(Some(&content));
    dialog.present();
}

fn force_close_tab(state: &Rc<RefCell<UiState>>, id: SessionId) {
    let Ok(mut state) = state.try_borrow_mut() else {
        return;
    };
    if let Some(tab) = state.sessions.close_tab(id) {
        if let Some(pid) = tab.child_pid {
            core::terminate_child(pid);
        }
        if let Some(terminal) = state.terminals.remove(&id.get()) {
            if let Some(page_child) = stack_page_child(&state.stack, &terminal) {
                state.stack.remove(&page_child);
            }
        }
    }
    let close_window = state.sessions.is_empty();
    let window = state.window.clone();
    drop(state);
    if close_window {
        window.close();
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
