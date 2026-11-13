//! Saved voices: the voices the user named (`.loop/speakers/NAMES.md`, N1). A voice print is biometric data:
//! it is never logged or printed (`Print`'s `Debug` hides it), is stored only through a [`VoiceStore`] (the
//! app's is encrypted for the Windows user) and never leaves Kiku's own processes.

use std::{
    fmt,
    sync::Mutex,
};

use crate::{engine::Result, log};

/// Names a saved voice; never reused.
pub type VoiceId = u32;

/// Audio a print rests on counts for at most this long (seconds), so recent sessions still move it.
const MAX_SECONDS: f32 = 600.0;
/// The file starts with this, so another file or a newer format is not read as a book.
const MAGIC: &[u8; 4] = b"KVB2";
const MAX_VOICES: usize = 10_000;
const MAX_TEXT: usize = 512;
const MAX_DIMENSIONS: usize = 1024;

/// A speaker's voice print: the mean of its embeddings, scaled to length 1 (so only the direction counts).
#[derive(Clone, PartialEq)]
pub struct Print(Vec<f32>);

impl Print {
    /// The print of `values`; `None` when they are empty, not finite or all zero.
    pub fn new(values: &[f32]) -> Option<Self> {
        let norm = values.iter().map(|x| x * x).sum::<f32>().sqrt();
        (!values.is_empty() && norm.is_finite() && norm > 0.0).then(|| Self(values.iter().map(|x| x / norm).collect()))
    }

    pub fn values(&self) -> &[f32] {
        &self.0
    }

    /// Cosine similarity with `other` (-1 to 1); both have length 1.
    pub fn similarity(&self, other: &Print) -> f32 {
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum()
    }

    /// The mean of this print (resting on `seconds`) and `other` (resting on `other_seconds`).
    pub fn blended(&self, seconds: f32, other: &Print, other_seconds: f32) -> Print {
        let total = (seconds + other_seconds).max(f32::EPSILON);
        let mixed: Vec<f32> = self.0.iter().zip(&other.0).map(|(a, b)| (a * seconds + b * other_seconds) / total).collect();
        Print::new(&mixed).unwrap_or_else(|| self.clone())
    }
}

impl fmt::Debug for Print {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Print({} values)", self.0.len())
    }
}

/// Where a stream's audio comes from: voices are recognised within a place.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Place {
    /// The microphone.
    Mic,
    /// One app (by its label): speakers are numbered per app.
    App(String),
    /// Every app at once, when speakers are recognised across apps.
    AllApps,
}

impl Place {
    /// The place of the app labelled `label`: its own, or all apps' when speakers are recognised across them.
    pub fn of_app(label: &str, across_apps: bool) -> Self {
        if across_apps { Place::AllApps } else { Place::App(label.to_owned()) }
    }
}

/// Where a saved voice is recognised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceScope {
    /// In every app and at the mic.
    Everywhere,
    /// Only in the app with this label.
    App(String),
}

impl VoiceScope {
    /// Whether a stream from `place` may be given the voice. `app` is the stream's app label (none for the mic).
    pub fn covers(&self, app: Option<&str>) -> bool {
        match self {
            VoiceScope::Everywhere => true,
            VoiceScope::App(label) => app == Some(label.as_str()),
        }
    }
}

/// A voice the user named.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedVoice {
    pub id: VoiceId,
    pub name: String,
    pub print: Print,
    /// The audio the print rests on, in seconds (capped).
    pub seconds: f32,
    pub scope: VoiceScope,
    /// The app it was named in, which "only in this app" limits it to (none when named at the mic).
    pub home: Option<String>,
}

/// A saved voice as the settings list it: everything but its print.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceInfo {
    pub id: VoiceId,
    pub name: String,
    pub scope: VoiceScope,
    pub home: Option<String>,
}

/// The saved voices, and the format they are stored in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceBook {
    voices: Vec<SavedVoice>,
    next_id: VoiceId,
}

impl VoiceBook {
    pub fn voices(&self) -> &[SavedVoice] {
        &self.voices
    }

    pub fn get(&self, id: VoiceId) -> Option<&SavedVoice> {
        self.voices.iter().find(|voice| voice.id == id)
    }

    /// Saves a new voice, anyone's `name`d in the app `home`, resting on `seconds` of audio.
    pub fn add(&mut self, name: &str, print: Print, seconds: f32, scope: VoiceScope, home: Option<&str>) -> VoiceId {
        let id = self.next_id;
        self.next_id += 1;
        let home = home.map(clean_name);
        self.voices.push(SavedVoice { id, name: clean_name(name), print, seconds: seconds.min(MAX_SECONDS), scope: clean_scope(scope), home });
        id
    }

    pub fn rename(&mut self, id: VoiceId, name: &str) -> bool {
        self.change(id, |voice| voice.name = clean_name(name))
    }

