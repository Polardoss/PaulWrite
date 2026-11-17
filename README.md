# PaulWrite

Éditeur de texte pour Windows écrit en Rust, conçu pour être **aussi rapide que Sublime Text**.
La vitesse est le critère n°1, avant les fonctionnalités. Pas d'Electron, pas de webview, pas de
framework UI lourd.

> État : en construction. Étapes 0 à 4 en place : l'éditeur ouvre, édite et enregistre des fichiers
> dans des onglets, avec coloration syntaxique (Rust, JSON, TOML, Python, JavaScript, HTML, CSS),
> Goto Anything, palette de commandes et recherche dans le projet.

## Objectifs de performance

Chaque objectif est **mesuré**, jamais supposé. Une régression de performance bloque l'étape.

| Objectif | Cible |
|---|---|
| Démarrage à froid jusqu'à la première fenêtre | < 100 ms |
| Latence de frappe (touche → affichage) | < 10 ms (p99 < 16 ms) |
| Fichier de 500 Mo | ouverture sans geler l'UI, défilement fluide |
| Mémoire au repos (hors fichiers ouverts) | quelques dizaines de Mo |
| Recherche dans un projet de 100 000 fichiers | premiers résultats < 200 ms |
| Fuzzy finder (Goto Anything) | < 30 ms par frappe |

Les mesures réelles sont consignées étape par étape dans [`docs/benchmarks.md`](docs/benchmarks.md).

## Règles de conception

1. Le thread UI ne fait jamais d'I/O, de parsing ni de calcul lourd (canaux vers des workers).
2. Rendu à la demande, invalidation par régions, seules les lignes visibles sont rendues.
3. Chaque fonctionnalité est une commande nommée ; raccourcis, palette et plugins appellent les mêmes.
4. Chaque étape a son benchmark.
5. Gros fichiers : lecture en flux ou mmap, coloration désactivée/partielle au-delà d'un seuil.
6. Aucune allocation inutile dans le chemin de saisie et de rendu.
7. Aucune dépendance sans justification (poids, temps de compilation, impact au démarrage).

## Architecture

Workspace Cargo (`crates/`) :

