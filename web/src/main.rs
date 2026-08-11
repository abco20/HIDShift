mod app;
mod browser_client;
mod command_effect;
mod settings_ui;
mod state;
mod transport;

fn main() {
    leptos::mount::mount_to_body(app::App);
}
