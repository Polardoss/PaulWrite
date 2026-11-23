# PaulWrite

Éditeur de texte pour Windows écrit en Rust, conçu pour être **aussi rapide que Sublime Text**.
La vitesse est le critère n°1, avant les fonctionnalités. Pas d'Electron, pas de webview, pas de
framework UI lourd.

> État : en construction. Étapes 0 à 4 en place : l'éditeur ouvre, édite et enregistre des fichiers
> dans des onglets, avec coloration syntaxique (Rust, JSON, TOML, Python, JavaScript, HTML, CSS, PHP et modèles `.phtml`, Markdown, YAML, XML, SQL, shell ; JavaScript et CSS à l'intérieur de `<script>` et `<style>`),
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
| `paulwrite-syntax` | Coloration incrémentale tree-sitter sur thread dédié (13 langages ; le HTML des fichiers PHP, le JavaScript et le CSS des pages HTML sont colorés aussi). |
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

## Installer

Pas besoin de compiler : l'archive `PaulWrite-<version>-windows-x64.zip` (2 Mo) contient
`paulwrite.exe` et deux scripts. Dans le dossier décompressé :

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1   # copie dans %LOCALAPPDATA%\Programs\PaulWrite, menu Démarrer, « Open with PaulWrite », et PaulWrite apparaît dans Paramètres ▸ Applications par défaut
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Associate .log,.csv  # ajoute d'autres extensions (27 courantes sont déjà déclarées)
powershell -ExecutionPolicy Bypass -File .\install.ps1 -AddToPath  # `paulwrite fichier` depuis un terminal
powershell -ExecutionPolicy Bypass -File .\install.ps1 -DryRun  # affiche ce qui serait fait, ne change rien
```

Tout est installé pour **l'utilisateur courant** (aucun droit d'administrateur). `uninstall.ps1`
(ou « Applications installées ») retire tout ; vos réglages (`%APPDATA%\PaulWrite`) sont conservés.
(Windows bloque les scripts téléchargés par défaut : d'où `-ExecutionPolicy Bypass`, qui ne vaut que pour cette commande.)
Détails : [`docs/distribution.md`](docs/distribution.md).

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
| Ctrl+N, Ctrl+Maj+N, Ctrl+W | nouvel onglet, **nouvelle fenêtre**, fermer l'onglet |
| Ctrl+molette, Ctrl+`+` / Ctrl+`-` / Ctrl+`0` | taille du texte : agrandir, réduire, par défaut (retenue dans `config.toml`) |
| (dossier dans un dépôt Git) | **état Git en lecture seule** : lettre et couleur à droite de chaque fichier de la barre latérale (`M` modifié orange, `A` ajouté et `U` non suivi en vert, `D` supprimé et `!` conflit en rouge, `R` renommé), point sur les dossiers qui contiennent des changements, fichiers ignorés grisés, onglets colorés, et la branche avec `↑` `↓` et le nombre de modifications dans la barre d'état |
| (fichier suivi par Git, enregistré) | **marqueurs dans la marge** des lignes changées par rapport à `HEAD` : barre verte (ajoutée), orange (modifiée), trait rouge (suppression juste au-dessus) ; un fichier non suivi est entièrement vert. Ils s'effacent dès la première frappe et reviennent à l'enregistrement |
| Ctrl+Maj+T | **rouvrir l'onglet fermé** (jusqu'à 20, avec leur curseur et leur défilement) |
| File ▸ Open Recent | les 30 derniers fichiers ouverts, d'une session à l'autre (`%APPDATA%\PaulWrite\recent`) |
| Ctrl+Tab, Ctrl+Maj+Tab, clic, clic milieu | onglet suivant, précédent, activer, fermer |
| Ctrl+P | Goto Anything : ouvrir un fichier du projet (recherche floue) |
| Ctrl+Maj+P | palette de commandes (toutes les commandes, avec leur raccourci) |
| Ctrl+G | aller à la ligne |
| Ctrl+Maj+F | rechercher du texte dans le projet |
| Ctrl+F, F3 / Maj+F3 | rechercher dans le document (occurrences surlignées, compteur « 3 sur 12 »), suivante / précédente |
| Ctrl+H, puis Tab | rechercher et remplacer : Entrée remplace l'occurrence, Ctrl+Alt+Entrée remplace tout (un seul Annuler) |
| Ctrl+Maj+H | **remplacer dans tout le projet** : chercher, Tab, remplacer par, Entrée ; un aperçu compte les occurrences et demande confirmation avant d'écrire sur le disque |
| Alt+C / Alt+W / Alt+R (dans la recherche) | respecter la casse, mot entier, expression régulière |
| Ctrl+D, Ctrl+L | sélectionner le mot puis l'occurrence suivante (multi-curseurs), sélectionner la ligne |
| Ctrl+Maj+D, Ctrl+Maj+K, Ctrl+Maj+↑/↓ | dupliquer, supprimer, déplacer la ou les lignes |
| Ctrl+/ ou Ctrl+: (clavier AZERTY) | commenter / décommenter : **une ligne** → `//` (ou `#`, `--`) ; **plusieurs lignes sélectionnées** → un seul `/* … */` (ou `<!-- -->`) autour des lignes, retiré au deuxième appui ; des lignes déjà commentées une à une sont décommentées une à une |
| Ctrl+Maj+/ ou Ctrl+Maj+: | **commentaire de bloc** `/* … */` autour de la sélection (ou de la ligne) ; retiré si déjà là |
| Tab / Maj+Tab | indenter / désindenter la sélection |
| (en tapant) | parenthèses, crochets, accolades et guillemets se ferment seuls ; Entrée garde l'indentation |
| Ctrl+Espace, F12, F1, Ctrl+Maj+M | serveur de langage (si activé) : complétion, définition, survol, diagnostics |
| Ctrl+Alt+↑ / Ctrl+Alt+↓ | ajouter un curseur au-dessus / en dessous |
| Échap | revenir à un seul curseur |
| Ctrl+↑ / Ctrl+↓, molette | faire défiler sans bouger le curseur |
| Clic, glisser, Maj+clic | placer le curseur, sélectionner |
| Clic dans la gouttière | sélectionner la ligne |
| Clic ou glisser dans la minimap, glisser la barre de défilement | faire défiler |
| Ctrl+K puis Ctrl+B, icône en bas à gauche | afficher / masquer la barre latérale |
| Glisser le bord de la barre latérale, son curseur de défilement | largeur, défilement |
| View ▸ Split Screen, Ctrl+Alt+← / → , Ctrl+Alt+Maj+← / → | **écran partagé** : deux fichiers côte à côte, chaque volet avec ses onglets ; un clic sur un volet lui donne le focus, la molette agit sur le volet sous la souris ; Ctrl+Alt+← / → change de volet, avec Maj envoie l'onglet dans l'autre (le partage se referme quand un volet est vide) ; View ▸ Clone File to Other Pane affiche **le même fichier dans les deux volets** (vues jumelles : curseurs et défilement propres, texte, enregistrement et annulation communs) |
| Clic droit sur un onglet | menu : **scinder à droite** (déplacer le fichier, ou le garder aussi ici), puis (en écran partagé) envoyer dans l'autre volet, défilement synchronisé, fermer le partage ; enregistrer, fermer |
| Arrêt défil ou Alt+Y (View ▸ Synchronized Scrolling) | **défilement synchronisé** des deux volets : ils restent à la même première ligne, pour comparer deux fichiers (le volet qui n'a pas le focus se cale à l'activation) |
| Alt+Z (View ▸ Word Wrap) | retour à la ligne automatique (actif par défaut) |
| (en revenant sur la fenêtre) | un fichier modifié par un autre programme est **rechargé tout seul** (curseur et vue conservés) s'il n'a pas été modifié ici ; sinon une question est posée |
| (au lancement, sans fichier en argument) | **reprise de session** : onglets, dossier, barre latérale, curseur et défilement de chaque fichier ; le texte non enregistré n'est pas conservé |
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
restore_session = true         # false : toujours démarrer vide (pas de reprise de session)
font_size = 15                  # taille du texte en pixels (8 à 48)
tab_size = 4                    # largeur d'une tabulation en colonnes (1 à 16)
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