| Crate | Rôle |
|---|---|
| `paulwrite-core` | Buffer ([ropey]), sélections et curseurs multiples, undo/redo transactionnel, encodages, I/O. Aucune dépendance UI. |
| `paulwrite-syntax` | Coloration incrémentale tree-sitter sur thread dédié (7 langages). |
| `paulwrite-search` | Liste de fichiers, recherche de texte (moteur de ripgrep) et fuzzy finder (nucleo), respectent `.gitignore`. |
| `paulwrite-render` | Rendu : Direct2D logiciel + DirectWrite par régions sales (texte, gouttière, onglets, minimap, barre d'état, barre latérale, panneau), presse-papiers, boîtes de dialogue, barre de menus. Choix mesuré : [`docs/rendering.md`](docs/rendering.md) ; apparence : [`docs/interface.md`](docs/interface.md). |
| `paulwrite-plugins` | Plugins Lua sur thread dédié : commandes et événements, instantané O(1) du texte, limites de temps et de mémoire. Voir [`docs/plugins.md`](docs/plugins.md). |
| `paulwrite-lsp` | Client LSP (JSON-RPC sur stdio, un thread par serveur) : diagnostics, complétion, définition, survol. Désactivé par défaut. Voir [`docs/lsp.md`](docs/lsp.md). |
| `paulwrite-app` | Fenêtre winit, commandes nommées, raccourcis, éditeur, worker de fichiers. Produit le binaire `paulwrite`. |

[ropey]: https://crates.io/crates/ropey

## Compiler

Prérequis : Windows 10/11, [Rust](https://rustup.rs) (toolchain `stable-x86_64-pc-windows-msvc`) et les
Visual Studio Build Tools avec la charge de travail « Développement Desktop en C++ ».

```bash
cargo build --release
```

Le binaire se trouve dans `target/release/paulwrite.exe`. Pour profiler (symboles de lignes) :

```bash
cargo build --profile profiling
```

## Utiliser

```bash
paulwrite [fichier]        # ou glisser un fichier sur la fenêtre
```

| Raccourci | Commande |
|---|---|
| Flèches, Ctrl+flèches (mots), Home/End, Ctrl+Home/End, PgUp/PgDn | déplacements ; avec Maj : sélection |
| Ctrl+Z, Ctrl+Y (ou Ctrl+Maj+Z) | annuler, rétablir |
| Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V | tout sélectionner, copier, couper, coller |
| Ctrl+S, Ctrl+Maj+S, Ctrl+O | enregistrer, enregistrer sous, ouvrir (dans un nouvel onglet) |
| Ctrl+N, Ctrl+W | nouvel onglet, fermer l'onglet |
| Ctrl+Tab, Ctrl+Maj+Tab, clic, clic milieu | onglet suivant, précédent, activer, fermer |
| Ctrl+P | Goto Anything : ouvrir un fichier du projet (recherche floue) |
| Ctrl+Maj+P | palette de commandes (toutes les commandes, avec leur raccourci) |
| Ctrl+G | aller à la ligne |
| Ctrl+Maj+F | rechercher du texte dans le projet |
| Ctrl+Espace, F12, F1, Ctrl+Maj+M | serveur de langage (si activé) : complétion, définition, survol, diagnostics |
| Ctrl+Alt+↑ / Ctrl+Alt+↓ | ajouter un curseur au-dessus / en dessous |
| Échap | revenir à un seul curseur |
| Ctrl+↑ / Ctrl+↓, molette | faire défiler sans bouger le curseur |
| Clic, glisser, Maj+clic | placer le curseur, sélectionner |
| Clic dans la gouttière | sélectionner la ligne |
| Clic ou glisser dans la minimap, glisser la barre de défilement | faire défiler |
| Ctrl+K puis Ctrl+B, icône en bas à gauche | afficher / masquer la barre latérale |
| Alt+Z (View ▸ Word Wrap) | retour à la ligne automatique (actif par défaut) |
| Preferences ▸ Language | anglais ou français (enregistré dans `config.toml`) |
| Ctrl+Maj+[ / Ctrl+Maj+], flèches de la gouttière, clic sur « … » | replier / déplier une région (par indentation) ; Edit ▸ Fold All / Unfold All |
| Maj+molette, molette latérale | défilement horizontal (sans retour à la ligne) |

Dans le panneau (Goto Anything, palette, recherche) : saisir pour filtrer, ↑ ↓ PgUp PgDn pour
choisir, Entrée ou clic pour valider, Échap ou clic à côté pour fermer. Le **projet** est le dossier
passé en argument (`paulwrite C:\chemin\projet`) ou glissé sur la fenêtre ; sinon le dossier du
fichier courant. `.gitignore` est respecté.

Les plugins Lua (`%APPDATA%\PaulWrite\plugins`) ajoutent leurs commandes à la palette : voir
[`docs/plugins.md`](docs/plugins.md).

La barre de menus (File, Edit, Selection, Find, View, Goto, Tools, Project, Preferences, Help ; en français Fichier, Édition, Sélection, Rechercher, Affichage, Aller à, Outils, Projet, Préférences, Aide)
donne accès aux mêmes commandes, dont **File ▸ Open Folder…** : le dossier devient le projet et sa
barre latérale s'affiche (un dossier passé en argument ou glissé sur la fenêtre fait de même).

Plusieurs fichiers peuvent être passés en argument (`paulwrite a.rs b.json`) ou glissés sur la
fenêtre : un onglet chacun. Un fichier déjà ouvert est simplement activé.

Réglages (facultatifs) dans `%APPDATA%\PaulWrite\config.toml` :

```toml
highlight = true                # false : pas de coloration syntaxique
highlight_max_bytes = 1048576   # au-delà de cette taille (octets), pas de coloration
language = "auto"               # "en", "fr" ou "auto" (langue de Windows)
word_wrap = true                # false : pas de retour à la ligne automatique
word_wrap_max_bytes = 16777216  # au-delà de cette taille (octets), pas de retour à la ligne
lsp = false                     # true : serveurs de langage (programmes externes à installer)
lsp_rust = "rust-analyzer"      # commande d'un langage (voir docs/lsp.md)
```

Chaque ligne correspond à une commande nommée (`cursor.left`, `edit.undo`, `file.save`, …) définie
dans `crates/paulwrite-app/src/command.rs`.

## Tests et qualité

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Contrôle visuel du rendu, sans rien afficher à l'écran (la fenêtre est hors de la zone visible) :

```bash
paulwrite --screenshot capture.png dossier fichier.rs   # écrit une capture PNG puis quitte
cargo run -p paulwrite-render --example snapshot -- demo.png   # démo du moteur seul
```

## Benchmarks

Les benchmarks utilisent [criterion]. Les fichiers de test volumineux (jusqu'à 500 Mo) sont générés
de façon déterministe dans `bench-data/` à la première exécution (ignoré par git, prévoir ~1 Go libre).

```bash
cargo bench -p paulwrite-core                 # étape 1 : buffer, historique, curseurs
cargo build --release -p paulwrite-app
.\scripts\bench-app.ps1                       # étapes 2-3 : démarrage, frappe, défilement, coloration
cargo run --release -p paulwrite-search --example gen_project -- bench-data\project-100k
cargo run --release -p paulwrite-search --example bench -- bench-data\project-100k   # étape 3 : recherche
```

[criterion]: https://crates.io/crates/criterion

## Licence

[MIT](LICENSE)
