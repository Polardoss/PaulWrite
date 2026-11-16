# Étape 2 — notes de reprise (en pause)

> État au moment de la pause : **prototype comparatif de rendu en cours, aucun choix définitif**.
> Rien de l'étape 2 n'est committé ni poussé. Ce document sert à reprendre sans rien refaire.
> À supprimer ou fusionner dans `docs/rendering.md` quand le choix sera documenté.

## Rappel de l'objectif de l'étape 2 (brief)

Prototype comparatif DirectWrite vs wgpu + atlas de glyphes (même scénario : 5000 lignes, défilement,
saisie) → mesure et choix documenté → fenêtre winit, affichage d'un fichier, défilement, curseur,
sélection, clavier, IME, DPI, multi-écrans → outil de latence de bout en bout → démarrage à froid.
**Gate : latence de frappe < 10 ms ET démarrage < 100 ms avant de continuer.**

## Ce qui est fait

### Étapes 0 et 1 (terminées, poussées sur `origin/main`)
Workspace, CI, `paulwrite-core` complet, benchmarks (`docs/benchmarks.md`). Dernier commit poussé :
`95f5f0d docs: resultats des benchmarks de l'etape 1`. Tests : 56 unitaires + 6 proptest.

### Étape 2 : seulement le prototype comparatif (non committé)
Dossier `prototypes/render-compare/` : crate **hors workspace** (son propre `[workspace]`, pour ne pas
alourdir la CI avec wgpu). Versions : winit 0.30.13 (la 0.31 n'existe qu'en bêta), windows 0.62.2,
windows-numerics 0.3, wgpu 30.0.2, fontdue 0.9, pollster, bytemuck.

- `src/lib.rs` : scénario commun (5000 lignes, 3 couleurs par ligne, 600 images de défilement,
  300 frappes simulées par un autre thread, mesure événement → fin de présentation, démarrage depuis la
  *création du processus* via `GetProcessTimes`). Variables : `VSYNC=1`, `TRACE=1`, `NO_DND=1`.
- `src/bin/d2d.rs` : Direct2D + DirectWrite sur swap chain DXGI « flip » (D3D11, GPU).
- `src/bin/d2d_sw.rs` : Direct2D **logiciel** (`ID2D1DCRenderTarget`) dans une DIB mémoire, BitBlt
  vers la fenêtre. Redessine tout à chaque image (pas encore de régions sales).
- `src/bin/wgpu.rs` : wgpu + atlas R8 (fontdue, Consolas lue dans `C:\Windows\Fonts`), un quad
  instancié par glyphe. `WGPU_BACKEND=dx12|vulkan`.
- `src/bin/raw_win.rs` : fenêtre Win32 brute, mesure de référence.
- `run.ps1` : lance chaque variante N fois, résume démarrage / défilement / latence / pic RAM.
  `cd prototypes/render-compare; cargo build --release; .\run.ps1 -Runs 5 [-Vsync]`.

Fichier modifié hors prototype : `.gitignore` (ajout de `prototypes/*/target/`). Rien d'autre.

## Mesures obtenues (i5-10500, Intel UHD 630, pilote 31.0.101.2140, sans vsync, cache chaud)

