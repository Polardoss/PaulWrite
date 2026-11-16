# Rendu : choix et mesures (étape 2)

Statut : **choix provisoire**, à confirmer par le gate de l'étape 2 (latence < 10 ms et démarrage < 100 ms
mesurés sur l'application réelle, pas seulement sur le prototype).

## Décision

**Direct2D + DirectWrite, rendu logiciel dans une DIB mémoire, avec régions sales.**

- Première image dessinée en logiciel (aucun pilote GPU sur le chemin critique du démarrage).
- Ne redessiner que les rangées modifiées ; défilement par copie des pixels déjà dessinés.
- Piste ouverte, non réalisée : initialiser un périphérique D3D11 sur un thread en parallèle et basculer
  sur une swap chain GPU si le défilement plein écran en haute résolution l'exige (voir limites).

## Le prototype

`prototypes/render-compare/` : crate jetable, **hors workspace** (wgpu alourdirait la CI). Scénario
commun : 5000 lignes colorées (3 plages par ligne), 600 images de défilement, 300 frappes simulées par un
autre thread (une toutes les 8 ms), latence mesurée de l'arrivée de l'événement à la fin de la
présentation, démarrage mesuré depuis la création du processus (`GetProcessTimes`).

| Binaire | Contenu |
|---|---|
| `d2d` | winit + Direct2D/DirectWrite sur swap chain DXGI flip (D3D11, GPU) |
| `d2d-sw` | winit + Direct2D logiciel (`ID2D1DCRenderTarget`) dans une DIB ; `DIRTY=1` = régions sales |
| `wgpu` | winit + wgpu (DX12/Vulkan) + atlas de glyphes fontdue |
| `raw-win` | fenêtre Win32 brute, mesure de référence du coût d'affichage |

Scripts : `run.ps1` (comparaison complète), `startup.ps1` (démarrage seul), `dirty.ps1` (complet contre
régions sales). Variables : `VSYNC`, `HIDDEN`, `NOFOCUS`, `QUIET`, `EXIT_AFTER_FIRST`, `DIRTY`, `SIZE`,
`WGPU_BACKEND`, `TRACE`.

Machine : i5-10500, Intel UHD 630 (pilote 31.0.101.2140), 7,8 Go, Windows 11. Machine partagée avec
d'autres processus : les ordres de grandeur comptent, pas les décimales.

## Mesures

### Frappe et défilement (1280x800, sans vsync)

| Variante | Défilement ms/image (p99) | Frappe ms (p99) | Pic RAM |
|---|---|---|---|
| Direct2D GPU | 1,7 (8,8) | 1,2 (3,1) | 41 Mo |
| wgpu DX12 + atlas | 1,9 (6,8) | 1,2 (4,8) | 315 Mo |
| wgpu Vulkan / backend par défaut | plante (accès invalide dans le pilote Intel) | | |
| Direct2D logiciel, redessin complet | 8 à 11 (14 à 31) | 9 à 14 (18 à 32) | 41 Mo |
| **Direct2D logiciel, régions sales** | **1,2 (5,2)** | **1,6 (4,4)** | 41 Mo |

### Grand écran (3840x2160)

| Variante | Défilement ms/image (p99) | Frappe ms (p99) |
|---|---|---|
| Direct2D logiciel, redessin complet | 53 (81) | 100 (136) |
| **Direct2D logiciel, régions sales** | 10 (30) | **1,9 (4,9)** |

### Démarrage (création du processus → fenêtre visible avec la première image)

Médianes de 7 exécutions, machine calme, fenêtre réellement visible.

| Variante | Démarrage |
|---|---|
| Direct2D logiciel, fenêtre normale | 99 ms (81-105) |
| Direct2D logiciel, fenêtre masquée puis affichée | **87 ms (81-97)** |
| Direct2D GPU, fenêtre normale | 181 ms (174-186) |
| Direct2D GPU, fenêtre masquée puis affichée | 173 ms (170-188) |

Décomposition : 25 à 47 ms avant `main` (chargeur Windows) ; création d'une fenêtre ≈ 5 à 10 ms puis
`ShowWindow` ≈ 50 ms (DWM), **quel que soit le framework** (fenêtre Win32 brute mesurée) ;
`D3D11CreateDevice` seul ≈ 150 ms (chargement du pilote Intel) ; wgpu DX12 : instance 15 ms, adaptateur
+58 ms, périphérique +83 ms. Premier lancement d'un exécutable neuf : jusqu'à +330 ms (analyse antivirus).

## Pourquoi ce choix

1. **Démarrage** : seul le rendu logiciel passe sous 100 ms (87 à 99 ms) ; initialiser le GPU coûte à lui
   seul ~150 ms sur cette machine.
2. **Latence de frappe** : 1,6 ms (p99 4,4) avec régions sales, loin des 10 ms visés, même en 4K.
3. **Mémoire** : 41 Mo contre 266 à 315 Mo pour wgpu ; l'objectif « quelques dizaines de Mo » n'est pas
   tenable avec wgpu sans travail important.
4. **Robustesse** : wgpu choisit Vulkan par défaut et plante sur ce pilote ; il faudrait forcer DX12.
5. **Texte** : DirectWrite fournit ClearType, repli de polices, emoji, formes complexes, et l'IME s'y
   raccorde naturellement. L'atlas fontdue du prototype est en niveaux de gris, ASCII seulement.

## Limites connues

- **Piège de performance** : `ID2D1DCRenderTarget::BindDC` et `EndDraw` copient toute la zone liée. Il
  faut lier **uniquement la bande modifiée** (coordonnées relatives à son haut) : lier l'écran entier
  coûtait ~5 ms pour une seule ligne.
- **Défilement en 4K** : 10 ms par image en logiciel (copie de ~33 Mo par image), donc pas à 60 fps
  constants. Si cela gêne, basculer sur le GPU en arrière-plan (piste ci-dessus).
- Le prototype ne gère pas `WM_PAINT` (fenêtre découverte) : l'application réelle doit recopier la DIB
  entière dans ce cas.
- Vsync / latence avec cadencement réaliste non mesurés ; mesure unique avec vsync, artificielle
  (frappes toutes les 8 ms, plus vite que les images).
- Mesures sur une seule machine et un seul pilote graphique.
