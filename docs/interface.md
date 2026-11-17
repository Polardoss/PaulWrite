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
Limites : le JavaScript d'une balise `<script>` et le CSS d'une balise `<style>` ne sont pas colorés
(vrai aussi pour un fichier `.html`) ; au-delà de `highlight_max_bytes` (1 Mo), pas de couleurs.

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
