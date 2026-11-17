# Benchmarks PaulWrite

Chaque étape consigne ici ses mesures (objectif vs mesuré). Une régression bloque l'étape.

## Machine de mesure

Intel Core i5-10500 (6 cœurs / 12 threads, 3,1 GHz), 7,8 Go de RAM, SSD, Windows 11, Intel UHD 630.
Rust 1.100 stable, profil `bench` (= `release` : LTO fat, codegen-units = 1) avec symboles de lignes.
C'est une machine **modeste** ; une mesure sur machine récente reste à faire pour l'audit final.

## Étape 1 — `paulwrite-core`

Jeu de données : `bench-data/text-500mb.txt`, généré de façon déterministe (524 288 114 octets,
502 242 782 caractères, 7 209 364 lignes, UTF-8, LF, avec quelques caractères multi-octets).
Commandes : `cargo bench -p paulwrite-core` et
`cargo run --release -p paulwrite-core --example open_large -- bench-data/text-500mb.txt`.

Le brief ne fixe pas de seuil au core ; les seuils ci-dessous ont été posés **avant** les mesures.

| Scénario | Objectif | Mesuré | Verdict |
|---|---|---|---|
| Ouverture 500 Mo, lecture en flux (cache chaud) | worker, UI jamais gelée ; débit rapporté | 0,80 s (625 Mio/s) en benchmark, 0,91 s via l'exemple | OK (voir remarques) |
| Ouverture 500 Mo, mmap (cache chaud) | idem | 1,04 s (482 Mio/s) | Plus lent que le flux |
| Pic mémoire à l'ouverture de 500 Mo, flux | ~ taille du fichier | 585 Mo (≈ 1,12 × le fichier) | OK |
| Pic mémoire à l'ouverture de 500 Mo, mmap | idem | 1 084 Mo (working set, pages mappées comprises) | Pire que le flux |
| Insertion d'un caractère au milieu de 500 Mo (`Buffer`) | < 50 µs | 185 ns | OK (× 270 sous le seuil) |
| Insertion + suppression au milieu de 500 Mo | — | 400 ns | — |
| Frappe complète (`Document` : édition + historique) puis undo, 500 Mo | — | 786 ns | — |
| 1000 curseurs, 1 caractère inséré, document de 30 Ko | < 2 ms | 222 µs | OK |
| 1000 curseurs, insertion + undo, document de 30 Ko | < 2 ms | 373 µs | OK |
| 1000 curseurs répartis dans 500 Mo, insertion + undo | < 2 ms | 909 µs | OK |
| 1000 curseurs répartis dans 500 Mo, retour arrière + undo | < 2 ms | 1,15 ms | OK |
| 10 000 frappes groupées en un seul pas d'annulation | — | 11,6 ms (1,16 µs / frappe) | — |
| `char_to_line_col` aléatoire sur 500 Mo | — | 762 ns | — |
| Accès à une ligne aléatoire sur 500 Mo | — | 829 ns | — |
| `char_to_byte` aléatoire sur 500 Mo | — | 550 ns | — |

### Remarques et limites

- **mmap n'apporte rien ici** : plus lent (+30 %) et deux fois plus gourmand en working set que la
  lecture en flux, avec en plus un `unsafe` et un risque de plantage si un autre processus tronque le
  fichier pendant le chargement. Le défaut est donc la lecture en flux ; mmap reste activable via
  `LoadOptions::mmap_threshold`. Mesure à refaire avec un cache froid.
- **Cache chaud uniquement.** Les temps d'ouverture excluent la lecture disque : le fichier était déjà
  dans le cache système. Un premier chargement à froid sera borné par le débit du SSD.
- **Un rope de 500 Mo occupe ~585 Mo de RAM.** C'est acceptable sur la machine de test (7,8 Go) mais
  cela s'ajoute à la mémoire de l'éditeur (l'objectif « quelques dizaines de Mo » exclut les fichiers
  ouverts). Options si c'est un problème pour l'étape 2 : seuil « gros fichier » en lecture seule avec
  affichage paginé sur mmap, ou édition par pièces (piece table) sur mmap. Aucune n'est nécessaire
  tant que le chargement reste sur un worker.
- Ouverture ≈ 0,8 à 0,9 s : exécutée sur un worker avec progression et annulation, l'UI n'est pas
  gelée. Une construction parallèle du rope (plusieurs threads, puis concaténation) est possible si
  on veut descendre sous la seconde sur des machines plus lentes ; non fait faute de besoin mesuré.
