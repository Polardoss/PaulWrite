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
