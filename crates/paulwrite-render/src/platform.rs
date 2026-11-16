//! Services Win32 : presse-papiers, boîtes de dialogue.
//!
//! Les boîtes de dialogue sont **modales et bloquantes** : à appeler depuis un worker (fichiers)
//! ou, pour la confirmation de fermeture, depuis le thread UI (l'utilisateur attend de toute façon).

use std::path::PathBuf;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDNO, IDYES, MB_ICONQUESTION, MB_YESNOCANCEL,
};

/// Texte du presse-papiers (s'il y en a).
#[must_use]
pub fn clipboard_get(owner: isize) -> Option<String> {
    // SAFETY: le presse-papiers est ouvert puis fermé dans cette fonction ; le bloc mémoire
    // renvoyé par Windows reste valide tant qu'il est verrouillé (GlobalLock/GlobalUnlock) ; le
    // texte est terminé par un 0 (CF_UNICODETEXT).
    unsafe {
        OpenClipboard(Some(HWND(owner as *mut _))).ok()?;
        let result = (|| {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal).cast::<u16>();
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(hglobal);
            Some(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Place du texte dans le presse-papiers. Renvoie `false` en cas d'échec.
pub fn clipboard_set(owner: isize, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    // SAFETY: bloc mémoire alloué, rempli puis donné à Windows (qui en devient propriétaire) ; en
    // cas d'échec avant le transfert, on le libère nous-mêmes.
    unsafe {
        let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            return false;
        };
        let ptr = GlobalLock(hmem).cast::<u16>();
        if ptr.is_null() {
            let _ = GlobalFree(Some(hmem));
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(hmem);

        if OpenClipboard(Some(HWND(owner as *mut _))).is_err() {
            let _ = GlobalFree(Some(hmem));
            return false;
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(hmem.0))).is_ok();
        if !ok {
            let _ = GlobalFree(Some(hmem));
        }
        let _ = CloseClipboard();
        ok
    }
}

/// Réponse à « Enregistrer les modifications ? ».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

/// Demande s'il faut enregistrer avant de fermer ou de remplacer le document.
#[must_use]
pub fn confirm_save(owner: isize, file_name: &str) -> SaveChoice {
    let text: Vec<u16> = format!("Enregistrer les modifications de « {file_name} » ?")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `text` est terminé par 0 et vit pendant l'appel (bloquant).
    let r = unsafe {
        MessageBoxW(
            Some(HWND(owner as *mut _)),
            PCWSTR(text.as_ptr()),
            w!("PaulWrite"),
            MB_YESNOCANCEL | MB_ICONQUESTION,
        )
    };
    if r == IDYES {
        SaveChoice::Save
    } else if r == IDNO {
        SaveChoice::Discard
    } else {
        SaveChoice::Cancel
    }
}

fn file_dialog(owner: isize, save: bool, initial: &str) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32_768];
    for (i, u) in initial.encode_utf16().take(buf.len() - 1).enumerate() {
        buf[i] = u;
    }
    // Filtre : paires « libellé\0motif\0 », terminées par un double 0.
    let filter: Vec<u16> = "Tous les fichiers\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut _),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        Flags: if save {
            OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST
        } else {
            OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST
        },
        ..Default::default()
    };
    // SAFETY: `ofn` référence `buf` et `filter`, tous deux vivants pendant l'appel bloquant.
    let ok = unsafe {
        if save {
            GetSaveFileNameW(&mut ofn)
        } else {
            GetOpenFileNameW(&mut ofn)
        }
    };
    if !ok.as_bool() {
        return None; // annulé (ou erreur)
    }
    let len = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// Boîte « Ouvrir » ; `None` si l'utilisateur annule. Bloquante : à appeler depuis un worker.
#[must_use]
pub fn pick_open_file(owner: isize) -> Option<PathBuf> {
    file_dialog(owner, false, "")
}

/// Boîte « Enregistrer sous » ; `None` si l'utilisateur annule. Bloquante : à appeler depuis un
/// worker.
#[must_use]
pub fn pick_save_file(owner: isize, suggested_name: &str) -> Option<PathBuf> {
    file_dialog(owner, true, suggested_name)
}