- Les mesures de latence de frappe de bout en bout (touche → affichage) relèvent de l'étape 2.

## Étape 2 — fenêtre et rendu (`paulwrite-render`, `paulwrite-app`)

Application réelle, binaire `release`. Commandes : `cargo build --release -p paulwrite-app` puis
`.\scripts\bench-app.ps1` (démarrage, frappe, défilement) et
`paulwrite --bench-open --bench-out rapport.txt <fichier>` (fichier affiché). Le choix de rendu et le
prototype comparatif sont détaillés dans [`rendering.md`](rendering.md).

| Scénario | Objectif | Mesuré | Verdict |
|---|---|---|---|
| Démarrage à froid : création du processus → fenêtre visible avec la première image | < 100 ms | **76 ms** (médiane de 7, 74-80) ; 1re image présentée 78 ms | OK (gate) |
| Latence de frappe : événement → image copiée vers la fenêtre, 300 frappes | < 10 ms, p99 < 16 ms | moy 2,1 ms, p99 **3,6 ms**, max 15,2 ms | OK (gate) |
| Défilement : dessin + copie par image (3 lignes), 600 images | fluide | moy 0,92 ms, p99 3,0 ms | OK |
| Petit fichier source affiché (cache chaud) | — | 96 ms depuis la création du processus, 32 Mo de pic | — |
| Fichier de 500 Mo : fenêtre visible, puis fichier affiché (cache chaud) | UI jamais gelée | fenêtre 84 ms ; fichier affiché à **1,19 s** ; pic 613 Mo | OK (voir limites) |
| Mémoire au repos (document vide) | quelques dizaines de Mo | 32 Mo de pic (petit fichier) | OK |

### Remarques et limites (étape 2)

- **Ce que mesure la latence de frappe** : l'intervalle entre l'arrivée de l'événement dans notre
  boucle (frappes simulées, une toutes les 20 ms) et la fin de la copie de l'image vers la fenêtre.
  Elle **n'inclut pas** le trajet matériel du clavier jusqu'à Windows ni la composition par DWM
  (jusqu'à une période de rafraîchissement, ~16 ms à 60 Hz, avant l'affichage physique). Ce n'est donc
  pas une mesure photon-à-photon.
