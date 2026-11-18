//! Remplacer dans tout le projet (Ctrl+Maj+H) : le panneau à deux champs (chercher, remplacer par),
//! puis un aperçu compté par le worker, une confirmation, et le remplacement lui-même. Le moteur est
//! dans [`crate::project_replace`] ; tout ce qui touche au disque se passe sur le worker.

use super::*;

impl App {
    /// Le panneau « remplacer dans le projet » est ouvert.
    pub(super) fn project_panel_open(&self) -> bool {
        self.palette
            .as_ref()
            .is_some_and(|s| s.palette.mode() == PaletteMode::ProjectReplace)
    }

    /// Prépare le panneau qui vient de s'ouvrir : saisie préremplie (texte sélectionné sur une
    /// ligne, sinon la dernière recherche) et second champ.
    pub(super) fn project_prepare(&mut self, st: &mut PaletteState) {
        let sel = ed!(self).selection_string(200);
        let prefill = if !sel.is_empty() && !sel.contains(['\n', '\r']) {
            sel
        } else {
            self.last_find.clone()
        };
        if !prefill.is_empty() {
            st.palette.handle(&PaletteKey::Text(prefill));
        }
        st.replace = Some(self.new_palette(PaletteMode::ReplaceWith));
        st.focus_replace = false;
    }

    /// Message sous la saisie : ce que fait Entrée et les options actives.
    pub(super) fn project_refresh_note(&mut self) {
        let lang = self.lang;
        let mut note = lang
            .pick(
                "Enter: next field, then replace in all files",
                "Entrée : champ suivant, puis remplacer partout",
            )
            .to_owned();
        let o = self.find_opts;
        for (on, label) in [
            (o.case_sensitive, lang.pick("[case]", "[casse]")),
            (o.whole_word, lang.pick("[word]", "[mot]")),
            (o.regex, "[regex]"),
        ] {
            if on {
                note.push_str("  ");
                note.push_str(label);
            }
        }
        if let Some(st) = self.palette.as_mut() {
            st.palette.set_note(note);
        }
        self.overlay_dirty = true;
    }

    /// Touche reçue par le panneau.
    pub(super) fn project_key(&mut self, key: &PaletteKey) {
        let Some(st) = self.palette.as_mut() else {
            return;
        };
        match key {
            PaletteKey::Tab => {
                st.focus_replace = !st.focus_replace;
            }
            PaletteKey::Escape => self.close_palette(),
            // Entrée dans le premier champ passe au second ; dans le second, lance l'aperçu
            PaletteKey::Enter if !st.focus_replace => st.focus_replace = true,
            PaletteKey::Enter => self.project_start(),
            other => {
                let target = match (st.focus_replace, st.replace.as_mut()) {
                    (true, Some(r)) => r,
                    _ => &mut st.palette,
                };
                if target.handle(other) == Outcome::Redraw {
                    self.overlay_dirty = true;
                }
            }
        }
        self.overlay_dirty = true;
    }

    /// Entrée dans le second champ : demande au worker de compter ce qui serait remplacé.
    fn project_start(&mut self) {
        let lang = self.lang;
        let Some(root) = self.project_root.clone() else {
            self.toast(lang.pick(
                "Open a folder first (File > Open Folder)",
                "Ouvrez d'abord un dossier (Fichier > Ouvrir un dossier)",
            ));
            return;
        };
        let Some(st) = self.palette.as_ref() else {
            return;
        };
        let query = st.palette.query().to_owned();
        let replacement = st
            .replace
            .as_ref()
            .map(|r| r.query().to_owned())
            .unwrap_or_default();
        if query.is_empty() {
            self.toast(lang.pick("Nothing to find yet", "Rien à chercher pour l'instant"));
            return;
        }
        self.last_find.clone_from(&query);
        // les fichiers ouverts avec des modifications non enregistrées ne sont pas touchés
        let skip = self
            .tabs
            .iter()
            .filter(|t| t.editor.is_modified())
            .filter_map(|t| t.editor.path().map(std::path::Path::to_path_buf))
            .collect();
        let params = project_replace::Params {
            root,
            query,
            replacement,
            opts: self.find_opts,
            skip,
        };
        self.close_palette();
        self.toast(lang.pick("Counting\u{2026}", "Comptage\u{2026}"));
        self.project_params = Some(params.clone());
        self.jobs
            .send(Job::ProjectReplace {
                params: Box::new(params),
                apply: false,
            })
            .ok();
    }

    /// Le worker a fini un aperçu ou un remplacement.
    pub(super) fn on_project_report(&mut self, report: project_replace::Report) {
        let lang = self.lang;
        if let Some(e) = &report.error {
            self.project_params = None;
            self.show_error(lang.pick(
                format!("Invalid pattern: {e}"),
                format!("Motif invalide : {e}"),
            ));
            return;
        }
        let (n, f, s) = (report.occurrences, report.files, report.skipped);
        let skipped = match (lang, s) {
            (_, 0) => String::new(),
            (Lang::En, s) => format!("\n\n{s} open file(s) with unsaved changes will be skipped."),
            (Lang::Fr, s) => format!(
                "\n\n{s} fichier(s) ouvert(s) avec des modifications non enregistrées seront ignorés."
            ),
        };
        if !report.applied {
            if n == 0 {
                self.project_params = None;
                self.toast(match (lang, s) {
                    (Lang::En, 0) => "No matches in the project".to_owned(),
                    (Lang::Fr, 0) => "Aucune occurrence dans le projet".to_owned(),
                    (Lang::En, s) => format!("No matches to replace ({s} open file(s) skipped)"),
                    (Lang::Fr, s) => {
                        format!(
                            "Aucune occurrence à remplacer ({s} fichier(s) ouvert(s) ignoré(s))"
                        )
                    }
                });
                return;
            }
            let text = lang.pick(
                format!(
                    "Replace {n} occurrences in {f} files?\n\nThis writes to the files on disk and cannot be undone with Ctrl+Z.{skipped}"
                ),
                format!(
                    "Remplacer {n} occurrences dans {f} fichiers ?\n\nLes fichiers sont modifiés sur le disque et Ctrl+Z ne pourra pas l'annuler.{skipped}"
                ),
            );
            self.jobs
                .send(Job::ConfirmProject {
                    owner: self.hwnd,
                    text,
                })
                .ok();
            return;
        }
        self.project_params = None;
        self.toast(match lang {
            Lang::En => format!("{n} occurrences replaced in {f} files"),
            Lang::Fr => format!("{n} occurrences remplacées dans {f} fichiers"),
        });
        if !report.errors.is_empty() {
            let list = report
                .errors
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            self.show_error(lang.pick(
                format!("Some files could not be changed:\n{list}"),
                format!("Certains fichiers n'ont pas pu être modifiés :\n{list}"),
            ));
        }
        // les fichiers ouverts (sans modification de votre part) sont relus tout de suite
        self.check_files();
    }

    /// Réponse à « Remplacer N occurrences dans M fichiers ? ».
    pub(super) fn on_project_confirmed(&mut self, yes: bool) {
        let Some(params) = self.project_params.take() else {
            return;
        };
        if !yes {
            return;
        }
        self.toast(self.lang.pick("Replacing\u{2026}", "Remplacement\u{2026}"));
        self.jobs
            .send(Job::ProjectReplace {
                params: Box::new(params),
                apply: true,
            })
            .ok();
    }
}
