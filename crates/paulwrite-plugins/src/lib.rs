//! Plugins Lua.
//!
//! Les plugins sont de petits scripts Lua (`%APPDATA%\PaulWrite\plugins\*.lua` ou
//! `<nom>/init.lua`) qui ajoutent des **commandes** (visibles dans la palette de commandes) et
//! réagissent à des **événements** (`open`, `save`, `close`). Ils agissent sur l'éditeur en
//! renvoyant des [`Action`]s : insérer du texte, afficher un message, lancer une commande
//! intégrée, aller à une ligne.
//!
//! # Architecture
//! - La machine virtuelle Lua vit sur un **thread dédié** ([`spawn`]) : le thread d'interface
//!   n'exécute jamais de code de plugin et ne peut donc pas être bloqué par lui.
//! - Un plugin ne touche pas à l'éditeur : il lit un instantané ([`EditorContext`], copie O(1)
//!   du texte) et renvoie des actions que l'application applique.
//! - Chaque plugin a son propre environnement global (un plugin ne peut pas écraser les variables
//!   d'un autre) ; chaque appel a une limite de temps ([`CALL_BUDGET`]) et la mémoire de la machine
//!   virtuelle est plafonnée ([`MEMORY_LIMIT`]).
//! - Les bibliothèques dangereuses sont retirées (`io`, `debug`, `package`, `require`,
//!   `os.execute`…), mais **ce n'est pas une frontière de sécurité** : un plugin est du code de
//!   l'utilisateur, comme un script Sublime Text. Voir `docs/plugins.md`.

mod host;

pub use host::{
    spawn, Action, CommandInfo, EditorContext, PluginReport, Request, Response, CALL_BUDGET,
    MEMORY_LIMIT,
};