    pub fn set_scope(&mut self, id: VoiceId, scope: VoiceScope) -> bool {
        self.change(id, |voice| voice.scope = clean_scope(scope))
    }

    pub fn forget(&mut self, id: VoiceId) -> bool {
        let before = self.voices.len();
        self.voices.retain(|voice| voice.id != id);
        self.voices.len() != before
    }

    /// Folds `seconds` of audio heard as this voice, with mean `heard`, into its print (a mean weighted by the
    /// audio each rests on; what rests on more than ten minutes counts as ten, so it keeps adapting).
    pub fn learn(&mut self, id: VoiceId, heard: &Print, seconds: f32) -> bool {
        self.change(id, |voice| {
            voice.print = voice.print.blended(voice.seconds, heard, seconds);
            voice.seconds = (voice.seconds + seconds).min(MAX_SECONDS);
        })
    }

    fn change(&mut self, id: VoiceId, edit: impl FnOnce(&mut SavedVoice)) -> bool {
        self.voices.iter_mut().find(|voice| voice.id == id).map(edit).is_some()
    }

    /// The book as bytes to store.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(self.next_id.to_le_bytes());
        bytes.extend((self.voices.len() as u32).to_le_bytes());
        for voice in &self.voices {
            bytes.extend(voice.id.to_le_bytes());
            bytes.extend(voice.seconds.to_le_bytes());
            match &voice.scope {
                VoiceScope::Everywhere => bytes.push(0),
                VoiceScope::App(label) => {
                    bytes.push(1);
                    push_text(&mut bytes, label);
                }
            }
            push_text(&mut bytes, &voice.name);
            push_text(&mut bytes, voice.home.as_deref().unwrap_or_default());
            bytes.extend((voice.print.0.len() as u32).to_le_bytes());
            bytes.extend(voice.print.0.iter().flat_map(|x| x.to_le_bytes()));
        }
        bytes
    }

    /// A book from bytes written by [`to_bytes`](Self::to_bytes); an error when they are anything else.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader(bytes);
        if reader.take(4)? != MAGIC {
            return Err("not a voice book".into());
        }
        let next_id = reader.u32()?;
        let count = reader.u32()? as usize;
        if count > MAX_VOICES {
            return Err("too many voices".into());
        }
        let mut voices = Vec::with_capacity(count);
        for _ in 0..count {
            let id = reader.u32()?;
            let seconds = reader.f32()?;
            let scope = match reader.take(1)?[0] {
                0 => VoiceScope::Everywhere,
                1 => VoiceScope::App(reader.text()?),
                _ => return Err("unknown scope".into()),
            };
            let name = reader.text()?;
            let home = Some(reader.text()?).filter(|home| !home.is_empty());
            let dimensions = reader.u32()? as usize;
            if dimensions > MAX_DIMENSIONS {
                return Err("a print is too long".into());
            }
            let values: Vec<f32> = (0..dimensions).map(|_| reader.f32()).collect::<Result<_>>()?;
            let print = Print::new(&values).ok_or("a print is empty")?;
            if !seconds.is_finite() || id >= next_id {
                return Err("a voice is garbled".into());
            }
            voices.push(SavedVoice { id, name, print, seconds: seconds.clamp(0.0, MAX_SECONDS), scope, home });
        }
        if !reader.0.is_empty() {
            return Err("trailing bytes".into());
        }
        Ok(Self { voices, next_id })
    }
}

/// `name` trimmed, or the reason it can't name a voice, for the panel to show.
pub fn valid_name(name: &str) -> std::result::Result<String, String> {
    match clean_name(name) {
        name if name.is_empty() => Err("A name can't be blank.".into()),
        name => Ok(name),
    }
}

fn clean_name(name: &str) -> String {
    name.trim().chars().take(MAX_TEXT / 4).collect()
}

fn clean_scope(scope: VoiceScope) -> VoiceScope {
    match scope {
        VoiceScope::App(label) => VoiceScope::App(clean_name(&label)),
        everywhere => everywhere,
    }
}

fn push_text(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend((text.len() as u16).to_le_bytes());
    bytes.extend(text.as_bytes());
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n).ok_or("the voice book is cut short")?;
        self.0 = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn text(&mut self) -> Result<String> {
        let length = u16::from_le_bytes(self.take(2)?.try_into()?) as usize;
        if length > MAX_TEXT {
            return Err("a name is too long".into());
        }
        Ok(String::from_utf8(self.take(length)?.to_vec())?)
    }
}

/// Where the book's bytes are kept. The app's keeps them encrypted for the Windows user.
pub trait VoiceStore: Send {
    /// The stored bytes, or none when nothing was stored yet.
    fn load(&self) -> Result<Option<Vec<u8>>>;
    fn save(&self, bytes: &[u8]) -> Result<()>;
    /// Keeps what couldn't be read as a backup, so the next save can't destroy it.
    fn set_aside(&self) {}
    /// Why nothing can be saved now (the stored voices couldn't be read or kept), for the panel to show.
    fn refusal(&self) -> Option<String> {
        None
    }
}

