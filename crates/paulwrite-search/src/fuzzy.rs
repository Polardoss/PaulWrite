//! Fuzzy finder (« Goto Anything ») : classement de chemins par nucleo.
//!
//! nucleo fait le travail sur ses propres threads : [`FuzzyFinder::set_query`] ne bloque pas, et
//! [`FuzzyFinder::tick`] récupère les résultats quand ils sont prêts. Le thread UI peut donc
//! appeler `tick` à chaque image sans jamais attendre un classement.

use std::sync::Arc;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Injector, Matcher, Nucleo, Status, Utf32Str};

/// Poignée pour ajouter des candidats depuis n'importe quel thread (voir [`FuzzyFinder::injector`]).
pub type FuzzyInjector = Injector<String>;

/// Classe une liste de chemins selon une requête floue.
pub struct FuzzyFinder {
    nucleo: Nucleo<String>,
    query: String,
    /// Sert à retrouver les caractères qui correspondent ([`FuzzyFinder::indices`]).
    matcher: Matcher,
}

impl std::fmt::Debug for FuzzyFinder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuzzyFinder")
            .field("query", &self.query)
            .field("items", &self.item_count())
            .finish_non_exhaustive()
    }
}

impl FuzzyFinder {
    /// Crée le classeur (un thread de calcul par cœur). `notify` est appelé, depuis un thread de
    /// nucleo, quand de nouveaux résultats sont disponibles : il doit réveiller la boucle UI (qui
    /// appellera ensuite [`tick`](Self::tick)).
    #[must_use]
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            nucleo: Nucleo::new(Config::DEFAULT.match_paths(), Arc::new(notify), None, 1),
            query: String::new(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        }
    }

    /// Poignée pour ajouter des chemins ([`add`](Self::add)) depuis un autre thread (parcours du
    /// projet).
    #[must_use]
    pub fn injector(&self) -> FuzzyInjector {
        self.nucleo.injector()
    }

    /// Ajoute un chemin candidat. Les antislashs (Windows) sont remplacés par des `/` : on tape `/`
    /// au clavier, et les résultats sont renvoyés avec des `/` (valides aussi sous Windows).
    pub fn add(injector: &FuzzyInjector, path: String) {
        let path = if path.contains('\\') {
            path.replace('\\', "/")
        } else {
            path
        };
        injector.push(path, |s, columns| columns[0] = s.as_str().into());
    }

    /// Change la requête. Le classement se fait en arrière-plan.
    pub fn set_query(&mut self, query: &str) {
        // Si la requête ne fait que s'allonger, nucleo réutilise les résultats précédents.
        let append = !self.query.is_empty() && query.starts_with(&self.query);
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        self.query.clear();
        self.query.push_str(query);
    }

    /// Récupère l'état du classement (attend au plus `timeout_ms` millisecondes que les threads
    /// de nucleo publient un résultat). `running` reste vrai tant que le classement continue.
    pub fn tick(&mut self, timeout_ms: u64) -> Status {
        self.nucleo.tick(timeout_ms)
    }

    /// Nombre de candidats connus.
    #[must_use]
    pub fn item_count(&self) -> u32 {
        self.nucleo.snapshot().item_count()
    }

    /// Nombre de candidats qui correspondent à la requête.
    #[must_use]
    pub fn matched_count(&self) -> u32 {
        self.nucleo.snapshot().matched_item_count()
    }

    /// Indices (en caractères) des caractères de `candidate` qui correspondent à la requête
    /// courante, triés : sert à les mettre en évidence dans la liste. Vide si rien ne correspond.
    pub fn indices(&mut self, candidate: &str) -> Vec<u32> {
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut out = Vec::new();
        pattern.indices(
            Utf32Str::new(candidate, &mut buf),
            &mut self.matcher,
            &mut out,
        );
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Les `limit` meilleurs résultats, du meilleur au moins bon.
    #[must_use]
    pub fn top(&self, limit: usize) -> Vec<String> {
        let snap = self.nucleo.snapshot();
        let n = snap.matched_item_count().min(limit as u32);
        snap.matched_items(..n).map(|i| i.data.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn finder_with(paths: &[&str]) -> FuzzyFinder {
        let f = FuzzyFinder::new(|| {});
        let inj = f.injector();
        for p in paths {
            FuzzyFinder::add(&inj, (*p).to_owned());
        }
        f
    }

    /// Attend que le classement soit terminé.
    fn settle(f: &mut FuzzyFinder) {
        for _ in 0..1000 {
            if !f.tick(10).running {
                return;
            }
        }
        panic!("le classement ne se termine pas");
    }

    #[test]
    fn classement_flou() {
        let mut f = finder_with(&[
            "src/main.rs",
            "src/editor.rs",
            "docs/readme.md",
            "crates/paulwrite-core/src/main_loop.rs",
            "Cargo.toml",
        ]);
        settle(&mut f);
        assert_eq!(f.item_count(), 5);

        f.set_query("mainrs");
        settle(&mut f);
        let top = f.top(10);
        assert_eq!(top.first().map(String::as_str), Some("src/main.rs"));
        assert!(!top.contains(&"Cargo.toml".to_owned()));

        // requête qui s'allonge, puis qui change complètement
        f.set_query("edit");
        settle(&mut f);
        assert_eq!(f.top(1), ["src/editor.rs"]);
        f.set_query("xyz");
        settle(&mut f);
        assert_eq!(f.matched_count(), 0);
        // requête vide : tout correspond
        f.set_query("");
        settle(&mut f);
        assert_eq!(f.matched_count(), 5);
    }

    #[test]
    fn indices_des_caracteres_correspondants() {
        let mut f = finder_with(&["src/main.rs"]);
        settle(&mut f);
        f.set_query("mrs");
        settle(&mut f);
        // m (4), r (9), s (10) dans « src/main.rs » : positions en caractères
        let idx = f.indices("src/main.rs");
        assert_eq!(idx.len(), 3);
        assert!(idx.windows(2).all(|w| w[0] < w[1]));
        let chars: Vec<char> = "src/main.rs".chars().collect();
        let picked: String = idx.iter().map(|&i| chars[i as usize]).collect();
        assert_eq!(picked, "mrs");
        assert!(f.indices("zzz").is_empty());
        f.set_query("");
        assert!(f.indices("src/main.rs").is_empty());
    }

    #[test]
    fn les_antislashs_windows_sont_normalises() {
        let mut f = finder_with(&["src\\sub\\main.rs", "docs\\a.md"]);
        settle(&mut f);
        f.set_query("src/sub/main");
        settle(&mut f);
        assert_eq!(f.top(5), ["src/sub/main.rs"]);
    }

    #[test]
    fn la_notification_reveille_l_appelant() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let mut f = FuzzyFinder::new(move || {
            h.fetch_add(1, Ordering::Relaxed);
        });
        let inj = f.injector();
        FuzzyFinder::add(&inj, "a/b.rs".into());
        settle(&mut f);
        assert!(hits.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn casse_intelligente() {
        let mut f = finder_with(&["src/Main.rs", "src/main_old.rs"]);
        settle(&mut f);
        f.set_query("Main");
        settle(&mut f);
        // une majuscule dans la requête : correspondance sensible à la casse
        assert_eq!(f.top(5), ["src/Main.rs"]);
    }
}
