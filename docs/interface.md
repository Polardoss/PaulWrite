# Interface (apparence « Sublime Text »)

L'interface reproduit la **disposition et les couleurs** de Sublime Text 4 (thème « Mariana »,
interface « Adaptive ») d'après des captures d'écran fournies par l'utilisateur. Aucune ressource
propriétaire n'est copiée : ni police, ni icône, ni fichier de thème — seulement des mesures de
pixels (couleurs, hauteurs, marges). Les polices sont celles de Windows (Consolas pour le texte,
Segoe UI pour l'interface).

## Disposition

```
┌──────────────────────────────────────────────────────────────┐
│ File  Edit  Selection  Find  View  Goto  Tools  Project …    │  barre de menus Windows native
├────────┬─────────────────────────────────────────────┬───────┤
│FOLDERS │ ◀ ▶ [ main.rs  × ] autre.rs          +  ▾   │       │  onglets (33 px)
│ ▾ proj ├─────────────────────────────────────────────┤ mini- │
│  ▸ src │ 1  fn main() {                              │ map   │
│  README│ 2      …                                    │       │  gouttière + texte
│        │                                             │     ▮ │  barre de défilement
├────────┴─────────────────────────────────────────────┴───────┤
│ ▯ Line 2, Column 5                       Tab Size: 4   Rust  │  barre d'état (25 px)
└──────────────────────────────────────────────────────────────┘
```

| Élément | Comportement |
|---|---|
| Barre de menus | native Windows : File, Edit, Selection, Find, View, Goto, Tools, Project, Preferences, Help. Chaque entrée est une commande nommée (`menu.rs`) avec son raccourci affiché. |
| Onglets | arrondis ; × pour fermer, pastille pour « modifié » ; ◀ ▶ précédent/suivant, + nouveau, ▾ liste des onglets ouverts. |
| Gouttière | numéros de ligne, ligne du curseur en évidence ; un clic sélectionne la ligne. Largeur adaptée au nombre de chiffres. Flèches de repli : ▼ (région repliable, visible quand la souris survole la gouttière) et ▶ (région repliée, toujours visible) ; un clic dessus replie ou déplie. |
| Retour à la ligne | actif par défaut : une ligne longue occupe plusieurs rangées (le numéro n'est sur que la première). Alt+Z ou View ▸ Word Wrap pour le désactiver ; sans lui, la vue défile horizontalement (Maj+molette, molette latérale, ou en suivant le curseur). |
| Repli de code | Edit ▸ Fold / Unfold / Fold All / Unfold All, Ctrl+Maj+[ et Ctrl+Maj+]. Une région repliée se termine par « … » (un clic dessus la déplie) ; entrer dedans au clavier la déplie. |
| Guides d'indentation | traits pointillés tous les 4 colonnes avant le premier caractère. |
| Minimap | tout le document en miniature (2 px par ligne), **colorée par la syntaxe** ; un clic ou un glissement fait défiler. Mise à jour au plus tous les 150 ms pendant la frappe. |
| Barre de défilement | curseur proportionnel, glissable. |
| Barre d'état | position (« Line 3, Column 7 »), taille de la sélection (« 12 characters selected », « 3 selection regions »), « Tab Size », langage ; les messages brefs s'y affichent aussi. L'icône à gauche affiche/masque la barre latérale. |
| Barre latérale | arborescence du dossier ouvert (**File ▸ Open Folder…**, glisser un dossier, ou `paulwrite dossier`) ; un clic déplie un dossier ou ouvre un fichier ; Ctrl+K puis Ctrl+B l'affiche ou la masque. Son **bord droit se glisse** pour changer sa largeur (120 px au moins, 60 % de la fenêtre au plus) ; une **barre de défilement** apparaît quand tout ne tient pas (la molette marche aussi) ; une **icône** distingue les dossiers et les familles de fichiers (Rust, Python, scripts, pages web, données, documents) ; un fichier créé ou supprimé hors de l'éditeur **apparaît ou disparaît tout seul**. Les listings de dossiers se font sur le worker. |
| Panneau (Ctrl+P, Ctrl+Maj+P…) | liste claire en police proportionnelle, correspondances soulignées, raccourcis à droite, barre de défilement si la liste est longue. |

Le titre de la fenêtre est le chemin complet du fichier, comme Sublime Text.

## Rendu

Le moteur (`paulwrite-render`, `soft.rs` et `soft/chrome.rs`) garde les principes de l'étape 2 :
rendu logiciel dans une DIB, seules les régions modifiées sont redessinées.

- Une **bande** est un rectangle de pixels. Chaque bande est redessinée **entièrement**, toutes
  couches comprises (onglets, rangées, barre latérale, barre d'état, panneau) : elle ne dépend que de
  l'état courant, jamais de ce qui était dessiné avant. Des tests comparent l'image obtenue après des
  suites aléatoires de frappes, défilements, panneaux, onglets et barre latérale avec un redessin
  complet : elles doivent être identiques au pixel près.
- Une frappe ne redessine que la rangée modifiée, la partie gauche de la barre d'état (et, au plus
  tous les 150 ms, la minimap).
- La zone de droite (minimap, barre de défilement) est dessinée **par copie de pixels**, sans
  Direct2D : une image Direct2D de cette taille coûtait 10 fois plus de temps à froid.
- Un défilement déplace les pixels de la zone des rangées seulement ; les autres zones ne bougent pas.

## Couleurs

Mesurées sur la capture de référence (`CHROME_COLORS` et `DEFAULT_KINDS` dans `soft.rs`) :

| Élément | Couleur |
|---|---|
| Éditeur | `#303841` ; texte `#d8dee9` ; curseur `#f9ae58` ; sélection `#4e5a65` |
| Barre d'onglets | `#72787e` ; texte actif `#ffffff` ; croix `#8c9196` |
| Gouttière | numéros `#848b95` ; ligne courante `#4c5863` |
| Barre d'état | fond `#c7cbd1` ; texte `#404040` |
| Panneau | saisie `#ffffff` ; liste `#e3e5e8` ; sélection `#c7cbd1` ; texte `#444b54` |
| Syntaxe | commentaire `#a7adba`, chaîne `#99c794`, nombre `#f9ae58`, mot-clé `#c695c6`, fonction `#6699cc`, opérateur `#f97b58`, balise `#ec5f67` |

La barre latérale n'avait pas de capture de référence : ses couleurs (`#2a3139`, texte `#b7bfc9`,
sélection `#3a444e`) et l'espacement des lignes (24 px) sont **estimés** à partir du thème.

## Langue

L'interface existe en **anglais** (comme Sublime Text) et en **français** : menus, barre d'état,
palette de commandes, messages, boîtes de dialogue, titre « sans titre ».

- **Choix** : `language = "fr"` ou `"en"` dans `config.toml` ; sans réglage (ou `"auto"`), la langue
  d'affichage de Windows (français si c'est le français, anglais sinon).
