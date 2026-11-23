//! Vues jumelles : le même fichier affiché dans les deux volets.
//!
//! Chaque vue est un onglet à part entière (son éditeur, donc ses sélections et son défilement) ;
//! les onglets qui portent le même [`Tab::link`] sont tenus **identiques** : les modifications de
//! l'un sont rejouées sur les autres (`MirrorOp`, voir `paulwrite-core`). L'historique d'annulation
//! est commun : il vit dans la vue qui a le focus et change de main avec lui. L'enregistrement, le
//! nom du fichier et l'état « modifié » valent pour toutes les vues.
//!
//! Le rejeu a lieu une fois par image (début de [`App::redraw`]) et avant chaque changement de
//! focus : une frappe coûte seulement la copie du texte inséré dans le journal.

use super::*;

/// Deux éléments distincts d'un tableau, en emprunt mutable.
fn two_mut<T>(v: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    debug_assert_ne!(a, b);
    if a < b {
        let (l, r) = v.split_at_mut(b);
        (&mut l[a], &mut r[0])
    } else {
        let (l, r) = v.split_at_mut(a);
        (&mut r[0], &mut l[b])
    }
}

impl App {
    /// Indices des autres vues du même fichier que l'onglet `i`.
    pub(super) fn peers_of(&self, i: usize) -> Vec<usize> {
        let link = self.tabs[i].link;
        if link == 0 {
            return Vec::new();
        }
        (0..self.tabs.len())
            .filter(|&j| j != i && self.tabs[j].link == link)
            .collect()
    }

