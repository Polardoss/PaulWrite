# Plugins Lua

Un plugin est un script Lua qui ajoute des **commandes** à PaulWrite (visibles dans la palette,
Ctrl+Maj+P, sous l'intitulé « Plugin : … ») et réagit à des **événements** (`open`, `save`, `close`).

## Installer un plugin

Déposer un fichier `nom.lua` (ou un dossier `nom/init.lua`) dans :

```
%APPDATA%\PaulWrite\plugins\
```

Les plugins se chargent après l'affichage de la fenêtre (ils ne comptent pas dans le temps de
démarrage). Après une modification : palette de commandes → « Plugins : recharger ». Les erreurs de
chargement s'affichent dans une boîte de message (nom du plugin, ligne de l'erreur). Des exemples
commentés sont dans [`docs/examples/plugins/`](examples/plugins/).

## Exemple

```lua
paul.command("exemple.date", "Exemple : insérer la date", function()
    paul.insert(os.date("%Y-%m-%d"))
end)

paul.on("save", function(ev)
    paul.message("Enregistré : " .. (ev.path or "sans nom"))
end)
```

## API

| Fonction | Rôle |
|---|---|
| `paul.command(nom, titre, fonction)` | déclare une commande (`nom` stable, unique, différent des noms intégrés) |
| `paul.on(événement, fonction)` | s'abonne à `"open"`, `"save"` ou `"close"` ; la fonction reçoit `ev.name` et `ev.path` |
| `paul.insert(texte)` | insère le texte à chaque curseur (remplace la sélection) |
| `paul.message(texte)` | affiche un message bref (3 s) en haut de la fenêtre |
| `paul.run(nom)` | lance une commande **intégrée** par son nom (`edit.undo`, `file.save`, `cursor.line_end`…) |
| `paul.goto_line(n)` | place le curseur à la ligne `n` (à partir de 1) |
| `paul.path()` | chemin du fichier courant, ou `nil` |
| `paul.selection()` | texte sélectionné (sélections jointes par des sauts de ligne) |
| `paul.cursor()` | ligne et colonne du curseur principal (à partir de 1) : `local l, c = paul.cursor()` |
| `paul.line_count()` | nombre de lignes |
| `paul.line(n)` | texte de la ligne `n` sans fin de ligne, ou `nil` hors du document |
| `paul.text(a, b)` | lignes `a` à `b` (toutes par défaut) jointes par des sauts de ligne (4 Mo au plus) |
| `paul.log(...)` | écrit sur la sortie d'erreur (visible en lançant `paulwrite` depuis un terminal) |

Les noms des commandes intégrées sont ceux de la palette (colonne de droite quand une commande n'a pas
de raccourci) ; la liste complète est dans `crates/paulwrite-app/src/command.rs`.

### Comment un plugin agit sur l'éditeur

Un plugin **ne modifie jamais l'éditeur directement**. Pendant un appel il lit un instantané du
document (copie O(1), le texte n'est pas recopié), et les fonctions `paul.insert`, `paul.message`,
`paul.run` et `paul.goto_line` **enregistrent des actions** que l'application applique ensuite, dans
l'ordre. Conséquence : `paul.run("cursor.line_end")` suivi de `paul.insert(...)` insère bien en fin
de ligne, mais `paul.selection()` lu après un `paul.run(...)` donne toujours la sélection d'*avant* l'appel.

## Limites et sécurité

- **Thread dédié** : les plugins s'exécutent sur leur propre thread ; un plugin lent ou bloqué ne
  ralentit pas l'éditeur (frappe, défilement, affichage restent fluides).
- **Temps et mémoire** : chaque appel (chargement, commande, événement) est interrompu après
  **2 secondes** avec une erreur « délai dépassé » ; la mémoire de la machine virtuelle est plafonnée
  à **64 Mo**. Les autres plugins continuent de fonctionner.
- **Isolation** : chaque plugin a ses propres variables globales (un plugin ne peut pas écraser celles
  d'un autre). Un plugin en erreur n'empêche pas le chargement des autres.
- **Bibliothèques disponibles** : `string`, `table`, `math`, `utf8`, `coroutine`, et `os` réduit à
  `time`, `clock`, `date`, `difftime`. Absents : `io`, `debug`, `package`, `require`, `load`,
  `dofile`, `loadfile`, `collectgarbage`, `os.execute`, `os.exit`, `os.remove`, `os.getenv`…
- **Ce n'est pas une frontière de sécurité.** Un plugin est du code que *vous* installez, comme un
  script Sublime Text ; ces restrictions évitent les accidents (boucle infinie, fichier effacé par
  erreur), pas un plugin malveillant. N'installez pas de plugin dont vous ne faites pas confiance à
  la source.
- `string.upper`, `string.lower` et `string.len` de Lua travaillent sur des **octets** : les
  caractères accentués ne sont pas pris en compte. `utf8.len`, `utf8.codepoint`… existent.

## Pourquoi Lua et pas WASM

|  | Lua (retenu) | WASM |
|---|---|---|
| Poids | ≈ 0,5 Mo ajoutés à l'exécutable | plusieurs Mo (moteur complet) |
| Chargement | 5 plugins en 5 ms | compilation des modules plus longue |
| Écrire un plugin | un fichier texte | une chaîne de compilation par plugin |
| Isolation | moyenne (voir ci-dessus) | forte |

Les règles du projet (démarrage et taille d'abord) ont fait choisir Lua. Un moteur WASM pourrait être
ajouté plus tard derrière la même interface (`Request` / `Response` / `Action`).
