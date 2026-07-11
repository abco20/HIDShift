mod app;
mod browser_client;
mod settings_ui;
mod transport;

fn main() {
    leptos::mount::mount_to_body(app::App);
}