- **Changement à chaud** : Preferences ▸ Language: English / Français (ou la palette). Il s'applique
  tout de suite (la barre de menus est reconstruite) et est **enregistré** dans `config.toml`, sans
  toucher au reste du fichier.
- **Réalisation** : chaque texte est écrit en deux versions à l'endroit où il sert
  (`lang.pick(en, fr)`, module `i18n`) : pas de fichier de traduction à lire au démarrage. Des tests
  vérifient que chaque commande et chaque entrée de menu est traduite et que les deux langues ont
  les mêmes entrées.

## PHP et modèles `.phtml`

`.php`, `.phtml`, `.php3`–`.php7` et `.phps` sont colorés par la grammaire PHP. Le HTML qui entoure
le code (hors `<?php … ?>` et `<?= … ?>`) l'est par la grammaire HTML : la grammaire PHP repère ces
plages (nœuds `text`) et une seconde analyse HTML, limitée à elles, les colore. Cette seconde
analyse réutilise son arbre d'une modification à l'autre. Mesuré (worker, fichier de 50 Ko / 200 Ko /
1 Mo) : modification + fenêtre colorée en **6 / 25 / 125 ms** (l'analyse complète fait 20 / 93 /
853 ms ; la première version, qui refaisait tout le HTML à chaque fois, coûtait 12 / 68 / 863 ms).
Au-delà de `highlight_max_bytes` (1 Mo), pas de couleurs.

## JavaScript, CSS et autres langages (étape 6)

