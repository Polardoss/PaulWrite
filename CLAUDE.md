# CLAUDE.md — PaulWrite

Éditeur de texte Windows en Rust, **ultra-rapide** (cible : aussi rapide que Sublime Text).
La vitesse est LE critère n°1, avant les fonctionnalités. Pas d'Electron, pas de webview,
pas de framework UI lourd. Dépôt PUBLIC : https://github.com/Polardoss/PaulWrite

## Règles de conception (non négociables)

1. Le thread UI ne fait JAMAIS d'I/O, de parsing ni de calcul lourd : tout passe par des canaux
   (crossbeam-channel) vers des workers.
2. Rendu à la demande (pas de boucle 60 fps permanente), invalidation par régions, seules les
   lignes visibles sont rendues.
3. Chaque fonctionnalité est une commande nommée (enum/registre). Raccourcis, palette de
   commandes et futurs plugins appellent les mêmes commandes.
4. Chaque étape a son benchmark. Une régression de performance bloque l'étape.
5. Gros fichiers : lecture en flux ou mmap ; coloration désactivée/partielle au-delà d'un seuil
   configurable.
6. Zéro allocation inutile dans le chemin de saisie et de rendu. Profiler avant d'optimiser, mais
   concevoir pour ne pas tout réécrire.
7. Pas de dépendance ajoutée sans justification (poids, temps de compilation, impact démarrage).

## Objectifs de performance (à mesurer, jamais à supposer)

- Démarrage à froid < 100 ms jusqu'à la première fenêtre
- Latence de frappe < 10 ms (p99 < 16 ms)
- Fichier de 500 Mo : ouverture sans geler l'UI, défilement fluide
- Mémoire au repos : quelques dizaines de Mo (hors fichiers)
- Recherche projet de 100 000 fichiers : premiers résultats < 200 ms
- Fuzzy finder : < 30 ms par frappe

## Architecture (workspace Cargo, `crates/`)

- `paulwrite-core` : buffer (ropey), sélections multiples, undo/redo, encodages, I/O. Zéro UI.
- `paulwrite-syntax` : coloration incrémentale tree-sitter, thread dédié.
- `paulwrite-search` : recherche fichier/projet (ignore + grep-regex), fuzzy (nucleo).
- `paulwrite-render` : winit + DirectWrite ou wgpu (choix après prototype mesuré, documenté).
- `paulwrite-app` : commandes, raccourcis, onglets, config, thèmes → binaire `paulwrite`.
- Plus tard : `paulwrite-lsp`, `paulwrite-plugins` (WASM ou Lua).

## Méthode de travail

- Avancer étape par étape (0 fondations, 1 core, 2 fenêtre/rendu, 3 coloration/recherche/onglets,
  4 palette/Goto Anything, 5 LSP/plugins, 6 finitions).
- À la fin de chaque étape : tests verts, benchmarks exécutés, tableau objectif vs mesuré, puis
  STOP et attendre le feu vert de l'utilisateur avant l'étape suivante.
- Petits commits atomiques, messages clairs. Push à la fin de chaque étape validée (le tout premier
  push nécessite l'accord explicite de l'utilisateur).
- Objectif de perf non atteint : le dire franchement, expliquer la cause, proposer des options
  plutôt que de passer à la suite.
- Décision d'architecture ambiguë : poser la question en une seule fois, avec recommandations.
- Dépôt public : jamais de secrets, tokens, chemins personnels ni config locale dans les commits.
- Licence : MIT.

## Commandes utiles

```bash
cargo build                                  # debug
cargo build --release                        # lto fat, codegen-units=1, panic=abort
cargo build --profile profiling              # release + symboles de lignes
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo bench -p paulwrite-core                # données générées dans bench-data/ (gitignoré)
```

Environnement : Windows, toolchain `stable-x86_64-pc-windows-msvc` + VS 2022 Build Tools (C++).
Si `cargo` n'est pas dans le PATH : `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`.
CI : GitHub Actions `windows-latest` (fmt + clippy `-D warnings` + build + test ; benchs compilés
seulement, jamais exécutés en CI car trop bruités).

## Conventions de code

- Positions internes du buffer en **offsets de caractères** (natif ropey) ; helpers byte/UTF-16
  fournis pour tree-sitter et LSP.
- `unsafe_code = "deny"` au niveau workspace ; seul `memmap2` (lecture mmap) est autorisé, isolé et
  documenté localement.
- Le core n'utilise pas l'horloge système : les temps sont injectés (tests déterministes).
