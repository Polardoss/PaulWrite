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

## Étape 5 — plugins Lua et serveurs de langage (LSP)

Le brief ne fixe pas d'objectif propre à cette étape ; j'ai donc posé les miens **avant de mesurer** :
la saisie ne doit pas ralentir quand un serveur de langage est actif (p99 < 10 ms), le démarrage reste
sous 100 ms (les serveurs ne démarrent qu'à l'ouverture d'un document, après la première image), et
l'interface ne bloque jamais sur un serveur lent (structurel : tout passe par des canaux ; pas de
mesure possible sans serveur réel lent).

Mesures dans l'application réelle (binaire `release`, fenêtre hors écran) :
`paulwrite --bench-lsp 300 --bench-out rapport.txt` avec `PAULWRITE_LSP_SERVER` pointant vers
`target\release\paulwrite-fake-lsp.exe` (section « Serveur de langage » de `.\scripts\bench-app.ps1`).
Le fichier est un source Rust de 5 000 lignes, coloré, avec le serveur de test actif ; 300 frappes
(une toutes les 20 ms), et une requête de survol toutes les 10 frappes.

**Le faux serveur répond instantanément** : ces mesures isolent le coût côté éditeur (suivi des
modifications, sérialisation, thread du client) et non celui d'un vrai serveur.

| Scénario | Objectif | Mesuré | Verdict |
|---|---|---|---|
| Frappe → image copiée, serveur actif (3 séries de 300) | p99 < 10 ms (objectif de l'étape ; < 16 ms au brief) | p50 1,3 à 1,6 ms ; p99 **3,1 / 10,9 / 15,7 ms** ; maximum 3,5 / 26,3 / 23,6 ms | **Partiel** : < 10 ms dans 1 série sur 3 ; < 16 ms dans 3 sur 3, mais de justesse |
| Même mesure **sans** serveur, coloration seule (3 séries) | — | p50 1,05 à 1,11 ms ; p99 **11,1 / 15,4 / 5,0 ms** | référence : mêmes valeurs aberrantes |
| Même mesure sans serveur ni coloration (3 séries) | — | p50 2,1 à 2,7 ms ; p99 10,0 / 3,5 / 3,6 ms | référence |
| Aller-retour d'une requête de survol (faux serveur local) | — | médiane 1,3 à 1,6 ms ; p99 3,1 à 3,5 ms (29 requêtes par série) | — |
| Démarrage à froid, fenêtre visible (7 runs) | < 100 ms | médiane 75,5 ms (74 à 87) | OK |
| Taille de l'exécutable (plugins Lua + LSP) | — | 5,74 Mo (5,0 Mo à l'étape 4) | — |

### Remarques et limites (étape 5)

- **Pas de régression démontrée, mais l'objectif de p99 < 10 ms n'est pas tenu à chaque série.** Les
  séries *sans* serveur montrent les mêmes valeurs aberrantes (p99 de 11 à 15 ms avec la coloration),
  et la série avec serveur la plus lente (15,7 ms) tombe dans cette plage : rien n'attribue ces
  queues au LSP, mais je n'ai pas non plus isolé leur cause (concurrence avec le thread de coloration
  sur une machine à 6 cœurs, ordonnancement Windows ? non établi). Trois séries par cas ne
  permettent pas de chiffrer un écart plus fin que quelques millisecondes.
- **Démarrage** : aucune différence attendue (les serveurs sont désactivés par défaut et lancés à la
  demande) ; la médiane de 75,5 ms est dans la plage des étapes 3 et 4 (83 à 84 ms, autre session).