- **`<script>` et `<style>`** : dans un fichier HTML ou PHP, le JavaScript et le CSS sont colorés par
  leurs propres grammaires. Même mécanisme que pour le HTML d'un fichier PHP : une « couche » est une
  seconde analyse tree-sitter limitée à des plages de la couche du dessus (HTML → JavaScript et CSS ;
  PHP → HTML → JavaScript et CSS). Une couche n'est créée que s'il y a quelque chose à colorer et
  réutilise son arbre d'une modification à l'autre. La liste des `<script>`/`<style>` est tenue à
  jour d'après les modifications et `changed_ranges` de tree-sitter : seules les zones modifiées sont
  relues. Un parcours complet à chaque frappe coûtait 30 à 40 % de plus (mesuré) ; un test
  aléatoire vérifie que la liste incrémentale vaut toujours celle d'un scan complet.
  Mesuré (worker, fichier HTML de 50 / 200 / 1000 Ko, un `<style>` et un `<script>` toutes les deux
  lignes, ce qui est extrême) : modification + fenêtre colorée en **4 / 13 / 85 ms**.
- **Nouveaux langages** : Markdown (`.md`, `.markdown` : titres, listes, code, et en seconde couche
  emphase, liens et code en ligne), YAML (`.yaml`, `.yml`), SQL, XML (`.xml`, `.xsd`, `.xsl`, `.svg`,
  `.csproj`, `.xaml`, `.rss`…) et scripts shell (`.sh`, `.bash`, `.zsh`). Commentaires :
  `#` pour YAML et shell, `<!-- -->` pour XML et Markdown.
