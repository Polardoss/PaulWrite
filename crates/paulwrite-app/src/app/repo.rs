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

    /// Retient le nouvel état Git et ne redessine que s'il a changé.
    pub(super) fn set_git(&mut self, new: Option<Arc<GitStatus>>) {
        if self.git == new {
            return;
        }
        self.git = new;
        if let Some(tree) = self.sidebar.as_mut() {
            tree.set_git(self.git.clone());
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
