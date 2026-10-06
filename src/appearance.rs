//! System appearance integration for GTK chrome.
//!
//! Terminal palettes remain profile-owned VTE properties. This module only
//! mirrors the desktop's documented interface preference into GTK's chrome.

use gtk::{gio, prelude::*};

const DESKTOP_INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const COLOR_SCHEME_KEY: &str = "color-scheme";

fn prefers_dark_chrome(color_scheme: &str) -> bool {
    color_scheme == "prefer-dark"
}

fn apply_chrome_color_scheme(settings: &gtk::Settings, color_scheme: &str) {
    settings.set_gtk_application_prefer_dark_theme(prefers_dark_chrome(color_scheme));
}

/// Follow the desktop interface preference for GTK windows, menus, tabs, and
/// buttons. A missing GNOME schema is a normal non-GNOME fallback: GTK keeps
/// its configured theme. VTE profiles are intentionally not changed here.
pub fn follow_system_appearance(app: &gtk::Application) {
    let Some(schema) = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(DESKTOP_INTERFACE_SCHEMA, true))
        .filter(|schema| schema.has_key(COLOR_SCHEME_KEY))
    else {
        return;
    };
    let Some(gtk_settings) = gtk::Settings::default() else {
        return;
    };

    let desktop_settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    apply_chrome_color_scheme(
        &gtk_settings,
        desktop_settings.string(COLOR_SCHEME_KEY).as_str(),
    );

    let gtk_settings_for_change = gtk_settings.clone();
    desktop_settings.connect_changed(Some(COLOR_SCHEME_KEY), move |settings, _| {
        apply_chrome_color_scheme(
            &gtk_settings_for_change,
            settings.string(COLOR_SCHEME_KEY).as_str(),
        );
    });

    // Keep the GSettings subscription alive for the GTK application lifetime.
    app.connect_shutdown(move |_| {
        let _keep_subscription_alive = &desktop_settings;
    });
}

#[cfg(test)]
mod tests {
    use super::prefers_dark_chrome;

    #[test]
    fn only_prefer_dark_selects_dark_chrome() {
        assert!(prefers_dark_chrome("prefer-dark"));
        assert!(!prefers_dark_chrome("default"));
        assert!(!prefers_dark_chrome("prefer-light"));
        assert!(!prefers_dark_chrome("unknown-future-value"));
    }
}
