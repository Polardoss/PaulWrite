//! État Git affiché par l'éditeur (barre latérale, barre d'état). Les demandes partent vers le
//! thread de [`crate::git`] ; les réponses reviennent en [`UserEvent::Git`].

use super::*;

impl App {
    /// Le dossier dont on veut l'état Git : le dossier ouvert, sinon celui du fichier courant.
    fn git_target(&self) -> Option<PathBuf> {
        self.project_root.clone().or_else(|| {
            self.tabs[self.active]
                .editor
                .path()
                .and_then(|p| p.parent())
                .map(std::path::Path::to_path_buf)
        })
    }

    /// Demande l'état Git au thread dédié (les demandes qui s'accumulent sont fusionnées). Rien
    /// dans les modes de mesure : ils ne mesurent pas Git.
    pub(super) fn request_git(&mut self) {
        if self.is_bench() {
            return;
        }
        match self.git_target() {
            Some(dir) => {
                self.git_dir = Some(dir.clone());
                self.git_tx.send(dir).ok();
            }
            None => {
                self.git_dir = None;
                self.set_git(None);
            }
        }
    }

    /// Une réponse du thread Git : ignorée si elle ne concerne plus le dossier courant.
    pub(super) fn on_git(&mut self, result: git::GitResult) {
        if self.git_dir.as_deref() != Some(result.dir.as_path()) {
            return;
        }
        self.set_git(result.status.map(Arc::new));
    }

    /// État Git du fichier de l'onglet `i` (selon le dernier `git status` reçu).
    fn file_state(&self, i: usize) -> Option<git::FileState> {
        let path = self.tabs[i].editor.path()?;
        self.git.as_deref()?.state_of(path)
    }

    /// Demande les marqueurs de la gouttière (lignes ajoutées, modifiées, supprimées par rapport à
    /// `HEAD`) du fichier de l'onglet `i`. Un fichier non suivi est « tout ajouté » sans appeler
    /// Git ; un fichier ignoré n'a pas de marqueurs ; les autres partent vers le thread des
    /// changements. Rien dans les modes de mesure, ni pour un fichier trop gros ou modifié (ses
    /// marqueurs seront demandés à l'enregistrement).
    pub(super) fn request_diff(&mut self, i: usize) {
        if self.is_bench() {
            return;
        }
        let state = self.file_state(i);
        let tab = &mut self.tabs[i];
        let Some(path) = tab.editor.path().map(std::path::Path::to_path_buf) else {
            return;
        };
        tab.diff_state = state;
        if tab.editor.is_modified() {
            return;
        }
        let changed = if tab.editor.document().buffer().len_bytes() > git::MAX_DIFF_BYTES {
            tab.editor.set_changes(None)
        } else {
            match state {
                Some(git::FileState::Untracked) => {
                    // les vraies lignes : pas la ligne vide qui suit le dernier saut de ligne
                    let buf = tab.editor.document().buffer();
                    let mut lines = buf.len_lines();
                    if lines > 1 && buf.line(lines - 1).len_chars() == 0 {
                        lines -= 1;
                    }
                    tab.editor
                        .set_changes(Some(Arc::new(git::LineChanges::all_added(lines))))
                }
                Some(git::FileState::Ignored) => tab.editor.set_changes(None),
                _ => {
                    self.diff_tx.send(path).ok();
                    false
                }
            }
        };
        if changed && i == self.active {
            self.refresh_gutter();
        }
    }

    /// Le thread des changements a répondu : les marqueurs de chaque onglet de ce fichier.
    pub(super) fn on_diff(&mut self, result: git::DiffResult) {
        let changes = result.changes.map(Arc::new);
        let mut redraw = false;
        for i in 0..self.tabs.len() {
            if self.tabs[i].editor.path() != Some(result.file.as_path()) {
                continue;
            }
            // un fichier non suivi ou ignoré a ses marqueurs (tout ajouté, aucun) depuis `git
            // status` : une réponse plus ancienne, calculée avant, ne doit pas les écraser
            if matches!(
                self.file_state(i),
                Some(git::FileState::Untracked | git::FileState::Ignored)
            ) {
                continue;
            }
            if self.tabs[i].editor.set_changes(changes.clone()) && i == self.active {
                redraw = true;
            }
        }
        if redraw {
            self.refresh_gutter();
        }
    }

    /// Redessine la gouttière de l'onglet actif (marqueurs Git changés).
    pub(super) fn refresh_gutter(&mut self) {
        if let Some(r) = self.renderer.as_mut() {
            r.invalidate();
        }
        self.full_present = true;
        self.request_redraw();
    }

    /// Retient le nouvel état Git et ne redessine que s'il a changé.
    pub(super) fn set_git(&mut self, new: Option<Arc<GitStatus>>) {
        if self.git == new {
            return;
        }
        self.git = new;
        if let Some(tree) = self.sidebar.as_mut() {
            tree.set_git(self.git.clone());
        }
        // le fichier de l'onglet actif est devenu suivi, non suivi ou ignoré : ses marqueurs changent
        if self.file_state(self.active) != self.tabs[self.active].diff_state {
            self.request_diff(self.active);
        }
        self.refresh_git_label();
        self.tabs_dirty = true;
        self.request_redraw();
    }

    /// Texte de la barre d'état : branche, avance / retard, nombre de modifications.
    pub(super) fn refresh_git_label(&mut self) {
        self.git_label = match self.git.as_deref() {
            None => String::new(),
            Some(g) => {
                let mut s = g.branch.clone();
                if g.ahead > 0 {
                    s.push_str(&format!(" \u{2191}{}", g.ahead));
                }
                if g.behind > 0 {
                    s.push_str(&format!(" \u{2193}{}", g.behind));
                }
                let n = g.changes;
                if n > 0 {
                    s.push_str(&match (self.lang, n) {
                        (Lang::En, 1) => " \u{b7} 1 change".to_owned(),
                        (Lang::En, n) => format!(" \u{b7} {n} changes"),
                        (Lang::Fr, 1) => " \u{b7} 1 modification".to_owned(),
                        (Lang::Fr, n) => format!(" \u{b7} {n} modifications"),
                    });
                }
                s
            }
        };
    }
}
