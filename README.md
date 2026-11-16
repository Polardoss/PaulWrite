# PaulWrite

Éditeur de texte pour Windows écrit en Rust, conçu pour être **aussi rapide que Sublime Text**.
La vitesse est le critère n°1, avant les fonctionnalités. Pas d'Electron, pas de webview, pas de
framework UI lourd.

> État : en construction. Étapes 0 (fondations), 1 (`paulwrite-core`) et 2 (fenêtre et rendu) en
> place : l'éditeur ouvre, édite et enregistre un fichier texte (un seul document, sans coloration
> syntaxique ni onglets pour l'instant).

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
| `paulwrite-syntax` | Coloration incrémentale tree-sitter sur thread dédié. *(étape 3)* |
| `paulwrite-search` | Recherche fichier/projet, fuzzy finder. *(étapes 3-4)* |
| `paulwrite-render` | Rendu du texte : Direct2D logiciel + DirectWrite par régions sales, presse-papiers, boîtes de dialogue. Choix mesuré : [`docs/rendering.md`](docs/rendering.md). |
| `paulwrite-app` | Fenêtre winit, commandes nommées, raccourcis, éditeur, worker de fichiers. Produit le binaire `paulwrite`. |

Plus tard : `paulwrite-lsp`, `paulwrite-plugins`.

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
| Ctrl+S, Ctrl+Maj+S, Ctrl+O | enregistrer, enregistrer sous, ouvrir |
| Ctrl+Alt+↑ / Ctrl+Alt+↓ | ajouter un curseur au-dessus / en dessous |
| Échap | revenir à un seul curseur |
| Ctrl+↑ / Ctrl+↓, molette | faire défiler sans bouger le curseur |
| Clic, glisser, Maj+clic | placer le curseur, sélectionner |

Chaque ligne correspond à une commande nommée (`cursor.left`, `edit.undo`, `file.save`, …) définie
dans `crates/paulwrite-app/src/command.rs`.

## Tests et qualité

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Benchmarks

Les benchmarks utilisent [criterion]. Les fichiers de test volumineux (jusqu'à 500 Mo) sont générés
de façon déterministe dans `bench-data/` à la première exécution (ignoré par git, prévoir ~1 Go libre).

```bash
cargo bench -p paulwrite-core                 # étape 1 : buffer, historique, curseurs
cargo build --release -p paulwrite-app
.\scriptsench-app.ps1                       # étape 2 : démarrage, latence de frappe, défilement
```

[criterion]: https://crates.io/crates/criterion

## Licence

[MIT](LICENSE)