- **Frappe et défilement mesurés hors écran** (la fenêtre est placée hors de la zone visible pour ne
  pas gêner l'utilisateur) : GDI y copie très peu de pixels, donc la copie est un peu sous-estimée.
  Les mêmes mesures en fenêtre visible, sur le prototype, donnaient 1,6 ms de frappe (p99 4,4 ms) et
  1,2 ms par image de défilement : même ordre de grandeur.
- **Démarrage** : « fenêtre visible » est mesuré au retour de `set_visible`, « 1re image » à la fin de la
  première copie vers la fenêtre ; l'affichage physique suit la composition DWM. Le premier lancement
  d'un exécutable neuf coûte jusqu'à +330 ms (analyse antivirus) : une exécution de chauffe précède
  les mesures. Machine partagée avec d'autres processus.
- **Fichier de 500 Mo** : le chargement se fait sur un worker pendant que la fenêtre est déjà
  affichée (84 ms). On n'a **pas** injecté d'entrées pendant le chargement pour mesurer la réactivité ;
  c'est une conséquence de l'architecture, non une mesure. Le pic mémoire de 613 Mo est celui du rope
  (≈ taille du fichier) : l'objectif « quelques dizaines de Mo » vise l'éditeur sans gros fichier.
- **Défilement** : par lignes entières (le rendu par régions copie des pixels alignés sur les
  rangées) ; pas de défilement au pixel près. En 4K, le prototype en logiciel est à ~10 ms par image
  (copie de 33 Mo) : à surveiller si le plein écran haute résolution devient un cas d'usage.
- **Non vérifié** : saisie IME réelle (logique testée, pas essayée avec un vrai IME), multi-écrans avec
  DPI différents (changement d'échelle codé et testé hors fenêtre), clavier réel (raccourcis testés,
  événements Windows réels non injectés).

## Étape 3 — coloration, recherche, onglets

Commandes : `.\scripts\bench-app.ps1` (application : démarrage, frappe, coloration),
`cargo run --release -p paulwrite-syntax --example bench -- <Ko> <frappes>` (analyse seule),
`cargo run --release -p paulwrite-search --example gen_project -- bench-data\project-100k` puis
`cargo run --release -p paulwrite-search --example bench -- bench-data\project-100k` (recherche).
Le projet de test (`bench-data/`, ignoré par git) compte 100 000 fichiers de ~1,4 Ko répartis en
100 modules × 20 sous-dossiers × 50 fichiers, plus 3 000 fichiers dans `target/` et `node_modules/`
exclus par `.gitignore` (une recherche correcte ne doit pas les voir : vérifié, 100 résultats et non
3 100 pour le motif rare).

### Recherche et fuzzy (100 000 fichiers, cache chaud)

| Scénario | Objectif | Mesuré | Verdict |
|---|---|---|---|
| Recherche de texte, premiers résultats (regex courante `fn \w+_handler`, 1 fichier sur 20) | < 200 ms | **21 ms** (20-22) | OK |
| Recherche de texte, premiers résultats (littéral rare `needle_xyz`, 1 fichier sur 1000) | < 200 ms | 22 à 83 ms | OK |
| Recherche limitée à 100 résultats | — | 21 ms jusqu'aux premiers, 48-63 ms au total (≈ 2 100 fichiers lus) | — |
| Balayage complet des 100 000 fichiers (texte) | — | 1,8 s (≈ 55 000 fichiers/s, 12 threads) | — |
| Liste des 100 000 fichiers (`.gitignore` respecté) | — | premier lot 19-22 ms, total 130-145 ms | — |
| Indexation fuzzy des 100 000 chemins | — | 124 ms | — |
| Fuzzy, temps par frappe (84 frappes, 5 requêtes) | < 30 ms | p50 0,52 ms, p99 **6,8 ms**, max 6,8 ms | OK |
| Fuzzy, première lettre (100 000 candidats) | < 30 ms | 5,3 ms | OK |

**Premier passage à froid : objectif non tenu, cause connue.** Juste après la génération des
fichiers, la première recherche a mis **2,5 s** avant ses premiers résultats et **48 s** au total
(contre 2 s au total ensuite). Les 100 000 fichiers étaient neufs et jamais relus : le temps est
dominé par l'analyse antivirus de Windows (Defender) à la première lecture de chaque fichier, que
l'on ne contrôle pas. Impossible de reproduire à volonté (il faudrait vider le cache système, ce qui
demande des droits administrateur). Les chiffres du tableau sont donc ceux d'un projet déjà lu une
fois, cas habituel d'un projet sur lequel on travaille.

### Coloration syntaxique (analyse tree-sitter)

