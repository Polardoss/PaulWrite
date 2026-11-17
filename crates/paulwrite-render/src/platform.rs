//! Services Win32 : presse-papiers, boîtes de dialogue, barre de menus.
//!
//! Les boîtes de dialogue sont **modales et bloquantes** : à appeler depuis un worker (fichiers)
//! ou, pour la confirmation de fermeture, depuis le thread UI (l'utilisateur attend de toute façon).

use std::path::PathBuf;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateMenu, CreatePopupMenu, DestroyMenu, DrawMenuBar, GetMenu, MessageBoxW,
    SetMenu, IDNO, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNOCANCEL, MF_POPUP,
    MF_SEPARATOR, MF_STRING, MSG, WM_COMMAND,
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
pub fn confirm_save(owner: isize, prompt: &str) -> SaveChoice {
    let text: Vec<u16> = prompt.encode_utf16().chain(std::iter::once(0)).collect();
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

/// Affiche un message d'erreur (boîte modale, bouton OK).
pub fn show_error(owner: isize, text: &str) {
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `text` est terminé par 0 et vit pendant l'appel (bloquant).
    unsafe {
        MessageBoxW(
            Some(HWND(owner as *mut _)),
            PCWSTR(text.as_ptr()),
            w!("PaulWrite"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn file_dialog(owner: isize, save: bool, initial: &str, all_files: &str) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32_768];
    for (i, u) in initial.encode_utf16().take(buf.len() - 1).enumerate() {
        buf[i] = u;
    }
    // Filtre : paires « libellé\0motif\0 », terminées par un double 0.
    let filter: Vec<u16> = format!("{all_files}\0*.*\0\0").encode_utf16().collect();
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
pub fn pick_open_file(owner: isize, all_files: &str) -> Option<PathBuf> {
    file_dialog(owner, false, "", all_files)
}

/// Boîte « Enregistrer sous » ; `None` si l'utilisateur annule. Bloquante : à appeler depuis un
/// worker.
#[must_use]
pub fn pick_save_file(owner: isize, suggested_name: &str, all_files: &str) -> Option<PathBuf> {
    file_dialog(owner, true, suggested_name, all_files)
}

/// Boîte « Ouvrir un dossier » ; `None` si l'utilisateur annule. Bloquante : à appeler depuis un
/// worker (elle initialise COM sur le thread appelant).
#[must_use]
pub fn pick_folder(owner: isize) -> Option<PathBuf> {
    // SAFETY: appels COM sur le thread courant ; la boîte et le texte renvoyé sont libérés ici.
    unsafe {
        // S_FALSE (déjà initialisé) est sans conséquence.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.Show(Some(HWND(owner as *mut _))).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}

/// Une entrée d'un menu déroulant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    /// Identifiant renvoyé par [`menu_command`] et intitulé (`\t` sépare le raccourci affiché).
    Item(u32, String),
    Separator,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Installe une barre de menus native sur la fenêtre. Renvoie `false` en cas d'échec.
#[must_use]
pub fn set_menu(hwnd: isize, menus: &[(String, Vec<MenuEntry>)]) -> bool {
    // SAFETY: les menus créés sont possédés par la fenêtre une fois `SetMenu` réussi ; les chaînes
    // sont terminées par 0 et copiées par Windows pendant `AppendMenuW`.
    unsafe {
        let Ok(bar) = CreateMenu() else {
            return false;
        };
        for (title, entries) in menus {
            let Ok(popup) = CreatePopupMenu() else {
                return false;
            };
            for e in entries {
                let _ = match e {
                    MenuEntry::Item(id, label) => {
                        let text = wide(label);
                        AppendMenuW(popup, MF_STRING, *id as usize, PCWSTR(text.as_ptr()))
                    }
                    MenuEntry::Separator => AppendMenuW(popup, MF_SEPARATOR, 0, PCWSTR::null()),
                };
            }
            let text = wide(title);
            let _ = AppendMenuW(bar, MF_POPUP, popup.0 as usize, PCWSTR(text.as_ptr()));
        }
        // le menu précédent (changement de langue) appartient à la fenêtre jusqu'ici : on le libère
        let old = GetMenu(HWND(hwnd as *mut _));
        let ok = SetMenu(HWND(hwnd as *mut _), Some(bar)).is_ok();
        if ok {
            if !old.is_invalid() {
                let _ = DestroyMenu(old);
            }
            let _ = DrawMenuBar(HWND(hwnd as *mut _));
        }
        ok
    }
}

/// Si `msg` (le `MSG` brut reçu par le crochet de messages de la boucle d'événements) est le
/// choix d'une entrée de menu, renvoie son identifiant.
#[must_use]
pub fn menu_command(msg: *const std::ffi::c_void) -> Option<u32> {
    if msg.is_null() {
        return None;
    }
    // SAFETY: la boucle d'événements fournit un pointeur valide vers un `MSG` pendant l'appel.
    let m = unsafe { &*msg.cast::<MSG>() };
    // Menu : mot de poids fort de `wParam` = 0 et `lParam` = 0 (sinon contrôle ou accélérateur).
    (m.message == WM_COMMAND && (m.wParam.0 >> 16) & 0xffff == 0 && m.lParam.0 == 0)
        .then_some((m.wParam.0 & 0xffff) as u32)
}

extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// La langue d'affichage de Windows est le français (choix de la langue par défaut de l'interface).
#[must_use]
pub fn ui_language_is_french() -> bool {
    // SAFETY: appel Win32 sans argument ni effet de bord.
    let id = unsafe { GetUserDefaultUILanguage() };
    id & 0x03ff == 0x000c // identifiant de langue principal : LANG_FRENCH
}
