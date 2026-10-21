#![windows_subsystem = "windows"]

mod compress;
mod gui;

fn main() {
    if let Err(error) = gui::run() {
        gui::show_error(&error);
    }
}
