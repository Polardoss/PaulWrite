# Serveurs de langage (LSP)

PaulWrite parle le *Language Server Protocol* : un serveur externe (rust-analyzer, pylsp…) analyse le
code et renvoie des diagnostics, des complétions, des définitions et des infobulles. Tout se fait hors
du thread d'interface (règle n°1) : le client LSP (`paulwrite-lsp`) tourne sur son propre thread,
chaque serveur sur deux autres (lecture et écriture de ses tuyaux), et l'éditeur n'est averti que par
des événements.

## Activation

**Désactivé par défaut** : un serveur est un programme externe que PaulWrite lance sur votre machine.
Dans `%APPDATA%\PaulWrite\config.toml` :

```toml
lsp = true                          # active les serveurs de langage
lsp_rust = "rust-analyzer"          # commande d'un langage (facultatif : valeurs par défaut ci-dessous)
lsp_python = "pyright-langserver --stdio"
```

Serveurs essayés par défaut (à installer vous-même) :

| Langage | Commande |
|---|---|
| Rust | `rust-analyzer` |
| Python | `pylsp` |
| JavaScript | `typescript-language-server --stdio` |
| JSON / HTML / CSS | `vscode-json-language-server --stdio` (idem `html`, `css`) |
| TOML | `taplo lsp stdio` |

Le serveur d'un langage n'est lancé qu'à l'**ouverture du premier document de ce langage**, jamais au
démarrage (le démarrage reste sous 100 ms). Si la commande est introuvable, un message l'indique une
seule fois par langage ; l'éditeur continue de fonctionner normalement.

## Commandes

| Raccourci | Commande | Effet |
|---|---|---|
| Ctrl+Espace | `lsp.completion` | liste de complétion ; la saisie filtre (fuzzy), Entrée remplace le mot en cours |
| F12 | `lsp.definition` | va à la définition (une seule : ouvre le fichier ; plusieurs : liste) |
| F1 | `lsp.hover` | affiche la documentation sous le curseur |
| Ctrl+Maj+M | `lsp.diagnostics` | liste des erreurs et avertissements du document ; Entrée y va |

Les diagnostics sont aussi soulignés dans le texte par un trait ondulé (rouge : erreur, jaune :
avertissement, bleu : information/suggestion).

## Fonctionnement

- Protocole : JSON-RPC sur stdio avec en-têtes `Content-Length`, sans bibliothèque de types
  (`serde_json` seulement, pour garder la compilation et le binaire légers).
- Positions : le tampon compte en caractères, LSP en unités UTF-16. Les conversions sont dans
  `paulwrite-core` (`lsp_position`, `char_at_lsp_position`) et testées sur des emojis et des accents.
- Synchronisation : le cœur journalise chaque insertion/suppression au format LSP. L'application
  envoie ce journal une fois par image (les frappes d'une même image sont regroupées en une
  notification), en **incrémental** si le serveur le permet, sinon en texte complet (copie O(1) de la
  rope, sérialisée sur le thread du client).
- Requêtes interactives : une seule à la fois ; une réponse qui arrive après une nouvelle demande est
  ignorée.
- Le client répond aux requêtes du serveur (`workspace/configuration`, `client/registerCapability`,
  `window/workDoneProgress/create`…) pour ne pas le bloquer.

## Tests

`cargo test -p paulwrite-lsp` (23 tests) utilise `paulwrite-fake-lsp`, un faux serveur déterministe
fourni avec le dépôt : poignée de main, ordre des messages avant « initialized », synchronisation
incrémentale et complète vérifiée sur des modifications aléatoires, diagnostics, complétion,
définition, survol, serveur qui plante ou qui n'existe pas.

## Limites connues

- **Aucun vrai serveur n'a été essayé** : rust-analyzer & co. ne sont pas installés sur la machine de
  développement. Le protocole est vérifié contre le faux serveur seulement ; attendez-vous à des
  ajustements avec les vrais.
- Les diagnostics ne suivent pas la frappe : ils restent à leur position d'origine jusqu'à ce que le
  serveur en renvoie de nouveaux.
- La complétion et l'infobulle s'affichent dans le panneau (liste), pas dans une fenêtre flottante
  près du curseur.
- Un `\r` isolé comme saut de ligne n'est pas compté de la même façon par ropey et par LSP.
- Pas encore : renommage, actions de code, formatage, symboles, reconnexion automatique d'un serveur
  arrêté.