/// Keeps the bytes in memory only: nothing survives the run. For tests and tools; the app keeps an encrypted file.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<Vec<u8>>>);

impl VoiceStore for MemoryStore {
    fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn save(&self, bytes: &[u8]) -> Result<()> {
        *self.0.lock().unwrap() = Some(bytes.to_vec());
        Ok(())
    }
}

/// A request to name the speaker shown as voice `voice` (its zero-based number) in `place`.
#[derive(Clone, Debug, PartialEq)]
pub struct NameRequest {
    pub place: Place,
    pub voice: u32,
    pub name: String,
    /// The app whose speaker it is (none for the mic).
    pub app: Option<String>,
}

/// The saved voices, shared by the panel (which names, renames and forgets) and the decoder (which
/// recognises them); every change is stored at once.
pub struct SavedVoices {
    book: Mutex<VoiceBook>,
    store: Mutex<Box<dyn VoiceStore>>,
    requests: Mutex<Vec<NameRequest>>,
}

impl SavedVoices {
    /// The voices in `store`; none when it holds none or can't be read (said in the log, never what it held).
    pub fn new(store: Box<dyn VoiceStore>) -> Self {
        let book = match store.load().and_then(|bytes| bytes.map(|bytes| VoiceBook::from_bytes(&bytes)).transpose()) {
            Ok(book) => book.unwrap_or_default(),
            Err(error) => {
                log::write(format!("the saved voices couldn't be read ({error}), so none are known"));
                store.set_aside();
                VoiceBook::default()
            }
        };
        Self { book: Mutex::new(book), store: Mutex::new(store), requests: Mutex::new(Vec::new()) }
    }

    /// Saved voices that live in memory only.
    pub fn in_memory() -> Self {
        Self::new(Box::new(MemoryStore::default()))
    }

    /// The voices to list, oldest first.
    pub fn list(&self) -> Vec<VoiceInfo> {
        let book = self.book.lock().unwrap();
        book.voices().iter().map(|voice| VoiceInfo { id: voice.id, name: voice.name.clone(), scope: voice.scope.clone(), home: voice.home.clone() }).collect()
    }

    /// A copy of the book, for seeding streams.
    pub fn snapshot(&self) -> VoiceBook {
        self.book.lock().unwrap().clone()
    }

    /// Why changes aren't being saved this run, if they aren't.
    pub fn refusal(&self) -> Option<String> {
        self.store.lock().unwrap().refusal()
    }

    pub fn add(&self, name: &str, print: Print, seconds: f32, scope: VoiceScope, home: Option<&str>) -> VoiceId {
        self.edit(|book| book.add(name, print, seconds, scope, home))
    }

    /// Renames a voice; an error, for the panel to show, when the name is blank or the voice isn't saved.
    pub fn rename(&self, id: VoiceId, name: &str) -> std::result::Result<(), String> {
        let name = valid_name(name)?;
        self.refusal().map_or(Ok(()), Err)?;
        self.edit(|book| book.rename(id, &name)).then_some(()).ok_or_else(|| "That voice isn't saved any more.".into())
    }

    pub fn set_scope(&self, id: VoiceId, scope: VoiceScope) -> bool {
        self.edit(|book| book.set_scope(id, scope))
    }

    /// Deletes the voice from the store at once.
    pub fn forget(&self, id: VoiceId) -> bool {
        self.edit(|book| book.forget(id))
    }

    pub fn learn(&self, id: VoiceId, heard: &Print, seconds: f32) -> bool {
        self.edit(|book| book.learn(id, heard, seconds))
    }

    /// Asks the decoder, which holds the speakers' prints, to name a speaker.
    /// An error, for the panel to show, when the name is blank.
    pub fn request_name(&self, request: NameRequest) -> std::result::Result<(), String> {
        let name = valid_name(&request.name)?;
        self.refusal().map_or(Ok(()), Err)?;
        self.requests.lock().unwrap().push(NameRequest { name, ..request });
        Ok(())
    }

