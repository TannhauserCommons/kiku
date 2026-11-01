//! Prints the apps currently playing audio, as discovery sees them.
use kiku::{processes::snapshot, sessions::Sessions};

fn main() {
    wasapi::initialize_mta().ok().unwrap();
    let (wake, _) = std::sync::mpsc::channel();
    for app in Sessions::new(wake).playing_apps(&snapshot()).unwrap() {
        println!("{} (pid {})", app.name, app.pid);
    }
}