- **SQL** (`.sql`) : la grammaire (`tree-sitter-sequel`) est la plus lourde de toutes (32 966 états,
  contre 3 825 pour Rust) et ajoute **15 Mo** à l'exécutable. Gardée, parce que le poids sur disque ne
  coûte rien au démarrage : mesuré en alternant les deux exécutables (12 mesures chacun), la fenêtre
  est visible en **83 ms (médiane) avec comme sans SQL**. Le coût est à l'ouverture d'un `.sql`
  (sur le worker, sans bloquer l'interface) : préparation de la grammaire 45 ms (20 ms pour Rust) puis
  analyse 18 ms pour 20 Ko. Commentaires `--`.
  **`'` dans une chaîne** : la grammaire suit le standard (quote doublée `''`) ; MySQL et MariaDB
  écrivent `'it's'`, et la grammaire fermait alors la chaîne au `'` et colorait tout le reste de
  travers. Un scanner (`sql.rs`) repère les quotes qui suivent un `\` dans une chaîne `'…'` ou
  `"…"` (en ignorant commentaires et `` `…` ``) et l'analyseur lit à leur place un octet neutre de
  même longueur : aucune position ne bouge. Contrepartie : en SQL standard ou PostgreSQL, une chaîne
  qui finit par un antislash (`'C:\dir'`) est lue comme en MySQL. Limite mesurée de la grammaire
  (et non du scanner, dont le coût est négligeable) : elle relit presque tout le fichier à chaque
  modification, soit **55 / 220 / 1090 ms** pour 50 / 200 / 1000 Ko, sur le worker (l'interface n'est
  pas touchée ; un script SQL de quelques Ko se recolore sans délai perceptible).
- Exécutable : 7,5 → 23 Mo (dont 15 pour SQL). Démarrage inchangé (voir ci-dessus).

## Retour à la ligne et repli (étape 6)

Le moteur de rendu ne connaît ni l'un ni l'autre : il dessine des **rangées** indépendantes. C'est
l'éditeur (`view.rs`, `editor.rs`) qui décide quelle partie de quelle ligne occupe chaque rangée.

- **Rangée** = un segment de ligne. Sans retour à la ligne, une rangée montre une fenêtre de la ligne
  (à partir du défilement horizontal, ~4 × la largeur au plus) : une ligne de 2 millions de
  caractères ne coûte pas plus qu'une ligne courte.
- **Découpe** (`wrap.rs`) : police à chasse fixe, donc en colonnes ; on coupe après le dernier espace,
  tiret ou barre qui précède le débordement, sinon au caractère. Jamais dans l'indentation. Les
  espaces de fin de rangée restent sur elle. Calculée **uniquement pour les rangées visibles**.
- **Défilement** par rangées : la vue mémorise (ligne, segment). La barre de défilement et la
  minimap restent exprimées en lignes du document.
- **Repli** (`fold.rs`) : déduit de l'**indentation**, sans analyse syntaxique : une ligne est
  repliable si la prochaine ligne non vide est plus indentée ; la région va jusqu'à la dernière ligne
  plus indentée. La ligne de fermeture (`}`) reste visible. Les régions repliées sont mémorisées en
  octets et suivent les modifications du texte ; une modification qui les chevauche les déplie.
- **Haut / Bas** suivent les rangées affichées (colonne visée conservée) et sautent les lignes
  repliées. Page suivante / précédente aussi.
- **Réglages** : `word_wrap = false` (défaut `true`) et `word_wrap_max_bytes` (défaut 16 Mo) dans
  `config.toml`. Au-delà de cette taille, ou pour une ligne de plus de 200 000 caractères, pas de
  retour à la ligne (voir `docs/benchmarks.md`).
- **Reprise de session** : à la fermeture, les onglets qui ont un fichier (chemin, ligne et colonne du
  curseur, première ligne visible), le dossier du projet et la barre latérale (affichée, largeur) sont
  écrits dans `%APPDATA%\PaulWrite\session` par le worker ; au lancement suivant ils sont rétablis
  (l'onglet actif est lu en premier, les fichiers disparus sont ignorés sans message). Pas de reprise
  ni d'enregistrement quand des fichiers sont donnés en argument, pour une « nouvelle fenêtre »
  (`--new-window`) ou avec `restore_session = false`. Le texte non enregistré n'est pas conservé.
- **Fichiers modifiés hors de l'éditeur** : chaque onglet garde l'empreinte du fichier (date et
  taille) relevée par le worker à la lecture et à l'enregistrement. Au retour sur la fenêtre, le
  worker compare ces empreintes au disque (un `stat` par fichier ouvert, jamais sur le thread UI) ;
  un fichier changé et sans modification de votre part est relu, curseur et vue conservés (l'historique
  d'annulation repart de zéro) ; un fichier changé **et** modifié ici donne une question
  (« recharger et perdre vos modifications ? ») ; répondre non garde votre version sans reposer la
  question pour ce changement. Un fichier supprimé est ignoré.
- **Rouvrir l'onglet fermé** (Ctrl+Maj+T) : les 20 derniers onglets fermés qui avaient un vrai
  fichier sont gardés en mémoire avec leur curseur et leur première ligne visible ; ceux qui sont
  déjà rouverts sont sautés. **Fichiers récents** (menu File ▸ Open Recent, ou la palette) : les 30
  derniers fichiers ouverts à la main (pas ceux d'une reprise de session ni d'un rechargement),
  gardés par le worker dans `%APPDATA%\PaulWrite
