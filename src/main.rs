mod app;
mod auth;
mod network;
mod server;
mod ui;
mod upload;

fn main() {
    if let Err(error) = app::run() {
        eprintln!("remagic-upload: {error:#}");
        std::process::exit(1);
    }
}
