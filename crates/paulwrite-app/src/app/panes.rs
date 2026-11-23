//! Écran partagé : deux volets côte à côte, chacun avec ses onglets.
//!
//! Le volet de gauche est dessiné par le moteur principal (`App::renderer`), qui reste celui de la
//! fenêtre entière : barre latérale, barre d'état, panneau flottant. Le volet de droite a son
//! propre moteur (`App::second`, [`SoftRenderer::new_pane`]) : le principal réserve un encart à
//! droite et y recopie son image. Les onglets restent tous dans `App::tabs` ; `Tab::pane` dit dans
//! quel volet s'affiche chacun. `App::active` est toujours l'onglet du volet qui a le focus, si bien
//! que toute la saisie (`ed!`) agit sur le bon éditeur ; `App::shown` retient l'onglet affiché dans
//! chaque volet.

use super::*;

impl App {
    /// Indices (dans `tabs`) des onglets du volet `pane`, dans l'ordre.
    pub(super) fn pane_tabs(&self, pane: usize) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.pane == pane)
            .map(|(i, _)| i)
            .collect()
    }

    /// Indice (dans `tabs`) de l'onglet affiché dans le volet `pane`.
    pub(super) fn shown_index(&self, pane: usize) -> usize {
        if pane == self.focus {
            return self.active;
        }
        self.tab_index(self.shown[pane])
            .filter(|&i| self.tabs[i].pane == pane)
            .or_else(|| self.tabs.iter().position(|t| t.pane == pane))
            .unwrap_or(self.active)
    }

    /// Abscisse (pixels de la fenêtre) du bord gauche du volet.
    pub(super) fn pane_origin(&self, pane: usize) -> f32 {
        match (pane, self.renderer.as_ref()) {
            (1, Some(r)) if self.second.is_some() => r.view_width() as f32,
            _ => 0.0,
        }
    }

    /// Volet qui occupe l'abscisse `x` (pixels de la fenêtre).
    pub(super) fn pane_at(&self, x: f32) -> usize {
        usize::from(self.second.is_some() && x >= self.pane_origin(1))
    }

    /// Rangées visibles dans le volet `pane`.
    pub(super) fn rows_in(&self, pane: usize) -> Option<usize> {
        let r = if pane == 1 {
            self.second.as_ref()
        } else {
            self.renderer.as_ref()
        }?;
        Some(r.metrics().rows_for(r.size().1))
    }

    /// Donne à l'onglet `i` le nombre de rangées visibles de son volet. L'éditeur ramène alors son
    /// curseur dans la vue : seulement si le nombre a changé (sinon la vue sautait au curseur à
    /// chaque rafraîchissement ou changement de focus, et perdait la position où l'utilisateur
    /// l'avait fait défiler, ce qui défait aussi le défilement synchronisé).
    pub(super) fn apply_rows(&mut self, i: usize, rows: usize) {
        if self.tabs[i].editor.rows() != rows.max(1) {
            self.tabs[i].editor.set_viewport_rows(rows);
        }
    }

    /// Redessin complet des deux volets à la prochaine image.
    pub(super) fn invalidate_panes(&mut self) {
        for r in [self.renderer.as_mut(), self.second.as_mut()]
            .into_iter()
            .flatten()
        {
            r.invalidate();
        }
    }

    /// Dispose les volets : largeur de l'encart du moteur principal, taille du second moteur (la
    /// moitié de la place à droite de la barre latérale, hauteur de la zone sans barre d'état).
    pub(super) fn layout_panes(&mut self) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let Some(r2) = self.second.as_mut() else {
            r.set_inset(0);
            return;
        };
        let (w, h) = r.size();
        let m = r.metrics();
        let inset = (w.saturating_sub(m.sidebar as u32) / 2).max(1);
        r.set_inset(inset as i32);
        let height = h.saturating_sub(m.status_bar as u32).max(1);
        if let Err(e) = r2.resize((inset, height)) {
            eprintln!("second volet : {e}");
        }
    }

    /// Onglet du volet `pane` le plus proche de la position `near` (le suivant d'abord, puis le
    /// précédent).
    pub(super) fn neighbor_in_pane(&self, near: usize, pane: usize) -> Option<usize> {
        let near = near.min(self.tabs.len());
        (near..self.tabs.len())
            .find(|&i| self.tabs[i].pane == pane)
            .or_else(|| (0..near).rev().find(|&i| self.tabs[i].pane == pane))
    }

    /// Remet les volets d'aplomb après une fermeture ou un déplacement d'onglet : un volet vide
    /// referme le partage, et chaque volet affiche un de ses onglets.
    pub(super) fn normalize_panes(&mut self) {
        if self.second.is_none() {
            self.tabs.iter_mut().for_each(|t| t.pane = 0);
            return;
        }
        if self.pane_tabs(0).is_empty() || self.pane_tabs(1).is_empty() {
            self.close_split();
            return;
        }
        for p in 0..2 {
            let valid = self
                .tab_index(self.shown[p])
                .is_some_and(|i| self.tabs[i].pane == p);
            if !valid {
                if let Some(i) = self.tabs.iter().position(|t| t.pane == p) {
                    self.shown[p] = self.tabs[i].id;
                }
            }
        }
    }

    /// Onglet suivant (`1`) ou précédent (`-1`) dans le volet qui a le focus.
    pub(super) fn step_tab(&mut self, delta: isize) {
        let list = self.pane_tabs(self.focus);
        if list.is_empty() {
            return;
        }
        let pos = list.iter().position(|&i| i == self.active).unwrap_or(0) as isize;
        let next = list[(pos + delta).rem_euclid(list.len() as isize) as usize];
        self.activate(next);
    }

    /// Écran partagé : ouvre le second volet, ou le referme.
    pub(super) fn toggle_split(&mut self) {
        if self.second.is_some() {
            // une vue jumelle du volet de droite fait double emploi : elle disparaît
            self.drop_redundant_twins();
            if self.second.is_some() {
                self.close_split();
                self.activate(self.active);
            }
        } else {
            self.open_split();
        }
    }

    /// Crée le moteur de rendu du second volet. `false` en cas d'échec.
    pub(super) fn ensure_second(&mut self) -> bool {
        if self.second.is_some() {
            return true;
        }
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |w| w.scale_factor() as f32);
        let mut r2 = match SoftRenderer::new_pane(scale, (400, 300), Colors::default()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("second volet : {e}");
                return false;
            }
        };
        if let Err(e) = r2.set_text_style(self.font_px as f32, self.tab_size as f32) {
            eprintln!("taille du texte : {e}");
        }
        self.second = Some(r2);
        true
    }

    /// Ouvre le second volet. L'onglet courant y passe s'il a des voisins dans le volet de gauche ;
    /// sinon un onglet vide y est créé (le volet de gauche garde son seul onglet).
    fn open_split(&mut self) {
        if self.second.is_some() || self.renderer.is_none() || !self.ensure_second() {
            return;
        }
        let target = if self.pane_tabs(0).len() >= 2 {
            self.active
        } else {
            self.push_tab()
        };
        self.tabs[target].pane = 1;
        // le volet de gauche affiche un onglet qui lui reste
        if let Some(i) = self.neighbor_in_pane(target, 0) {
            self.shown[0] = self.tabs[i].id;
        }
        self.minimap_key = [None, None];
        self.hover = Hover::default();
        self.tabs_dirty = true;
        self.layout_panes();
        self.activate(target);
    }

    /// Referme le second volet : tous les onglets reviennent dans le volet unique, dans l'ordre,
    /// l'onglet courant restant affiché. L'appelant réactive un onglet pour tout remettre à la
    /// bonne taille.
    pub(super) fn close_split(&mut self) {
        self.second = None;
        self.tabs.iter_mut().for_each(|t| t.pane = 0);
        self.focus = 0;
        self.shown = [self.tabs[self.active].id; 2];
        self.minimap_key = [None, None];
        self.hover = Hover::default();
        self.fold_hover = false;
        self.tabs_dirty = true;
        self.layout_panes();
    }

    /// Première rangée affichée dans chaque volet : `(ligne, rangée dans la ligne)`.
    fn first_positions(&self) -> ((usize, usize), (usize, usize)) {
        (
            self.tabs[self.shown_index(0)].editor.first_position(),
            self.tabs[self.shown_index(1)].editor.first_position(),
        )
    }

    /// Retient les positions actuelles des deux volets comme « synchronisées ».
    pub(super) fn mark_scroll_synced(&mut self) {
        self.synced_first = self.first_positions();
    }

    /// Défilement synchronisé : active ou coupe, et recale l'autre volet sur celui qui a le focus.
    pub(super) fn toggle_sync_scroll(&mut self) {
        self.sync_scroll = !self.sync_scroll;
        if self.sync_scroll && self.second.is_some() {
            let (line, sub) = self.tabs[self.active].editor.first_position();
            let other = self.shown_index(1 - self.focus);
            self.tabs[other].editor.set_first_position(line, sub);
        }
        self.mark_scroll_synced();
        let on = self.sync_scroll;
        let split = self.second.is_some();
        self.toast(match (self.lang, on, split) {
            (Lang::Fr, true, true) => "Défilement synchronisé : activé",
            (Lang::Fr, true, false) => "Défilement synchronisé : activé (actif en écran partagé)",
            (Lang::Fr, false, _) => "Défilement synchronisé : désactivé",
            (Lang::En, true, true) => "Synchronized scrolling: on",
            (Lang::En, true, false) => "Synchronized scrolling: on (applies in split screen)",
            (Lang::En, false, _) => "Synchronized scrolling: off",
        });
        self.request_redraw();
    }

    /// Une fois par image : si un volet a défilé, l'autre le suit (même première ligne). Quand les
    /// deux ont bougé, le volet qui a le focus l'emporte.
    pub(super) fn sync_scroll_panes(&mut self) {
        if !self.sync_scroll || self.second.is_none() {
            return;
        }
        let (f0, f1) = self.first_positions();
        if f0 == f1 {
            self.synced_first = (f0, f1);
            return;
        }
        let (l0, l1) = self.synced_first;
        let source = match (f0 != l0, f1 != l1) {
            (true, false) => 0,
            (false, true) => 1,
            // rien n'a bougé depuis la dernière fois (le plus court s'est arrêté plus tôt) :
            // rien à refaire
            (false, false) => return,
            // les deux ont bougé : le volet qui a le focus l'emporte
            (true, true) => self.focus,
        };
        let (line, sub) = if source == 0 { f0 } else { f1 };
        let target = self.shown_index(1 - source);
        self.tabs[target].editor.set_first_position(line, sub);
        self.mark_scroll_synced();
    }

    /// Clic droit sur l'onglet `index` : il devient l'onglet courant, puis un menu propose de
    /// scinder l'écran (déplacer le fichier ou le garder aussi ici), de fermer ou d'enregistrer.
    pub(super) fn tab_context_menu(&mut self, index: usize, at: (i32, i32), el: &ActiveEventLoop) {
        self.activate(index);
        let (entries, table) = menu::tab_context(self.lang, self.second.is_some());
        // le dessin du clic précédent doit être à l'écran avant que le menu ne bloque
        if let Some(r) = self.renderer.as_mut() {
            let _ = r.present(self.hwnd);
        }
        let chosen = platform::popup_menu(self.hwnd, &entries, at.0, at.1);
        if let Some(cmd) = chosen
            .and_then(|id| id.checked_sub(menu::CONTEXT_BASE))
            .and_then(|k| table.get(k as usize).copied())
        {
            self.execute(cmd, el);
            self.touch(Instant::now());
        }
    }

    /// Donne le focus à l'autre volet (sans effet hors écran partagé).
    pub(super) fn focus_other_pane(&mut self) {
        if self.second.is_some() {
            let i = self.shown_index(1 - self.focus);
            self.activate(i);
        }
    }

    /// Envoie l'onglet courant dans l'autre volet ; ouvre le partage s'il n'y en a pas. Si le volet
    /// quitté se vide, le partage se referme.
    pub(super) fn move_to_other_pane(&mut self) {
        if self.second.is_none() {
            self.open_split();
            return;
        }
        let (from, to) = (self.focus, 1 - self.focus);
        let i = self.active;
        self.tabs[i].pane = to;
        if let Some(n) = self.neighbor_in_pane(i, from) {
            self.shown[from] = self.tabs[n].id;
        }
        self.shown[to] = self.tabs[i].id;
        self.tabs_dirty = true;
        self.normalize_panes();
        self.activate(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une application sans fenêtre : un moteur de rendu de 1000×600 suffit pour les volets.
    fn app_without_window() -> (App, EventLoop<UserEvent>) {
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .with_any_thread(true)
            .build()
            .expect("boucle d'événements");
        let opts = Options::parse(std::iter::empty()).unwrap();
        let mut app = App::new(
            opts,
            crossbeam_channel::unbounded().0,
            crossbeam_channel::unbounded().0,
            crossbeam_channel::unbounded().0,
            crossbeam_channel::unbounded().0,
            event_loop.create_proxy(),
        );
        app.renderer =
            Some(SoftRenderer::new(1.0, (1000, 600), Colors::default()).expect("moteur de rendu"));
        (app, event_loop)
    }

    fn panes_of(app: &App) -> Vec<usize> {
        app.tabs.iter().map(|t| t.pane).collect()
    }

    /// Parcours complet : ouvrir, changer de focus, déplacer, fermer, refermer le partage.
    /// (Un seul test : une seule boucle d'événements par processus.)
    #[test]
    fn ecran_partage_de_bout_en_bout() {
        let (mut app, _event_loop) = app_without_window();
        for _ in 0..2 {
            app.push_tab();
        }
        app.activate(2);
        let ids: Vec<TabId> = app.tabs.iter().map(|t| t.id).collect();
        assert_eq!(panes_of(&app), [0, 0, 0]);
        assert!(app.second.is_none());

        // ouvrir : l'onglet courant passe à droite, le volet de gauche garde les autres
        app.toggle_split();
        assert!(app.second.is_some());
        assert_eq!(panes_of(&app), [0, 0, 1]);
        assert_eq!((app.focus, app.active), (1, 2));
        assert_eq!(app.shown, [ids[1], ids[2]]);
        // géométrie : moitié-moitié, le second volet commence au milieu
        assert_eq!(app.renderer.as_ref().unwrap().inset(), 500);
        assert_eq!(app.pane_origin(1), 500.0);
        assert_eq!((app.pane_at(499.0), app.pane_at(500.0)), (0, 1));

        // focus : l'autre volet, avec l'onglet qu'il affichait
        app.focus_other_pane();
        assert_eq!((app.focus, app.active), (0, 1));
        assert_eq!(app.shown_index(1), 2);
        // les onglets suivants se limitent au volet
        app.step_tab(1);
        assert_eq!(app.active, 0);
        app.step_tab(1);
        assert_eq!(app.active, 1);

        // envoyer l'onglet courant (indice 1) à droite : le volet de gauche garde l'indice 0
        app.move_to_other_pane();
        assert_eq!(panes_of(&app), [0, 1, 1]);
        assert_eq!((app.focus, app.active), (1, 1));
        assert_eq!(app.shown, [ids[0], ids[1]]);

        // fermer l'onglet 0 : son volet est vide, le partage se referme
        app.close_now(0);
        assert!(app.second.is_none());
        assert_eq!(panes_of(&app), [0, 0]);
        assert_eq!(app.focus, 0);
        assert_eq!(app.tabs[app.active].id, ids[1]);
        assert_eq!(app.renderer.as_ref().unwrap().inset(), 0);

        // refermer à la main : tout revient dans le volet unique
        app.toggle_split();
        assert_eq!(
            panes_of(&app),
            [1, 0],
            "l'onglet courant (indice 0) passe à droite"
        );
        app.toggle_split();
        assert!(app.second.is_none());
        assert_eq!(panes_of(&app), [0, 0]);

        // un seul onglet : l'ouverture crée un onglet vide à droite
        app.close_now(0);
        assert_eq!(app.tabs.len(), 1);
        app.toggle_split();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(panes_of(&app), [0, 1]);
        assert_eq!((app.focus, app.active), (1, 1));
        // fermer l'onglet de droite (le focus passe à gauche, le partage se referme)
        app.close_now(1);
        assert!(app.second.is_none());
        assert_eq!(app.tabs.len(), 1);

        // même fichier dans les deux volets (vues jumelles)
        app.scenario_vues_jumelles();

        // défilement synchronisé : deux fichiers de 300 lignes
        app.tabs[0].editor.insert_text(&"ligne\n".repeat(300), 0);
        app.toggle_split();
        assert_eq!(app.tabs.len(), 2);
        app.tabs[1].editor.insert_text(&"autre\n".repeat(300), 0);
        let (a, b) = (0usize, 1usize);
        app.tabs[a].editor.set_first_line(7);
        app.tabs[b].editor.set_first_line(2);
        // coupé : chacun garde sa position
        app.sync_scroll_panes();
        assert_eq!(
            (
                app.tabs[a].editor.first_line(),
                app.tabs[b].editor.first_line()
            ),
            (7, 2)
        );
        // activé : l'autre volet se cale sur celui qui a le focus (le volet de droite)
        assert_eq!(app.focus, 1);
        app.toggle_sync_scroll();
        assert!(app.sync_scroll);
        assert_eq!(app.tabs[a].editor.first_line(), 2);
        // le volet qui a le focus défile : l'autre le suit
        app.tabs[b].editor.scroll_by(40);
        app.sync_scroll_panes();
        assert_eq!(app.tabs[b].editor.first_line(), 42);
        assert_eq!(app.tabs[a].editor.first_line(), 42);
        // l'autre volet défile (molette sous la souris) : le premier le suit
        app.tabs[a].editor.scroll_by(-12);
        app.sync_scroll_panes();
        assert_eq!(app.tabs[a].editor.first_line(), 30);
        assert_eq!(app.tabs[b].editor.first_line(), 30);
        // changer de volet n'est pas un défilement
        app.focus_other_pane();
        app.sync_scroll_panes();
        assert_eq!(app.tabs[a].editor.first_line(), 30);
        // coupé : plus de suivi
        app.toggle_sync_scroll();
        app.tabs[a].editor.scroll_by(5);
        app.sync_scroll_panes();
        assert_eq!(app.tabs[a].editor.first_line(), 35);
        assert_eq!(app.tabs[b].editor.first_line(), 30);

        // retour à la ligne : les deux volets restent à la même *rangée*, pas seulement à la même
        // ligne (sinon l'un montre le début de la ligne et l'autre sa suite : un décalage)
        for i in [a, b] {
            let e = &mut app.tabs[i].editor;
            e.goto(1, 0);
            e.insert_text(&format!("{}\n", "mot ".repeat(200)).repeat(40), 0);
            e.set_first_line(0);
        }
        app.toggle_sync_scroll();
        assert!(app.sync_scroll);
        let focus_tab = app.active;
        app.tabs[focus_tab].editor.scroll_by(7);
        app.sync_scroll_panes();
        let (pa, pb) = (
            app.tabs[a].editor.first_position(),
            app.tabs[b].editor.first_position(),
        );
        assert!(
            pa.1 > 0,
            "le défilement s'est arrêté dans une ligne repliée : {pa:?}"
        );
        assert_eq!(pa, pb, "même ligne et même rangée dans les deux volets");
        // et l'autre sens, rangée par rangée
        let other_tab = if focus_tab == a { b } else { a };
        app.tabs[other_tab].editor.scroll_by(3);
        app.sync_scroll_panes();
        assert_eq!(
            app.tabs[a].editor.first_position(),
            app.tabs[b].editor.first_position()
        );
    }
}
