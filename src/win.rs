use crate::repo::ConflictChoice;
use std::{path::Path, process::Command};
use windows::{core::HSTRING, Win32::UI::WindowsAndMessaging::*};

pub fn prompt_conflict(game: &str, local: &str, remote: &str) -> ConflictChoice {
    let text = format!(
        "SaveOwl found divergent save history for {game}.\n\nLocal: {local}\nUpstream: {remote}\n\nYES = keep local and publish it\nNO = restore upstream\nCANCEL = keep both branches (no overwrite)"
    );
    unsafe {
        let r = MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("SaveOwl save conflict"), MB_YESNOCANCEL | MB_ICONWARNING | MB_SETFOREGROUND);
        match r { IDYES => ConflictChoice::KeepLocal, IDNO => ConflictChoice::UseUpstream, _ => ConflictChoice::KeepBoth }
    }
}

pub fn open_folder(path: &Path) { let _ = Command::new("explorer.exe").arg(path).spawn(); }