Fichier Rust synthétique valide (~50 octets par ligne, structure plate : le **pire cas** pour
l'analyse incrémentale de tree-sitter, qui coûte plus cher quand la racine a des milliers
d'enfants). Fenêtre de coloration : 230 lignes autour de la frappe. Frappes au milieu du document.

| Taille | Analyse complète (worker) | Analyse incrémentale par frappe (p50) | Requête de coloration (230 lignes) | Pic mémoire |
|---|---|---|---|---|
| 64 Ko | 14 ms | 0,5 ms | ~1 ms | — |
| 256 Ko | 57 ms | 2,2 ms | ~1 ms | — |
| **1 Mo** | 234 ms | **15,8 ms** (p99 24) | 1,1-1,3 ms | 56 Mo |
| 2 Mo | 553 ms | 43 ms | ~1 ms | 104 Mo |
| 4 Mo | 0,9 à 1,4 s | 72 à 77 ms (p99 98 à 166) | ~1 ms | 200 Mo |
| 16 Mo | 3,8 s | 304 ms | — | 785 Mo |

Compilation de la requête de coloration du Rust : 21 ms (une fois par document, sur le worker).

**Seuil par défaut : 1 Mo** (`highlight_max_bytes` dans `%APPDATA%\PaulWrite\config.toml`, ou
variable `PAULWRITE_HIGHLIGHT_MAX_BYTES`). Au-delà, pas de coloration. Raisons : l'analyse
incrémentale devient perceptible (> 40 ms) dès 2 Mo et la mémoire vaut environ 50 fois la taille du
fichier. `highlight = false` désactive la coloration partout.

| Scénario dans l'application (fichier Rust de ~1 Mo, 300 frappes, 1 toutes les 20 ms) | Mesuré |
|---|---|
| Frappe, événement → image copiée vers la fenêtre | moy 1,2 ms, **p99 3,0 ms** |
| Frappe → couleurs à jour reçues par le thread UI | moy 12,0 ms, p99 18,2 ms |
| Thread de coloration, analyse + requête par frappe | moy 11,7 ms, p99 15,3 ms |

La coloration ne ralentit **pas** la frappe (p99 3,0 ms contre 3,6 ms sans) : elle tourne sur son
propre thread et, en attendant sa réponse, les plages colorées existantes sont décalées par
l'éditeur lui-même, donc les couleurs ne « sautent » pas. Le retard des couleurs (~12 ms à 1 Mo)
n'est visible que sur le mot en cours de frappe. Si les frappes arrivent plus vite que l'analyse,
le worker regroupe les modifications et n'analyse qu'une fois.

### Application (onglets, démarrage)

| Scénario | Objectif | Mesuré | Verdict |
|---|---|---|---|
| Démarrage à froid, fenêtre visible avec barre d'onglets (21 exécutions) | < 100 ms | médiane **76 ms** (69-91) ; 1re image 80 ms (71-93) | OK (gate) |
| Latence de frappe, événement → image (300 frappes) | < 10 ms | moy 2,2 ms, p99 **3,6 ms** | OK (gate) |
| Défilement, dessin + copie par image | fluide | moy 0,87 ms, p99 2,6 ms | OK |
| Taille de l'exécutable | — | 4,1 Mo (7 grammaires comprises) | — |

Les onglets n'ont pas changé le démarrage (76 ms en médiane, comme à l'étape 2). Une série isolée a
donné 89 ms de médiane (maximum 123 ms) : machine occupée, non reproduite sur 21 exécutions.

### Remarques et limites (étape 3)

- **Données synthétiques** : le code Rust de test est régulier et plat ; des fichiers réels (modules
  imbriqués) devraient être plus favorables à l'analyse incrémentale, mais JSON volumineux ou code
  généré peuvent ressembler au cas mesuré. Seul le Rust a été mesuré en détail (7 langages sont
  vérifiés par des tests de bon fonctionnement, pas de performance).
- **Coloration de HTML** : les scripts et styles intégrés (`<script>`, `<style>`) ne sont pas colorés
  dans le langage correspondant (pas d'injection de langage).
- **Interface de recherche** : le crate `paulwrite-search` fournit la liste de fichiers, la recherche
  de texte et le fuzzy finder, mesurés ci-dessus. L'interface (palette de commandes, Goto Anything,
  panneau de résultats) est l'objet de l'étape 4 : rien n'est encore appelable depuis l'éditeur.
- **Onglets** : la logique de gestion des onglets (ouverture, fermeture avec confirmation,
  enregistrement avant fermeture) est dans la boucle de l'application et n'est pas couverte par des
  tests automatiques (elle exige une fenêtre) ; le rendu de la barre, lui, est testé (régions
  identiques au redessin complet, zones de clic).
- **Non-régression du core** (le buffer tient maintenant un journal des modifications, désactivé par
  défaut) : insertion au milieu de 500 Mo 187 ns (185 ns à l'étape 1), insertion + suppression 408 ns
  (400 ns), frappe + undo 805 ns (786 ns), 1000 curseurs 225 µs (222 µs), 1000 curseurs avec undo
  402 µs (373 µs, +8 %, à comparer au seuil de 2 ms). Aucune régression bloquante.

## Étape 4 — palette de commandes, Goto Anything, recherche dans le projet

Mesures faites **dans l'application réelle** (binaire `release`, fenêtre hors écran) sur le projet de
100 000 fichiers de l'étape 3 : `.\scripts\bench-app.ps1` (section « Goto Anything » et
« Recherche dans le projet »), ou directement
`paulwrite --bench-palette 300 --bench-out rapport.txt bench-data\project-100k` et
`paulwrite --bench-palette-search needle_xyz --bench-out rapport.txt bench-data\project-100k`.
Goto Anything : 300 frappes simulées (une toutes les 25 ms) sur trois requêtes (par exemple
`mod_017/sub_05/file_0023.py`), la saisie étant effacée entre deux requêtes. La latence va de
l'arrivée de la frappe dans la boucle d'événements jusqu'à la copie vers la fenêtre de l'image qui
montre les résultats **à jour** de cette frappe (le classement de nucleo est terminé).

