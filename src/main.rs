mod appearance;
mod connections;
mod context_menu;
mod core;
mod inspector;
mod profiles;
mod session_restore;
mod settings;
mod shortcuts;
mod ui;

pub const APPLICATION_ID: &str = "io.github.ksudo_dev.CoreTerminal";
pub const DISPLAY_NAME: &str = "Core Terminal";

fn main() {
    ui::run(APPLICATION_ID, DISPLAY_NAME);
}
