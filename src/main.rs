mod app;
mod auth;
mod catalog;
mod network;
mod peer;
mod server;
mod trash;
mod ui;
mod upload;

fn main() {
    if let Err(error) = app::run() {
        eprintln!("remagic-upload: {error:#}");
        std::process::exit(1);
    }
}
