# PaulWrite

Éditeur de texte pour Windows écrit en Rust, conçu pour être **aussi rapide que Sublime Text**.
La vitesse est le critère n°1, avant les fonctionnalités. Pas d'Electron, pas de webview, pas de
framework UI lourd.

> État : en construction. Étapes 0 (fondations) et 1 (`paulwrite-core`) en cours ; il n'y a pas
> encore d'interface graphique.

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
| `paulwrite-render` | Fenêtre winit et rendu du texte (DirectWrite ou wgpu, choix mesuré). *(étape 2)* |
| `paulwrite-app` | Commandes, raccourcis, onglets, config, thèmes. Produit le binaire `paulwrite`. |

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
cargo bench -p paulwrite-core
```

[criterion]: https://crates.io/crates/criterion

## Licence

[MIT](LICENSE)
