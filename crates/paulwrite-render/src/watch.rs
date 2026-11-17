//! Surveillance d'un dossier (`ReadDirectoryChangesW`) : prévient quand un fichier ou un dossier
//! est créé, supprimé ou renommé, n'importe où dans l'arborescence. Un thread dédié attend les
//! changements ; la fermeture du dossier surveillé interrompt l'attente.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::CancelIoEx;

/// Taille du tampon des notifications, en octets (un dépassement est signalé comme un
/// changement de la racine).
const BUFFER_BYTES: usize = 64 * 1024;

/// Surveillance en cours ; elle s'arrête quand l'objet est détruit.
#[derive(Debug)]
pub struct DirWatcher {
    handle: isize,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Surveille `root` et tout ce qu'il contient. `on_change` est appelé **depuis le thread de
/// surveillance** avec le dossier qui contient l'élément créé, supprimé ou renommé. `None` si le
/// dossier ne peut pas être ouvert.
pub fn watch_dir(root: &Path, on_change: impl Fn(PathBuf) + Send + 'static) -> Option<DirWatcher> {
    let wide: Vec<u16> = root
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` est terminé par 0 et vit pendant l'appel ; le handle obtenu est fermé par
    // `Drop` (ou ici en cas d'échec).
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .ok()?;
    let raw = handle.0 as isize;
    let stop = Arc::new(AtomicBool::new(false));
    let root = root.to_path_buf();
    let thread = {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("paulwrite-watch".into())
            .spawn(move || run(raw, &root, &stop, &on_change))
            .ok()
    };
    let Some(thread) = thread else {
        // SAFETY: le handle vient d'être ouvert et n'est utilisé nulle part ailleurs.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    };
    Some(DirWatcher {
        handle: raw,
        stop,
        thread: Some(thread),
    })
}

/// Boucle du thread : attend les changements et les transmet.
fn run(raw: isize, root: &Path, stop: &AtomicBool, on_change: &dyn Fn(PathBuf)) {
    let handle = HANDLE(raw as *mut _);
    // tampon aligné sur 4 octets, comme l'exige FILE_NOTIFY_INFORMATION
    let mut buf = vec![0u32; BUFFER_BYTES / 4];
    while !stop.load(Ordering::Acquire) {
        let mut bytes = 0u32;
        // SAFETY: `buf` est valide pendant l'appel (synchrone) ; `handle` est ouvert jusqu'à ce
        // que `Drop` ait attendu la fin de ce thread.
        let result = unsafe {
            ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                Some(&mut bytes),
                None,
                None,
            )
        };
        if stop.load(Ordering::Acquire) || result.is_err() {
            break;
        }
        if bytes == 0 {
            // tampon dépassé : on ne sait plus ce qui a changé
            on_change(root.to_path_buf());
            continue;
        }
        let bytes = &mut buf.as_mut_slice();
        let base = bytes.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            // SAFETY: le noyau a écrit des enregistrements FILE_NOTIFY_INFORMATION consécutifs :
            // NextEntryOffset (u32), Action (u32), FileNameLength (octets, u32), puis le nom en
            // UTF-16. Les décalages restent dans le tampon (vérifiés avant chaque lecture).
            let (next, name_len) = unsafe {
                let rec = base.add(offset);
                (
                    rec.cast::<u32>().read() as usize,
                    rec.add(8).cast::<u32>().read() as usize,
                )
            };
            if offset + 12 + name_len > BUFFER_BYTES {
                on_change(root.to_path_buf());
                break;
            }
            let name: Vec<u16> = (0..name_len / 2)
                .map(|i| {
                    // SAFETY: borné par la vérification ci-dessus.
                    unsafe { base.add(offset + 12 + i * 2).cast::<u16>().read_unaligned() }
                })
                .collect();
            let changed = root.join(String::from_utf16_lossy(&name));
            on_change(
                changed
                    .parent()
                    .map_or_else(|| root.to_path_buf(), Path::to_path_buf),
            );
            if next == 0 || offset + next + 12 > BUFFER_BYTES {
                break;
            }
            offset += next;
        }
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let handle = HANDLE(self.handle as *mut _);
        if let Some(thread) = self.thread.take() {
            // Le thread peut ne pas encore attendre (il traite un lot) : on annule l'attente en
            // boucle jusqu'à ce qu'il ait fini.
            while !thread.is_finished() {
                // SAFETY: le handle est encore ouvert (fermé plus bas, après la fin du thread).
                unsafe {
                    let _ = CancelIoEx(handle, None);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let _ = thread.join();
        }
        // SAFETY: plus aucun thread n'utilise le handle.
        unsafe {
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paulwrite-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Attend qu'un changement concerne `dir`.
    fn wait_for(rx: &mpsc::Receiver<PathBuf>, dir: &Path) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if let Ok(p) = rx.recv_timeout(Duration::from_millis(200)) {
                if p == dir {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn creations_et_suppressions_sont_signalees_dans_tout_l_arbre() {
        let root = temp_dir("arbre");
        std::fs::create_dir(root.join("sous")).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let watcher = watch_dir(&root, move |d| {
            let _ = tx.lock().unwrap().send(d);
        })
        .expect("dossier surveillé");
        // le thread doit être en attente (un changement antérieur à la première attente est manqué)
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(root.join("a.txt"), "x").unwrap();
        assert!(wait_for(&rx, &root), "création à la racine");
        std::fs::write(root.join("sous").join("b.txt"), "x").unwrap();
        assert!(
            wait_for(&rx, &root.join("sous")),
            "création dans un sous-dossier"
        );
        std::fs::remove_file(root.join("a.txt")).unwrap();
        assert!(wait_for(&rx, &root), "suppression");
        // l'arrêt est rapide même sans aucun changement en attente
        let t = Instant::now();
        drop(watcher);
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "arrêt en {:?}",
            t.elapsed()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dossier_inexistant() {
        assert!(watch_dir(Path::new("C:\\ce\\dossier\\n\\existe\\pas"), |_| {}).is_none());
    }
}