ecent` ; les fichiers disparus sont écartés à
  la lecture.
- **Taille du texte et tabulation** : `font_size` (15 px par défaut, 8 à 48) et `tab_size` (4 par
  défaut, 1 à 16) dans `config.toml`. Ctrl+molette ou Ctrl+`+` / `-` / `0` changent la taille en
  direct (le worker l'enregistre) : le moteur de rendu recrée la police du texte et recalcule la
  géométrie, sans toucher aux polices de l'interface. La tabulation règle à la fois les taquets
  d'affichage, le retour à la ligne, la touche Tab (espaces jusqu'au taquet suivant) et Maj+Tab.
  Rien n'est refait au démarrage tant que ces réglages sont ceux par défaut (démarrage mesuré à
  77 ms, médiane).
- **Remplacer dans le projet** (Ctrl+Maj+H) : panneau à deux champs (Entrée dans le premier passe au
  second ; Alt+C / W / R : casse, mot entier, expression régulière avec `$1`). Entrée dans le second
  lance, **sur le worker**, un aperçu qui compte les occurrences et les fichiers sans rien écrire ;
  une boîte demande confirmation avec ces chiffres, puis le remplacement est appliqué. Même
  moteur et mêmes fichiers que la recherche dans le projet (`.gitignore` respecté), fichiers de plus
  de 5 Mo et binaires ignorés ; lecture et écriture comme l'éditeur (encodage, BOM et fins de ligne
  conservés, écriture atomique). Les fichiers **ouverts et modifiés** dans l'éditeur ne sont pas
  touchés (comptés à part) ; les autres fichiers ouverts sont relus aussitôt (détection des
  modifications hors de l'éditeur). Le remplacement se fait sur le disque : Ctrl+Z ne l'annule pas.
- **État Git** (lecture seule) : si le dossier ouvert (ou, sans dossier, celui du fichier courant) est
  dans un dépôt Git, la barre latérale marque chaque fichier (`M` modifié, `A` ajouté, `D` supprimé,
  `R` renommé, `U` non suivi, `!` conflit ; couleurs de la palette : orange, vert, rouge, bleu), un
  point signale les dossiers qui contiennent des changements, les fichiers ignorés (`.gitignore`) sont
  grisés, le libellé des onglets prend la couleur de l'état, et la barre d'état affiche
  `branche ↑avance ↓retard · N modifications`. On interroge le programme `git`
  (`git status --porcelain=v2 --branch -z --ignored`, `--no-optional-locks` pour ne jamais gêner un
  autre outil Git) : aucune dépendance ajoutée. Tout se passe sur **un thread dédié** : les demandes
  qui s'accumulent sont fusionnées, deux exécutions sont espacées d'au moins 300 ms, une exécution est
  abandonnée au bout de 30 s (l'ancien état reste affiché). L'état est rafraîchi à l'ouverture du
  dossier, au retour sur la fenêtre, après un enregistrement et quand le disque change (y compris
  `index` et `HEAD` de `.git` : commit, changement de branche, mais pas ses entrailles). Rien n'est
  demandé sans dossier ni fichier, ni dans les modes de mesure. Mesuré : démarrage 82,8 ms contre
  83,0 ms, frappe p99 6,7 ms contre 6,6 ms (15 mesures, dans le bruit).

## Limites connues

- **Repli** : par indentation seulement (pas de repli des accolades sur une même ligne, des
  commentaires multi-lignes ni des régions Markdown) ; l'état de repli n'est pas conservé à la
  fermeture ; la minimap montre toujours tout le texte ; Fold All est limité aux 100 000 premières
  lignes et prend ~80 ms sur 150 000 lignes (sur le thread de l'interface, une seule fois).
- **Retour à la ligne** : les rangées de continuation ne sont pas indentées ; une tabulation au
  milieu d'une rangée de continuation s'aligne à partir du début de cette rangée ; Début / Fin vont
  au début / à la fin de la **ligne** (pas de la rangée) ; les caractères larges (CJK, emoji) comptent
  pour une colonne ; la bascule n'a pas de coche dans le menu (un message s'affiche dans la barre
  d'état).
- Ctrl+Maj+[ / ] ne fonctionnent pas sur les claviers où `[` demande AltGr (AZERTY) : utiliser le menu
  Edit ou la palette.
- **Minimap** : colorée seulement si la coloration syntaxique est active (fichier de langage connu,
  moins de `highlight_max_bytes`, 1 Mo par défaut) ; grise sinon. Après une frappe, les couleurs
  sont décalées avec le texte puis recalculées 400 ms après la dernière touche.
- **Langue** : anglais ou français, pas d'autre. Les noms des commandes des plugins Lua, les messages
  de rustc, des serveurs de langage et du système (boutons Oui/Non/Annuler, boîtes de fichiers) ne
  sont pas traduits par PaulWrite. Les panneaux déjà ouverts gardent leurs listes dans l'ancienne
  langue jusqu'à leur réouverture.
- Le thème est **fixe** (pas de fichier de thème ni de thème clair).
- La barre de menus est celle de Windows (pas de dessin personnalisé) : son apparence suit le
  thème du système.
- Barre latérale : pas de menu contextuel (renommer, supprimer, nouveau fichier), pas de glisser-déposer ; la
  largeur n'est pas conservée d'une session à l'autre ; les icônes sont des formes simples teintées
  (pas les logos des langages) ; le rafraîchissement ne relit que les dossiers déjà affichés ;
  couleurs et espacement toujours **estimés** (aucune capture de référence).
- Non vérifié à la souris dans une vraie session : clics sur les boutons d'onglets, la barre
  latérale, la gouttière, la minimap (les zones de clic sont testées, pas le chemin complet
  clic → fenêtre).