    /// Rejoue sur chaque vue jumelle les modifications faites dans les autres.
    pub(super) fn sync_links(&mut self) {
        let mut batches: Vec<(u64, usize, Vec<paulwrite_core::MirrorOp>)> = Vec::new();
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if t.link != 0 {
                let ops = t.editor.take_mirror();
                if !ops.is_empty() {
                    batches.push((t.link, i, ops));
                }
            }
        }
        for (link, source, ops) in batches {
            for j in 0..self.tabs.len() {
                if j != source && self.tabs[j].link == link {
                    self.tabs[j].editor.apply_mirror(&ops);
                }
            }
        }
    }

    /// L'onglet `index` va recevoir la saisie : l'historique d'annulation commun passe chez lui.
    /// Les deux textes sont d'abord remis identiques.
    pub(super) fn claim_history(&mut self, index: usize) {
        let link = self.tabs[index].link;
        if link == 0 || self.tabs[index].holds_history {
            return;
        }
        self.sync_links();
        let holder = (0..self.tabs.len())
            .find(|&j| j != index && self.tabs[j].link == link && self.tabs[j].holds_history);
        if let Some(j) = holder {
            let (a, b) = two_mut(&mut self.tabs, j, index);
            a.editor.swap_history(&mut b.editor);
            a.holds_history = false;
        }
        self.tabs[index].holds_history = true;
    }

    /// L'onglet `index` va disparaître (ou quitter le groupe) : s'il porte l'historique, une autre
    /// vue le reprend.
    pub(super) fn release_history(&mut self, index: usize) {
        if !self.tabs[index].holds_history {
            return;
        }
        self.sync_links();
        if let Some(&j) = self.peers_of(index).first() {
            let (a, b) = two_mut(&mut self.tabs, index, j);
            a.editor.swap_history(&mut b.editor);
            a.holds_history = false;
            b.holds_history = true;
        }
    }

    /// Un onglet resté seul de son groupe n'est plus une vue jumelle : plus de journal.
    pub(super) fn tidy_links(&mut self) {
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for t in &self.tabs {
            if t.link != 0 {
                *counts.entry(t.link).or_default() += 1;
            }
        }
        for t in &mut self.tabs {
            if t.link != 0 && counts[&t.link] < 2 {
                t.link = 0;
                t.holds_history = false;
                t.editor.set_mirror(false);
            }
        }
    }

    /// L'onglet `index` quitte son groupe de vues jumelles (il va recevoir un autre fichier).
    pub(super) fn unlink(&mut self, index: usize) {
        if self.tabs[index].link == 0 {
            return;
        }
        self.release_history(index);
        let t = &mut self.tabs[index];
        t.link = 0;
        t.holds_history = false;
        t.editor.set_mirror(false);
        self.tidy_links();
    }

    /// Après un enregistrement de l'onglet `index` : les autres vues du fichier sont à jour.
    pub(super) fn mark_peers_saved(&mut self, index: usize, revision: u64) {
        let stamp = self.tabs[index].disk_stamp;
        for j in self.peers_of(index) {
            self.tabs[j].editor.mark_saved(revision);
            self.tabs[j].disk_stamp = stamp;
        }
    }

    /// Affiche le même fichier dans l'autre volet : une vue jumelle de l'onglet courant (le
    /// partage s'ouvre au besoin). Le focus passe à la nouvelle vue.
    pub(super) fn clone_to_other_pane(&mut self) {
        let src = self.active;
        if self.tabs[src].loading.is_some() || self.renderer.is_none() {
            return;
        }
        // les deux textes identiques, avant de copier
        self.sync_links();
        if self.tabs[src].link == 0 {
            self.next_link += 1;
            let t = &mut self.tabs[src];
            t.link = self.next_link;
            t.holds_history = true;
            t.editor.set_mirror(true);
        }
        let link = self.tabs[src].link;
        let first_split = self.second.is_none();
        if first_split && !self.ensure_second() {
            return;
        }
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut twin = Tab::new(id);
        twin.editor = self.tabs[src].editor.fork();
        twin.language = self.tabs[src].language;
        twin.link = link;
        twin.disk_stamp = self.tabs[src].disk_stamp;
        twin.shown_modified = self.tabs[src].shown_modified;
        twin.pane = if first_split { 1 } else { 1 - self.focus };
        self.tabs.push(twin);
        let index = self.tabs.len() - 1;
        if first_split {
            self.shown[0] = self.tabs[src].id;
        }
        // sa propre coloration (le même langage), mais pas de second serveur de langage
        self.attach_syntax(index, self.tabs[src].language);
        self.minimap_key = [None, None];
        self.tabs_dirty = true;
        self.layout_panes();
        self.activate(index);
    }

    /// Scénario de test des vues jumelles (voir `panes::tests` : une seule boucle d'événements
    /// par processus, donc un seul test pour les volets et les vues jumelles).
    #[cfg(test)]
    pub(super) fn scenario_vues_jumelles(&mut self) {
        use crate::command::Command;
        let text = |app: &App, i: usize| app.tabs[i].editor.document().buffer().to_string();
        assert_eq!(self.tabs.len(), 1);
        assert!(self.second.is_none());
        self.tabs[0].editor.insert_text("hello\nworld\n", 0);

        // cloner : une seconde vue, à droite, qui a le focus et l'historique
        self.clone_to_other_pane();
        assert_eq!(self.tabs.len(), 2);
        assert_eq!(self.tabs.iter().map(|t| t.pane).collect::<Vec<_>>(), [0, 1]);
        assert_ne!(self.tabs[0].link, 0);
        assert_eq!(self.tabs[0].link, self.tabs[1].link);
        assert_eq!((self.focus, self.active), (1, 1));
        assert!(self.tabs[1].holds_history && !self.tabs[0].holds_history);
        assert_eq!(text(self, 0), text(self, 1));
        assert!(self.tabs[0].editor.is_modified() && self.tabs[1].editor.is_modified());

        // une frappe dans la vue de droite arrive dans celle de gauche
        self.tabs[1].editor.insert_text("X", 100);
        self.sync_links();
        assert!(text(self, 0).contains('X'));
        assert_eq!(text(self, 0), text(self, 1));
        assert_eq!(
            self.tabs[0].editor.revision(),
            self.tabs[1].editor.revision()
        );

        // le focus passe à gauche : annuler y annule la frappe faite à droite, et la droite suit
        self.focus_other_pane();
        assert_eq!(self.active, 0);
        assert!(self.tabs[0].holds_history && !self.tabs[1].holds_history);
        self.tabs[0].editor.execute(Command::Undo, 0);
        self.sync_links();
        assert!(
            !text(self, 1).contains('X'),
            "l'annulation arrive aussi à droite"
        );
        assert_eq!(text(self, 0), text(self, 1));

        // enregistrer vaut pour les deux vues
        let rev = self.tabs[0].editor.revision();
        self.tabs[0].editor.mark_saved(rev);
        self.mark_peers_saved(0, rev);
        assert!(!self.tabs[0].editor.is_modified() && !self.tabs[1].editor.is_modified());

        // refermer le partage : la vue jumelle de droite fait double emploi et disparaît
        self.toggle_split();
        assert!(self.second.is_none());
        assert_eq!(self.tabs.len(), 1);
        assert_eq!(self.tabs[0].link, 0, "seule, elle n'est plus une jumelle");
        assert_eq!(self.focus, 0);

        // fermer une vue ne perd ni le texte ni l'historique : l'autre vue les reprend
        self.clone_to_other_pane();
        self.tabs[1].editor.insert_text("Y", 200);
        self.close_tab(1);
        assert_eq!(self.tabs.len(), 1);
        assert!(self.second.is_none());
        assert!(text(self, 0).contains('Y'));
        assert!(
            self.tabs[0].editor.is_modified(),
            "aucune question : rien n'est perdu"
        );
        assert!(self.tabs[0].editor.document().history().undo_depth() >= 1);
        self.tabs[0].editor.execute(Command::Undo, 0);
        assert!(!text(self, 0).contains('Y'));
    }

    /// Referme le partage en supprimant d'abord les vues jumelles du volet de droite qui font
    /// double emploi avec une vue du volet de gauche.
    pub(super) fn drop_redundant_twins(&mut self) {
        loop {
            let twin = (0..self.tabs.len()).find_map(|i| {
                let left = self
                    .peers_of(i)
                    .into_iter()
                    .find(|&j| self.tabs[j].pane == 0)?;
                (self.tabs[i].pane == 1).then_some((i, left))
            });
            let Some((i, left)) = twin else {
                return;
            };
            if i == self.active {
                self.activate(left);
            }
            self.close_now(i);
        }
    }
}