Machine bruitée (Claude et d'autres processus tournent) : les ordres de grandeur comptent, pas les
décimales. Médianes de 5 exécutions sauf mention contraire.

| Variante | Démarrage (processus → 1re image) | Défilement ms/image (p99) | Latence frappe ms (p99) | Pic RAM |
|---|---|---|---|---|
| Direct2D GPU (D3D11 flip) | 294 ms (224–359) | 1,67 (8,8) | 1,23 (3,1) | 41 Mo |
| Direct2D logiciel, redessin complet | 246 ms (141–380) | 14,5 (33) ; 9–10 sur un run plus calme | 15,6 (36) ; ~10 sur un run plus calme | 41 Mo |
| wgpu DX12 + atlas | 413 ms (336–445) | 1,93 (6,8) | 1,19 (4,8) | 315 Mo |
| wgpu Vulkan / backend par défaut | **plante** : violation d'accès `0xC0000005` dans le pilote Intel pendant `request_device` | — | — | 266 Mo avant plantage |

Avec vsync (un seul run Direct2D GPU) : défilement 16,4 ms/image, latence frappe 28 ms. Chiffre
artificiel : les frappes simulées arrivent toutes les 8 ms, plus vite que les images à 16 ms, donc elles
s'empilent. À refaire avec un cadencement réaliste (frappes espacées de ≥ 50 ms) ou une swap chain
« waitable » (latence 1 image).

### Décomposition du démarrage (le point dur)
- **Avant `main`** : 25–47 ms, et **355 ms au premier lancement d'un exe fraîchement compilé**
  (analyse antivirus). Hors de notre contrôle, sauf signature/réputation de l'exe.
- `EventLoop::build` winit : 3–11 ms.
- `create_window` winit : 55–60 ms, inchangé avec `with_drag_and_drop(false)`. **Ce n'est pas winit** :
  une fenêtre Win32 brute coûte `CreateWindowEx` 4–9 ms puis `ShowWindow` + pompe de messages
  **46–60 ms**. Afficher une fenêtre coûte ~50 ms ici (DWM), quel que soit le framework.
- Direct2D GPU : `D3D11CreateDevice` seul = **145–168 ms** (chargement du pilote Intel) ; le reste de
  l'init D2D + DirectWrite ≈ 15 ms.
- Direct2D logiciel : init ≈ 30–50 ms (création du `DCRenderTarget`), pas de pilote GPU.
- wgpu DX12 : `Instance::new` 15 ms, `request_adapter` +58 ms, `request_device` +83 ms, police +26 ms,
  pipeline +12 ms. wgpu Vulkan : `Instance::new` 143–178 ms.

### Mise à jour (reprise) : analyse de démarrage, machine au calme
Nouveaux outils : `startup.ps1` (démarrage seul, fenêtre ~200 ms à l'écran, sans focus) ; variables
`HIDDEN=1` (fenêtre masquée → 1re image → affichage), `NOFOCUS=1`, `EXIT_AFTER_FIRST=1`, `QUIET=1`
(hors écran ; **ne pas l'utiliser pour le démarrage** : DWM ne compose pas une fenêtre hors écran,
on mesure alors ~83 ms au lieu de ~99 ms, chiffre trompeur). `run.ps1` lance en `QUIET=1` pour ne plus
déranger l'utilisateur (les anciennes mesures de démarrage ci-dessus, 220-300 ms, étaient prises
machine chargée : à ne plus citer).

Médianes de 7 exécutions, machine calme, fenêtre visible à l'écran :

| Variante | fenêtre visible + 1re image (depuis création du processus) |
|---|---|
| Direct2D logiciel, fenêtre normale | 99 ms (81-105) |
| Direct2D logiciel, fenêtre masquée puis affichée | **87 ms (81-97)** |
| Direct2D GPU, fenêtre normale | 181 ms (174-186) |
| Direct2D GPU, fenêtre masquée puis affichée | 173 ms (170-188) |

Conclusion : le seuil < 100 ms est atteignable **en rendu logiciel** pour la 1re image ; le GPU
(`D3D11CreateDevice` ≈ 150 ms) l'interdit tant qu'il est sur le chemin critique. Piste retenue :
1re image en logiciel, périphérique D3D créé sur un thread en parallèle, bascule ensuite ; ou rester
en logiciel si les régions sales tiennent latence et défilement.

### Mise à jour : régions sales en rendu logiciel (`DIRTY=1`, `dirty.ps1`)
Médianes de 3 exécutions, fenêtre visible (donc BitBlt réel vers l'écran), machine calme :

| Taille | Mode | Défilement ms/image (p99) | Frappe ms (p99) |
|---|---|---|---|
| 1280x800 | redessin complet | 8,4 à 11,3 (14 à 31) | 9,4 à 13,6 (18 à 32) |
| 1280x800 | **régions sales** | **1,15 (5,2)** | **1,6 (4,4)** |
| 3840x2160 | redessin complet | 53 (81) | 100 (136) |
| 3840x2160 | **régions sales** | 10,4 (30) | **1,9 (4,9)** |

Point clé : `ID2D1DCRenderTarget::BindDC` + `EndDraw` copient toute la zone liée. Lier l'écran entier
coûtait ~5 ms pour une seule ligne ; **lier uniquement la bande modifiée** (coordonnées relatives au haut
de la bande) ramène la frappe à ~1,6 ms. Le défilement 4K (10 ms/image) reste le point faible du mode
logiciel (copie de pixels de 33 Mo par image) ; la frappe tient, même en 4K.
Limite du prototype : sur `WM_PAINT` (fenêtre découverte), il faudra recopier toute la DIB vers la fenêtre.
Les mesures 4K ont fait occuper les deux écrans de l'utilisateur : ne pas les refaire sans son accord
(voir mémoire `feedback-fenetres-de-test`) ; `dirty.ps1` refuse désormais les tailles > 1600x1000.

## Conclusions provisoires (à confirmer, pas encore documentées dans `docs/rendering.md`)

1. **Latence de frappe** : Direct2D GPU et wgpu sont équivalents (~1 ms, très sous les 10 ms). Le rendu
   logiciel passe sous les 10 ms seulement s'il ne redessine que la région modifiée (à prototyper).
2. **Mémoire** : Direct2D 41 Mo contre 266–315 Mo pour wgpu. Avec wgpu, l'objectif « quelques dizaines
   de Mo au repos » n'est pas tenable sans travail important.
3. **Robustesse** : wgpu plante en Vulkan sur ce pilote et choisit Vulkan par défaut ; il faudrait
   forcer DX12. Direct2D/DirectWrite n'a pas ce problème.
4. **Qualité du texte** : DirectWrite apporte ClearType, repli de polices, emoji, mise en forme
   complexe, et l'IME s'y raccorde naturellement. L'atlas fontdue est en niveaux de gris et ASCII
   seulement (non évalué sur ce point dans le prototype).
5. **Démarrage < 100 ms : aucune variante n'y arrive**. Même sans rendu, `avant main` (25–47 ms) +
   affichage de la fenêtre (~55 ms) ≈ 80–100 ms. Il faut soit des astuces (voir ci-dessous), soit
   **redéfinir ce que « première fenêtre affichée » signifie** (fenêtre visible avec le fond, ou
   première image avec du texte) : **décision à poser à l'utilisateur**.
6. Orientation probable : **Direct2D + DirectWrite**. Piste hybride pour le démarrage : afficher une
   première image en rendu logiciel (rapide) pendant que le périphérique D3D s'initialise sur un
   thread, puis basculer sur la swap chain GPU ; ou rester en logiciel avec régions sales si la
   latence et le défilement tiennent.

## Reste à faire (dans l'ordre)

1. **Compléter l'analyse de démarrage** : créer la fenêtre masquée puis l'afficher après la première
   image ; initialiser D3D sur un thread en parallèle de la fenêtre ; mesurer « fenêtre visible » et
   « première image avec texte » séparément.
2. **Prototyper les régions sales en logiciel** (redessiner seulement la ligne éditée, copier les bits
   pour le défilement) et mesurer latence et défilement, y compris sur grand écran (4K).
3. Relancer `run.ps1` plus de fois et sur machine calme ; refaire le vsync avec un cadencement réaliste.
4. **Écrire `docs/rendering.md`** (choix + chiffres + limites), mettre à jour `docs/benchmarks.md`,
   ajouter `exclude = ["prototypes"]` au `Cargo.toml` racine si nécessaire, committer le prototype.
5. Implémenter pour de vrai : crate `paulwrite-render` (fenêtre winit, texte, curseur, sélection,
   défilement par régions) et `paulwrite-app` (ouverture de fichier sur un worker via
   crossbeam-channel, clavier, IME, DPI, multi-écrans, commandes nommées). Ajouter les dépendances
   avec justification (règle n°7) : `crossbeam-channel`, `winit`, `windows`.
6. Outil de latence de bout en bout (horodatage événement clavier → présentation) et mesure du
   démarrage à froid intégrés à l'application.
7. Gate (latence < 10 ms, démarrage < 100 ms selon la définition retenue), tableau objectif vs mesuré,
   commits atomiques, **STOP** et attente du feu vert avant l'étape 3. Pousser à la fin de l'étape
   validée (le premier push a déjà été autorisé).

## Pièges déjà rencontrés

- **Toujours des chemins absolus** vers la racine du dépôt : une erreur de chemin a créé un dossier
  parasite, supprimé depuis. Un `Set-Location` dans le prototype change le dossier de travail de la
  session.
- **Ne pas gêner l'utilisateur** : pas de fenêtre de test visible grande (4K) ou longue sans accord
  (`QUIET=1` pour le débit, `EXIT_AFTER_FIRST=1` pour le démarrage).
- `cargo` n'est pas dans le PATH par défaut : `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`.
- wgpu 30 : API différente des versions 2x (`InstanceDescriptor::new_without_display_handle()`,
  `apply_limit_buckets`, `color_space`, `Queue::present`, `bind_group_layouts: &[Some(..)]`,
  `immediate_size`, `multiview_mask`). Source lisible dans `~/.cargo/registry/src/*/wgpu-30.0.2`.
- windows 0.62 : `DrawTextLayout` prend un `windows_numerics::Vector2` (crate `windows-numerics 0.3`).
- Les sorties du prototype sont en UTF-8 : décoder avec `StandardOutputEncoding = UTF8` dans PowerShell.
- Windows Defender ralentit le premier lancement d'un exe neuf (jusqu'à +330 ms) : lancer deux fois et
  ne retenir que les exécutions suivantes pour les mesures de démarrage.
