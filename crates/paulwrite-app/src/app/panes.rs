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
            self.close_split();
            self.activate(self.active);
        } else {
            self.open_split();
        }
    }

    /// Ouvre le second volet. L'onglet courant y passe s'il a des voisins dans le volet de gauche ;
    /// sinon un onglet vide y est créé (le volet de gauche garde son seul onglet).
    fn open_split(&mut self) {
        if self.second.is_some() || self.renderer.is_none() {
            return;
        }
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |w| w.scale_factor() as f32);
        let mut r2 = match SoftRenderer::new_pane(scale, (400, 300), Colors::default()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("second volet : {e}");
                return;
            }
        };
        if let Err(e) = r2.set_text_style(self.font_px as f32, self.tab_size as f32) {
            eprintln!("taille du texte : {e}");
        }
        self.second = Some(r2);
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
    }
}
