//! Transcript variables: who said it (`WHO`) and how much of it (`HOW_MUCH`), over the captions in memory.

use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime};

use crate::captions::{Caption, Captions, YOU, YOU_SECTION};

/// A caption line as transcript variables see it.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptLine {
    /// An app's label, or "You".
    pub speaker: String,
    /// When it was first heard, local time.
    pub time: NaiveDateTime,
    pub text: String,
    pub complete: bool,
}

/// Whose lines.
#[derive(Clone, Debug, PartialEq)]
pub enum Who {
    /// An app, by its label (any case).
    App(String),
    You,
    /// Everyone but you.
    Others,
    Everyone,
    /// The app in front.
    Front,
}

/// How much of what they said.
#[derive(Clone, Debug, PartialEq)]
pub enum HowMuch {
    /// The newest finished line.
    Line,
    Lines(usize),
    /// Lines heard this long ago or later.
    Last(Duration),
    /// Back to the last pause of 3 s or more.
    Paragraph,
    /// Everything since your last line.
    SinceMe,
    /// Everything since Kiku started.
    Session,
}

/// Lines in the order they were heard.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    lines: Vec<TranscriptLine>,
}

/// A pause this long (or longer) between two lines ends a paragraph.
const PARAGRAPH_GAP: chrono::Duration = chrono::Duration::seconds(3);

impl Transcript {
    /// `lines` must be in the order they were heard.
    pub fn new(lines: Vec<TranscriptLine>) -> Transcript {
        Transcript { lines }
    }

    /// The finished lines in the captions, merged in time order. Your mic lines copied into apps' sections
    /// are left out: the You section has them.
    pub fn from_captions(captions: &Captions) -> Transcript {
        let mut lines: Vec<TranscriptLine> = captions
            .sections
            .iter()
            .enumerate()
            .flat_map(|(index, section)| {
                let own = move |caption: &&Caption| index == YOU_SECTION || caption.speaker != Some(YOU);
                section.captions.iter().filter(own).map(|caption| TranscriptLine {
                    speaker: section.label.clone(),
                    time: DateTime::<Local>::from(caption.time).naive_local(),
                    text: caption.text.clone(),
                    complete: caption.complete,
                })
            })
            .collect();
        lines.sort_by_key(|line| line.time);
        Transcript::new(lines)
    }

    /// The finished lines `who` said, `how_much` of them, oldest first; `front` names the app in front.
    /// Walks back from the newest line and stops as soon as it has enough.
    pub fn select(&self, who: &Who, how_much: &HowMuch, front: &str, now: NaiveDateTime) -> Vec<TranscriptLine> {
        let finished = self.lines.iter().rev().filter(|line| line.complete);
        let mut wanted = finished.clone().filter(|line| who.includes(&line.speaker, front));
        let mut picked: Vec<&TranscriptLine> = match how_much {
            HowMuch::Line => wanted.by_ref().take(1).collect(),
            HowMuch::Lines(count) => wanted.by_ref().take(*count).collect(),
            HowMuch::Last(span) => {
                let since = chrono::Duration::from_std(*span).ok().and_then(|span| now.checked_sub_signed(span));
                wanted.take_while(|line| since.is_none_or(|since| line.time >= since)).collect()
            }
            HowMuch::Paragraph => {
                let mut later = None;
                wanted.take_while(|line| {
                    let same = later.is_none_or(|later: NaiveDateTime| later - line.time < PARAGRAPH_GAP);
                    later = Some(line.time);
                    same
                }).collect()
            }
            HowMuch::SinceMe => {
                let since_me = finished.take_while(|line| line.speaker != YOU);
                since_me.filter(|line| who.includes(&line.speaker, front)).collect()
            }
            HowMuch::Session => wanted.collect(),
        };
        picked.reverse();
        picked.into_iter().cloned().collect()
    }
}

impl Who {
    fn includes(&self, speaker: &str, front: &str) -> bool {
        match self {
            Who::App(label) => speaker.eq_ignore_ascii_case(label),
            Who::You => speaker == YOU,
            Who::Others => speaker != YOU,
            Who::Everyone => true,
            Who::Front => speaker.eq_ignore_ascii_case(front),
        }
    }
}
