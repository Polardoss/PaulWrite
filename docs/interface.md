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
| Gouttière | numéros de ligne, ligne du curseur en évidence ; un clic sélectionne la ligne. Largeur adaptée au nombre de chiffres. |
| Guides d'indentation | traits pointillés tous les 4 colonnes avant le premier caractère. |
| Minimap | tout le document en miniature (2 px par ligne) ; un clic ou un glissement fait défiler. Mise à jour au plus tous les 150 ms pendant la frappe. |
| Barre de défilement | curseur proportionnel, glissable. |
| Barre d'état | position (« Line 3, Column 7 »), taille de la sélection (« 12 characters selected », « 3 selection regions »), « Tab Size », langage ; les messages brefs s'y affichent aussi. L'icône à gauche affiche/masque la barre latérale. |
| Barre latérale | arborescence du dossier ouvert (**File ▸ Open Folder…**, glisser un dossier, ou `paulwrite dossier`) ; un clic déplie un dossier ou ouvre un fichier ; Ctrl+K puis Ctrl+B l'affiche ou la masque. Les listings de dossiers se font sur le worker. |
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

## Limites connues

- Pas de **repli de code** : les flèches ▼ de la gouttière de Sublime Text ne sont pas dessinées
  (elles seraient factices). L'espace qu'elles occupent est réservé.
- Pas de **retour à la ligne automatique** : les lignes longues sont coupées par le bord de la
  fenêtre (pas de défilement horizontal non plus).
- La minimap n'est **pas colorée** par la syntaxe (texte gris) : la coloration ne couvre que les
  lignes visibles.
- Intitulés des menus, de la barre d'état et « untitled » en **anglais**, comme Sublime Text ;
  la palette de commandes reste en français. Aucune option de langue pour l'instant.
- Le thème est **fixe** (pas de fichier de thème ni de thème clair).
- La barre de menus est celle de Windows (pas de dessin personnalisé) : son apparence suit le
  thème du système.
- Barre latérale : pas de redimensionnement à la souris, pas de barre de défilement (molette
  seulement), pas d'icônes de fichiers, pas de menu contextuel, pas de rafraîchissement automatique
  quand le disque change.
- Non vérifié à la souris dans une vraie session : clics sur les boutons d'onglets, la barre
  latérale, la gouttière, la minimap (les zones de clic sont testées, pas le chemin complet
  clic → fenêtre).