- **Aucun vrai serveur essayé** (rust-analyzer & co. non installés ici) : les temps d'un vrai serveur
  (indexation, complétion de plusieurs centaines d'éléments) ne sont pas mesurés. Le panneau de
  complétion est borné à 5 000 éléments.
- **Plugins Lua** : chargement de 5 plugins d'exemple en 5 ms, environ 0,5 Mo ajoutés à
  l'exécutable (voir [`plugins.md`](plugins.md)). Limites de temps et de mémoire appliquées par
  plugin, mais **ce n'est pas une frontière de sécurité**. Le coût d'un appel de plugin n'a pas
  été mesuré dans le chemin de saisie : aucun plugin n'y est appelé par frappe.
- Limites fonctionnelles du LSP : voir [`lsp.md`](lsp.md).

## Interface « Sublime Text » (début de l'étape 6)

Refonte visuelle : gouttière, onglets, barre d'état, minimap, barre de défilement, barre latérale,
panneau clair, barre de menus (voir [`interface.md`](interface.md)). Chaque élément ajoute du travail
à chaque image : la mesure a donc été faite **avant de conclure**, et une première version a été
corrigée.

Mesures dans l'application réelle (binaire `release`, fenêtre hors écran 1100×720, `.\scriptsench-app.ps1`).
La référence « avant » est la même machine, la même séance, juste avant la refonte (3 séries).

| Scénario | Objectif | Avant la refonte | Première version | **Version finale** | Verdict |
|---|---|---|---|---|---|
| Frappe → image copiée, 300 frappes | < 10 ms (p99 < 16) | moy 2,25–2,36 ms, p99 3,5–10,0 | moy 6,26 ms, p99 10,2 | **moy 2,7–3,7 ms, p50 2,0–3,5, p99 6,6–7,0, max ≤ 7,9** (4 séries) | OK |
| Frappe dans un fichier Rust de 1 Mo (coloration) | < 10 ms | moy 1,3–1,7, p99 5–15 | — | moy 1,71, p99 4,7 | OK |
| Frappe avec un serveur de langage actif | p99 < 10 ms | moy 1,67, p99 3,1–15,7 | — | moy 1,62, p99 3,1 | OK |
| Défilement, 600 images de 3 lignes | fluide | — (0,8 ms p50 hors fenêtre) | moy 1,95, p99 4,4 | **moy 1,27–1,33 ms, p99 3,4–4,4** (4 séries) | OK |
| Démarrage à froid, fenêtre visible (7 runs) | < 100 ms | médiane 75,5 ms | — | **médiane 78 ms** (74–117) | OK |
| Goto Anything, frappe → résultats affichés (100 000 fichiers) | < 30 ms | p99 10,7–27,4 | — | p99 26,3–30,8 (2 séries) | à la limite, comme à l'étape 4 |
| Recherche dans le projet, premiers résultats | < 200 ms | 29–131 ms | — | 29–47 ms (motif courant) ; 47–277 ms (motif rare, 4 séries) | OK (une série à 277 ms) |
| Taille de l'exécutable | — | 5,74 Mo | — | 5,87 Mo | — |

### Ce que la mesure a montré

- **La première version coûtait 2,7× plus par frappe** (moyenne 6,26 ms contre 2,3). Mesuré hors
  fenêtre (`cargo test --release -p paulwrite-render mesure_frappe -- --ignored --nocapture`, 1100×720,
  CPU chaud) : la rangée seule coûte 0,36 ms, la barre d'état ajoute 0,33 ms, la minimap 0,8 ms.
  Dans l'application, avec 20 ms de repos entre deux frappes, les mêmes opérations coûtent environ
  6 fois plus (le processeur ralentit pendant les pauses) : l'écart se voit donc beaucoup plus que
  dans un test en boucle serrée.
- **Corrections** : (1) la minimap n'est reconstruite qu'au plus tous les 150 ms pendant la frappe,
  tout de suite au défilement ; (2) la zone de droite (minimap, barre de défilement) est dessinée par
  copie de pixels et non par une image Direct2D (0,8 ms → 0,2 ms) ; (3) seule la partie gauche de la
  barre d'état, qui change à chaque frappe, est redessinée. Reste, par frappe : 0,36 ms (rangée)
  + 0,33 ms (barre d'état, dont la création du texte) mesurés à chaud.
- **Le coût qui reste est la barre d'état** (position du curseur, mise à jour à chaque frappe) :
  +0,4 à +1,3 ms par frappe dans l'application, sans dépasser l'objectif (p99 7 ms).
- **Minimap en retard** : elle peut montrer l'état du texte d'il y a 150 ms pendant la frappe
  continue ; elle se met à jour dès qu'on s'arrête (une image est redemandée à l'échéance).
- **Machine bruitée** : un antivirus (Kaspersky) a bloqué l'accès à l'exécutable pendant près de
  15 minutes après une série d'essais ; une recherche mesurée à ce moment-là a pris 68 s au lieu de
  2 s (non retenue, refaite 4 fois). Les valeurs ci-dessus viennent de séries hors de cette période.
- **Non mesuré** : coût de la barre latérale ouverte (dossier de 100 000 fichiers : listing de la
  racine seulement, mais l'arbre aplati est reconstruit à chaque dépli), rendu à 125 % et 150 % de
  mise à l'échelle Windows, minimap d'un fichier de 500 Mo (elle lit les lignes affichées seulement).

## Retour à la ligne et repli de code (étape 6)

Ajout : retour à la ligne automatique (actif par défaut), repli par indentation, défilement
horizontal (voir [`interface.md`](interface.md)). Comme pour l'interface, la mesure compare
**deux binaires construits dans la même séance** : le commit précédent (`8e0b12c`, dans un worktree
à part) et la version courante, lancés en alternance (même machine, même charge) avec
`.\scripts\bench-app.ps1` (fenêtre hors écran 1100×720). Le texte synthétique des mesures a des
lignes de 30 à 110 caractères : une partie d'entre elles se coupe dans une fenêtre de 1100 px, donc
le retour à la ligne **travaille réellement** pendant la mesure.

| Scénario | Objectif | Avant | **Après** | Verdict |
|---|---|---|---|---|
| Frappe → image copiée, 300 frappes (8 paires) | < 10 ms (p99 < 16) | moy 2,9–4,1 ms, p99 6,3–8,0, max ≤ 17,4 | **moy 3,3–4,4 ms, p99 6,8–7,8** dans 6 séries sur 8, max ≤ 12,9 ; **2 séries avec un pic** (p99 20,1 ms, max 31 ms ; max 73 ms) | OK en général, **non reproduit** : voir ci-dessous |
| Défilement, 600 images de 3 lignes (4 paires) | fluide | moy 1,18–1,27 ms, p99 2,8–3,3 | **moy 1,24–1,31 ms, p99 3,0–3,4** | OK, écart négligeable |
| Frappe dans un fichier Rust de 1 Mo, coloration (3 paires) | < 10 ms | moy 1,70–1,79, p99 4,4–5,5 | **moy 1,75–1,78, p99 4,5–5,3** | OK, identique |

Les valeurs absolues sont plus hautes que celles du tableau de l'interface (machine plus chargée ce
jour-là) ; seul l'écart entre les deux binaires, mesurés en alternance, compte. Sur la frappe, la
moyenne augmente d'environ **0,1 à 0,3 ms** (la construction des rangées passe de 0,07 à 0,21 ms
dans l'application, rangées de la fenêtre construites à froid) ; au p99, +0,5 ms.

Les deux séries avec pic se suivaient dans le temps ; le binaire « avant » a eu un pic de 17 ms dans
la série 7 ; une vingtaine d'autres séries de la version courante, mesurées dans la même séance,
n'ont jamais dépassé p99 9,4 ms / max 17,6 ms. Je les attribue à la machine (antivirus), mais **je
ne peux pas l'exclure** : elles ne se sont pas reproduites.

### Coût de la construction des rangées sur un gros document

Mesure sans fenêtre (`cargo test --release -p paulwrite-app --test frame_cost cout_du_retour --
--ignored --nocapture`) : 150 000 lignes, 7,2 Mo, vue de 40 rangées de 90 colonnes.

| Opération | Sans retour à la ligne | Avec retour à la ligne |
|---|---|---|
| Construction des rangées (`build_view`), moyenne / p99 | 0,04 / 0,05 ms | 0,06 / 0,10 ms |
| Molette (3 rangées) + construction | 0,03 / 0,05 ms | 0,05 / 0,08 ms |
| Frappe + construction | 0,02 / 0,03 ms | 0,07 / 0,12 ms |
| Fold All (25 000 régions, 150 000 lignes) | — | **73 à 85 ms** (une fois, thread de l'interface) |
| Unfold All | — | 0,1 ms |

(Ctrl+Fin, Ctrl+Début et page suivante : 0,03 à 0,25 ms, mesurés avant les dernières optimisations.)

### Ce que la mesure a montré

- **Le coût du retour à la ligne est faible** : la découpe n'est calculée que pour les ~40 rangées
  visibles ; aucune structure ne dépend de la taille du document, sauf le repli (une entrée par
  région repliée). Une première version coûtait 0,07 ms (sans retour à la ligne) / 0,11 ms (avec)
  par construction : chercher les lignes repliables pour les flèches ▼ à chaque image (maintenant
  fait seulement pendant le survol de la gouttière), et relire plusieurs fois le texte de chaque
  ligne (maintenant une seule lecture) ont ramené cela à 0,04 / 0,06 ms.
- **Le coût visible en frappe vient surtout du reflux du texte** : taper au début d'une ligne longue
  redessine toutes ses rangées (3 ou 4), là où le texte clippé n'en redessinait qu'une. C'est inhérent
  au retour à la ligne.
- **Première version de Fold All : 398 ms.** Elle appelait `Folds::fold` en boucle (insertion triée,
  quadratique) : 25 000 régions → 625 millions d'opérations. Remplacé par une construction linéaire
  (`replace_all`) : 73 à 85 ms (la lecture des lignes en est la plus grande part, non détaillé).
  **Reste supérieur à 50 ms** : c'est une commande ponctuelle, bornée à 100 000 lignes, mais elle
  fige l'interface ce temps-là ; la déplacer sur un worker est possible si cela gêne.
- **Recalcul des régions à chaque frappe** : recalculer la forme en lignes de toutes les régions à
  chaque frappe coûterait de l'ordre de plusieurs ms après un Fold All sur 25 000 régions (estimé,
  non mesuré). Une modification qui garde le même nombre de lignes ne recalcule plus rien (seuls les
  décalages en octets sont mis à jour).
- **Taper au bord droit sans retour à la ligne** coûtait 5 ms par frappe (toutes les rangées
  redessinées à chaque défilement horizontal d'une colonne) : le défilement se fait maintenant par
  à-coups d'un quart de largeur.
- **Plafonds** : retour à la ligne désactivé au-delà de 16 Mo (`word_wrap_max_bytes`) et pour les
  lignes de plus de 200 000 caractères, qui ne sont pas découpées (un parcours complet par image).
- **Non mesuré** : défilement avec retour à la ligne dans un fichier de 16 Mo en fenêtre 4K ; rendu à
  125 % et 150 % ; fichiers dont toutes les lignes se coupent en dizaines de rangées.

## Minimap colorée et langue (étape 6)

Ajout : la minimap est colorée par la syntaxe (le worker colore le document entier quand il est au
repos), et l'interface existe en anglais et en français. Même méthode que pour le retour à la ligne :
le commit précédent (`ad0949a`, dans un worktree) contre la version courante, lancés en alternance
avec `.\scriptsench-app.ps1` (fenêtre hors écran 1100×720).

| Scénario | Objectif | Avant | **Après** | Verdict |
|---|---|---|---|---|
| Frappe, texte synthétique, 300 frappes (6 paires) | < 10 ms (p99 < 16) | moy 2,9–4,5 ms, p99 6,4–15,2 | **moy 3,2–4,0 ms, p99 6,6–7,7** | OK, identique |
| Frappe dans un fichier Rust de 1 Mo, coloration (8 paires, après chauffe) | < 10 ms | moy 1,66–1,87, p99 3,7–5,0 | **moy 1,94–2,24, p99 3,9–7,6**, un max à 24,6 ms | OK (+0,25 ms en moyenne) |
| Défilement, 600 images de 3 lignes (4 paires) | fluide | moy 1,19–1,43, p99 2,8–4,4 | **moy 1,19–1,47, p99 2,9–4,5** | OK, identique |

Côté coût, sans fenêtre (`cargo test --release -p paulwrite-app --test frame_cost cout_de_la_minimap --
--ignored --nocapture`), document Rust de 1,00 Mo, 40 992 lignes :

| Opération | Mesuré |
|---|---|
| Coloration du document entier, côté worker (209 054 plages) | **213 ms** (par morceaux de 64 Ko : le worker traite une frappe entre deux morceaux) |
| Décalage des plages de la minimap à chaque frappe (`take_edits`) | moy 0,20 ms, p99 0,71 ms |
| Construction de l'image de la minimap, colorée / grise | moy 0,19 / 0,12 ms (p99 0,30 / 0,27) |

### Ce que la mesure a montré

- **Le coût en frappe est de l'ordre de 0,25 ms** sur un fichier de 1 Mo (dominé par le décalage des
  209 000 plages à chaque frappe ; il croît avec le nombre de plages, non mesuré sur d'autres tailles). Il vient de
  mon choix de garder les couleurs de la minimap pendant la frappe plutôt que de la griser.
- **Deux séries mesurées juste après la compilation** (binaire neuf) ont montré un p99 de 14 à 17 ms ;
  elles ont disparu après une exécution de chauffe (8 séries suivantes : p99 3,9–7,6). Je les attribue à
  l'analyse antivirus du binaire neuf, comme pour le démarrage (voir plus haut), mais je les signale.
- **La coloration entière ne gêne pas la frappe** : le worker ne colore que lorsque sa file est vide
  et abandonne le calcul à la première modification ; elle est redemandée 400 ms après la dernière
  frappe. Un fichier de plus de `highlight_max_bytes` (1 Mo par défaut) garde une minimap grise.
- **La langue ne coûte rien à l'exécution** : un choix de texte par message, aucun fichier lu.

## Correction : défilement (bug de l'étape 2 trouvé à l'étape 4)

**Bug** : après un défilement, le milieu de la fenêtre gardait l'ancien texte pendant que le haut et
le bas changeaient. Le rendu déplace les pixels déjà dessinés dans l'image en mémoire (rapide), mais
la copie vers la fenêtre ne portait que sur les lignes *redessinées* : la zone déplacée n'était
jamais recopiée. Corrigé (la zone déplacée fait maintenant partie de la copie). Les tests de
l'étape 2 comparaient l'image en mémoire avec un redessin complet, jamais ce que reçoit la
fenêtre : un nouveau test simule la fenêtre (cible mémoire) et vérifie qu'elle reste identique à
l'image en mémoire après défilements, frappes, panneau et changements d'onglet. Il échoue sans la
correction (vérifié).

**Mesures corrigées.** Les « 0,9 ms par image » des étapes 2 à 4 venaient d'une fenêtre hors écran,
où Windows rogne la copie, et ne comptaient pas la copie de la zone déplacée : elles étaient trop
optimistes. Coût réel d'une image de défilement de 3 lignes (dessin + déplacement + copie de toute la
zone) sur une cible non rognée (`cargo test --release -p paulwrite-render mesure_defilement --
--ignored --nocapture`) :

| Fenêtre | p50 | p95 | max | Verdict |
|---|---|---|---|---|
| 1100×720 | 0,8 ms | 1,5 ms | 2,3 ms | fluide |
| 1920×1080 | 5,9 ms | 12,1 ms | 16,3 ms | fluide en général, quelques images à la limite de 16 ms |
| 3840×2160 | 16,5 ms | 21,3 ms | 29,2 ms | **pas fluide à 60 images/s** |

La copie de la surface entière (deux fois : décalage dans l'image en mémoire, puis envoi à la
fenêtre) est limitée par la bande passante mémoire : 33 Mo par copie en 4K. Pistes si cela devient
un cas d'usage : défilement par le GPU (swap chain, voir `rendering.md`), ou ne déplacer que la
zone visible utile. Aucune n'est entreprise sans votre avis.
