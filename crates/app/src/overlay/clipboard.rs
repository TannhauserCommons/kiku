//! Putting text on the clipboard, and lending the clipboard to a paste.

use windows::{
    core::{w, Result, PCWSTR},
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
                RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
            Ole::{
                CF_BITMAP, CF_DIB, CF_DIBV5, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_ENHMETAFILE, CF_GDIOBJFIRST,
                CF_GDIOBJLAST, CF_METAFILEPICT, CF_OEMTEXT, CF_OWNERDISPLAY, CF_PALETTE, CF_PRIVATEFIRST, CF_PRIVATELAST, CF_TEXT,
                CF_UNICODETEXT,
            },
        },
    },
};

/// Formats whose data is a bitmap or other GDI object rather than memory; Windows remakes the
/// common ones from the formats that are kept.
const NOT_MEMORY: [u16; 8] = [
    CF_BITMAP.0, CF_METAFILEPICT.0, CF_PALETTE.0, CF_ENHMETAFILE.0, CF_OWNERDISPLAY.0, CF_DSPBITMAP.0, CF_DSPMETAFILEPICT.0, CF_DSPENHMETAFILE.0,
];
/// Formats Windows makes from a fuller one whenever that one is on the clipboard, as (fuller, made from
/// it): ANSI and OEM text from Unicode text, a plain bitmap from one with alpha. Only the fuller is
/// kept, so a screenshot isn't copied twice; CF_LOCALE stays, as it says which code page the text used.
const REMADE: [(u16, &[u16]); 2] = [(CF_UNICODETEXT.0, &[CF_TEXT.0, CF_OEMTEXT.0]), (CF_DIBV5.0, &[CF_DIB.0])];
/// Marks that keep clipboard contents out of Windows' clipboard history, the cloud clipboard and clipboard managers.
const PRIVATE_MARKS: [PCWSTR; 3] = [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("CanIncludeInClipboardHistory"), w!("CanUploadToCloudClipboard")];

/// What was on the clipboard: each format and its bytes.
pub struct Contents(Vec<(u32, Vec<u8>)>);

/// Replaces the clipboard's contents with `text`.
pub fn copy(owner: HWND, text: &str) -> Result<()> {
    replace(owner, &[text_format(text)])
}

/// Puts `text` on the clipboard for a paste, kept out of clipboard history.
pub fn lend(owner: HWND, text: &str) -> Result<()> {
    replace(owner, &[vec![text_format(text)], private_marks()].concat())
}

/// Puts back what [`contents`] read, kept out of clipboard history since it is already there.
pub fn restore(owner: HWND, contents: &Contents) -> Result<()> {
    if contents.0.is_empty() {
        return replace(owner, &[]);
    }
    replace(owner, &[contents.0.clone(), private_marks()].concat())
}

/// The text on the clipboard, if any.
pub fn text(owner: HWND) -> Option<String> {
    unsafe { OpenClipboard(Some(owner)).ok()? };
    let bytes = read(CF_UNICODETEXT.0 as u32);
    let _ = unsafe { CloseClipboard() };
    let units: Vec<u16> = bytes?.as_chunks::<2>().0.iter().map(|&pair| u16::from_le_bytes(pair)).take_while(|&unit| unit != 0).collect();
    Some(String::from_utf16_lossy(&units))
}

/// Everything on the clipboard now that is held as memory, less what Windows remakes from the rest.
pub fn contents(owner: HWND) -> Contents {
    let mut formats = Vec::new();
    if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if remade(format) {
                continue;
            }
            if let Some(bytes) = read(format) {
                formats.push((format, bytes));
            }
        }
        let _ = unsafe { CloseClipboard() };
    }
    Contents(formats)
}

/// Whether Windows makes `format` from a fuller format that is on the open clipboard.
fn remade(format: u32) -> bool {
    let available = |fuller: u16| unsafe { IsClipboardFormatAvailable(fuller as u32) }.is_ok();
    REMADE.iter().any(|&(fuller, made)| made.iter().any(|&m| m as u32 == format) && available(fuller))
}

/// A format's bytes, unless its data is a GDI object or a handle only its owner understands.
fn read(format: u32) -> Option<Vec<u8>> {
    let kind = u16::try_from(format).unwrap_or(u16::MAX);
    let special = (CF_PRIVATEFIRST.0..=CF_PRIVATELAST.0).contains(&kind) || (CF_GDIOBJFIRST.0..=CF_GDIOBJLAST.0).contains(&kind);
    if special || NOT_MEMORY.contains(&kind) {
        return None;
    }
    let memory = HGLOBAL(unsafe { GetClipboardData(format) }.ok()?.0);
    let size = unsafe { GlobalSize(memory) };
    let data = unsafe { GlobalLock(memory) };
    if size == 0 || data.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) }.to_vec();
    let _ = unsafe { GlobalUnlock(memory) };
    Some(bytes)
}

/// Empties the clipboard and puts `formats` on it.
fn replace(owner: HWND, formats: &[(u32, Vec<u8>)]) -> Result<()> {
    unsafe {
        OpenClipboard(Some(owner))?;
        let placed = EmptyClipboard().and_then(|()| formats.iter().try_for_each(|(format, bytes)| place(*format, bytes)));
        let _ = CloseClipboard();
        placed
    }
}

/// Puts one format on the open clipboard, which then owns the memory.
unsafe fn place(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), GlobalLock(memory).cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(memory);
        SetClipboardData(format, Some(HANDLE(memory.0))).map(|_| ()).inspect_err(|_| {
            let _ = GlobalFree(Some(memory));
        })
    }
}

fn text_format(text: &str) -> (u32, Vec<u8>) {
    (CF_UNICODETEXT.0 as u32, text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect())
}

fn private_marks() -> Vec<(u32, Vec<u8>)> {
    PRIVATE_MARKS.iter().map(|&name| (unsafe { RegisterClipboardFormatW(name) }, 0u32.to_le_bytes().to_vec())).collect()
}
