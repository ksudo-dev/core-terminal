mod core;
mod profiles;
mod settings;
mod shortcuts;
mod ui;

pub const APPLICATION_ID: &str = "app.coreterminal.CoreTerminal";
pub const DISPLAY_NAME: &str = "Core Terminal";

fn main() {
    ui::run(APPLICATION_ID, DISPLAY_NAME);
}