    pub fn take_name_requests(&self) -> Vec<NameRequest> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }

    fn edit<T>(&self, change: impl FnOnce(&mut VoiceBook) -> T) -> T {
        let mut book = self.book.lock().unwrap();
        if self.refusal().is_some() {
            return change(&mut book.clone());
        }
        let before = book.clone();
        let result = change(&mut book);
        if *book != before && let Err(error) = self.store.lock().unwrap().save(&book.to_bytes()) {
            log::write(format!("the saved voices couldn't be stored: {error}"));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print(values: &[f32]) -> Print {
        Print::new(values).unwrap()
    }

    #[test]
    fn a_print_has_length_one_and_hides_its_values() {
        let p = print(&[3.0, 4.0]);
        assert_eq!(p.values(), [0.6, 0.8]);
        assert!((p.similarity(&print(&[0.0, 1.0])) - 0.8).abs() < 1e-6);
        assert_eq!(format!("{p:?}"), "Print(2 values)");
        assert!(Print::new(&[]).is_none() && Print::new(&[0.0, 0.0]).is_none() && Print::new(&[f32::NAN, 1.0]).is_none());
    }

    #[test]
    fn the_book_survives_bytes() {
        let mut book = VoiceBook::default();
        let alex = book.add("  Alex ", print(&[1.0, 0.0, 0.5]), 42.0, VoiceScope::Everywhere, None);
        let sam = book.add("Sam Ünal", print(&[0.0, 1.0, 0.0]), 1e6, VoiceScope::App("Discord".into()), Some("Discord"));
        book.forget(alex);
        let again = VoiceBook::from_bytes(&book.to_bytes()).unwrap();
        assert_eq!(again, book);
        assert_eq!(again.get(sam).unwrap().seconds, MAX_SECONDS);
        assert_eq!(again.get(sam).unwrap().home.as_deref(), Some("Discord"), "where it was named is kept");
        assert_eq!(again.next_id, 2, "an id is never reused, even of a forgotten voice");
    }

    #[test]
    fn bytes_that_are_not_a_book_are_refused() {
        let mut book = VoiceBook::default();
        book.add("Alex", print(&[1.0, 2.0]), 5.0, VoiceScope::Everywhere, None);
        let good = book.to_bytes();
        for cut in [0, 3, 10, good.len() - 1] {
            assert!(VoiceBook::from_bytes(&good[..cut]).is_err(), "cut at {cut}");
        }
        assert!(VoiceBook::from_bytes(&[good.clone(), vec![0]].concat()).is_err());
        let mut wrong = good.clone();
        wrong[0] = b'X';
        assert!(VoiceBook::from_bytes(&wrong).is_err());
        let mut huge = good;
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(VoiceBook::from_bytes(&huge).is_err(), "a count the file can't hold");
    }

    #[test]
    fn learning_weighs_by_audio_and_caps_it() {
        let mut book = VoiceBook::default();
        let id = book.add("Alex", print(&[1.0, 0.0]), 30.0, VoiceScope::Everywhere, None);
        book.learn(id, &print(&[0.0, 1.0]), 10.0);
        let voice = book.get(id).unwrap();
        assert_eq!(voice.seconds, 40.0);
        assert!(voice.print.values()[0] > voice.print.values()[1], "30 s against 10 s");
        book.learn(id, &print(&[0.0, 1.0]), 1e6);
        assert_eq!(book.get(id).unwrap().seconds, MAX_SECONDS);
        assert!(!book.learn(99, &print(&[1.0, 0.0]), 1.0));
    }

    #[test]
    fn a_voice_is_covered_by_its_scope() {
        assert!(VoiceScope::Everywhere.covers(None) && VoiceScope::Everywhere.covers(Some("Discord")));
        let only = VoiceScope::App("Discord".into());
        assert!(only.covers(Some("Discord")) && !only.covers(Some("Edge")) && !only.covers(None));
    }

    #[test]
    fn saved_voices_are_stored_at_every_change_and_forgotten_at_once() {
        struct Shared(std::sync::Arc<MemoryStore>);
        impl VoiceStore for Shared {
            fn load(&self) -> Result<Option<Vec<u8>>> {
                self.0.load()
            }
            fn save(&self, bytes: &[u8]) -> Result<()> {
                self.0.save(bytes)
            }
        }
        let stored = std::sync::Arc::new(MemoryStore::default());
        let voices = SavedVoices::new(Box::new(Shared(stored.clone())));
        let id = voices.add("Alex", print(&[1.0, 0.0]), 20.0, VoiceScope::Everywhere, None);
        assert!(voices.rename(id, "Alexa").is_ok());
        assert!(voices.rename(id, "  ").is_err() && voices.rename(99, "Nobody").is_err());
        assert_eq!(voices.snapshot().get(id).unwrap().name, "Alexa", "a blank name changes nothing");
        let reopened = SavedVoices::new(Box::new(Shared(stored.clone())));
        assert_eq!(reopened.snapshot().get(id).unwrap().name, "Alexa");
        assert!(voices.forget(id));
        assert!(VoiceBook::from_bytes(&stored.load().unwrap().unwrap()).unwrap().voices().is_empty());
    }

    #[test]
    fn a_garbled_store_means_no_voices_and_no_crash() {
        let store = MemoryStore::default();
        store.save(b"garbage").unwrap();
        assert!(SavedVoices::new(Box::new(store)).snapshot().voices().is_empty());
    }
}
