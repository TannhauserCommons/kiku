//! Speakers' downloads: turning speakers on queues the speaker model when it isn't here. It runs in a process
//! of its own because it points `KIKU_MODELS_DIR` at an empty folder, and starts no downloader (no network).

use kiku_core::{
    catalog,
    download::{speaker_model_to_fetch, Target},
};

#[test]
fn a_missing_speaker_model_is_queued_for_download() {
    let folder = std::env::temp_dir().join("kiku-speakers-test");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    // Nothing else in this process reads it meanwhile.
    unsafe { std::env::set_var("KIKU_MODELS_DIR", &folder) };
    assert!(catalog::speaker_model().is_none());

    let speakers = Target::Component(catalog::component(catalog::SPEAKERS));
    assert_eq!(speaker_model_to_fetch(), Some(speakers), "queued, like a model's components");
    let _ = std::fs::remove_dir_all(&folder);
}