| Scénario (100 000 fichiers, cache chaud) | Objectif | Mesuré | Verdict |
|---|---|---|---|
| Goto Anything, latence par frappe | < 30 ms | p50 2,3 à 5,7 ms, p95 8,7 à 19,8 ms, **p99 10,7 à 27,4 ms**, maximum 11,5 à 29,9 ms | OK, marge faible sur la pire frappe |
| dont classement seul (nucleo, avant le dessin) | — | p50 0,8 à 2,0 ms, p99 9 à 24 ms | — |
| Goto Anything, parcours + indexation du projet à l'ouverture | — | 140 à 220 ms (une fois en 351 ms), en arrière-plan : la saisie reste possible | — |
| Recherche dans le projet, premiers résultats affichés (motif courant `_handler`) | < 200 ms | **29 à 40 ms** ; terminée (2000 résultats) en 0,8 s | OK |
| Recherche dans le projet, premiers résultats affichés (motif rare `needle_xyz`, 100 résultats) | < 200 ms | 54 à 131 ms ; terminée en 2,0 à 3,6 s | OK |
| Démarrage à froid, étape 3 contre étape 4, **même session, runs alternés** (25 + 25) | < 100 ms | médiane 84 ms contre 83 ms (min 74 / 74, max 95 / 95) | OK, aucune régression |
| Latence de frappe dans le texte (300 frappes) | < 10 ms | moy 0,95 ms, p99 2,3 ms | OK |
| Défilement (600 images), coloration (frappe → couleurs) | — | p99 3,3 ms ; moy 12,4 ms, p99 17,4 ms | inchangé |
| Taille de l'exécutable | — | 5,0 Mo (4,1 Mo à l'étape 3 : +0,9 Mo pour `ignore`, `grep-*`, `nucleo`) | — |

### Remarques et limites (étape 4)

- **Variabilité entre sessions** : la même mesure de Goto Anything a donné des p99 de 11 à 27 ms
  selon le moment (même binaire, même machine). La plage ci-dessus regroupe une trentaine de
  séries. La frappe la plus lente d'une série a atteint 29,9 ms avec la configuration par défaut,
  donc l'objectif « < 30 ms » est tenu mais sans marge sur la pire frappe. Une série avec 4 threads
  de classement imposés a mesuré 31,4 ms ; ce n'est pas la configuration par défaut.
- **D'où vient la queue** : l'interface elle-même ajoute environ 2 ms (de « classement terminé » à
  « résultats affichés ») ; le reste est le classement de nucleo sur les **premières lettres d'une
  requête**, quand 100 000 candidats correspondent encore. Dès que la requête se précise, c'est
  quasi instantané (médiane 0,8 à 2 ms). Tests sans effet mesurable, **retirés** : limiter le nombre
  de threads de classement (une série isolée a montré un gain de 2× qui ne s'est pas reproduit),
  demander une minuterie de 1 ms à Windows, désactiver le bridage des processus d'arrière-plan,
  sonder la fin du classement.
- **Bug trouvé par la mesure** : nucleo notifie la boucle à chaque candidat ajouté (100 000 fois pour
  un gros projet) ; sans regroupement des réveils, la boucle de l'application était saturée et
  Goto Anything ne se terminait jamais. Un seul réveil est maintenant en route à la fois.
- **Premier passage à froid** : comme à l'étape 3, la première recherche après un changement de
  binaire ou sur des fichiers non relus peut prendre plusieurs secondes (analyse antivirus à la
  première lecture) ; non reproductible à volonté.
- **Recherche dans le projet** : texte littéral, insensible à la casse sauf si la saisie contient une
  majuscule ; minimum 2 caractères ; 2 000 résultats au plus (la liste signale quand elle est
  tronquée). Pas encore d'expressions régulières ni de remplacement dans l'interface.
- **Goto Anything** : les 200 meilleurs résultats sont navigables ; le nombre total de
  correspondances est affiché. Les chemins longs sont tronqués par la gauche (le nom du fichier
  reste visible).
- **Non vérifié** : saisie dans le panneau avec un vrai clavier (accents, AltGr), composition IME dans
  le panneau (seule la validation est prise en compte), clic de souris dans le panneau (zones de
  clic testées, pas essayées à la main), comportement sur plusieurs écrans. Les mesures utilisent
  des frappes simulées.
