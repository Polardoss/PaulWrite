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
