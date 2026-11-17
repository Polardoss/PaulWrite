# Distribution

## Ce qui est produit

`.\scripts\package.ps1` construit la version `release` et écrit dans `dist\` (ignoré par git) :

| Fichier | Contenu |
|---|---|
| `PaulWrite-<version>-windows-x64.zip` | `paulwrite.exe`, `install.ps1`, `uninstall.ps1`, `LICENSE`, `README.md` |
| `PaulWrite-<version>-windows-x64.zip.sha256` | somme SHA-256 de l'archive |

La version vient de `Cargo.toml` ; le script refuse d'empaqueter un exécutable d'une autre version.
Mesuré : archive de 2,1 Mo, `paulwrite.exe` de 6,0 Mo (la compression par `lto = "fat"` et
`opt-level = 3` du profil `release` est déjà dans l'exécutable).

## L'exécutable

- **Icône** : `scripts/make-icon.py` (Pillow) dessine un carré arrondi aux couleurs du thème portant
  un « P » orange ; il écrit `assets/paulwrite.png` et `crates/paulwrite-app/resources/paulwrite.ico`
  (9 tailles de 16 à 256 px). Dessin original, aucune ressource d'un autre logiciel.
- **Ressources** : `crates/paulwrite-app/build.rs` produit un fichier `.rc` (icône + informations de
  version prises dans `Cargo.toml`) et le compile avec le compilateur de ressources de Windows par
  la crate `embed-resource` (dépendance **de compilation** seulement : aucun effet sur le démarrage ;
  elle ajoute une quinzaine de petites crates à la première compilation). La fenêtre et la barre des
  tâches utilisent la même icône.

## Installation

`install.ps1` installe **pour l'utilisateur courant**, sans droits d'administrateur :

| Élément | Où | Option |
|---|---|---|
| Fichiers | `%LOCALAPPDATA%\Programs\PaulWrite` | `-InstallDir` |
| Raccourci du menu Démarrer | profil de l'utilisateur | `-NoShortcut` pour l'omettre |
| « Open with PaulWrite » (fichiers, dossiers, fond d'un dossier) | `HKCU\Software\Classes` | `-NoContextMenu` |
| Application « PaulWrite » : liste « Ouvrir avec » **et Paramètres ▸ Applications ▸ Applications par défaut** pour 27 extensions courantes (`.txt .md .json .php .phtml .py .rs .html .css .js`…) | `HKCU\Software\Classes\Applications\paulwrite.exe`, `HKCU\Software\PaulWrite\Capabilities`, `RegisteredApplications` | `-NoFileTypes` pour l'omettre ; `-Associate .log,.csv` pour en ajouter |
| `paulwrite` dans les terminaux | PATH de l'utilisateur | `-AddToPath` |
| Entrée « Applications installées » | `HKCU\...\Uninstall\PaulWrite` | — |

`-DryRun` affiche tout sans rien changer. `uninstall.ps1` retire exactement ces éléments (les
réglages `%APPDATA%\PaulWrite` sont conservés).

**Vérifié** (sur cette machine) : paquet construit, installation dans un dossier temporaire avec
des clés de registre de test, démarrage de l'exécutable installé, désinstallation (dossier, clés et
entrées « Ouvrir avec » disparus), puis suppression des clés de test. **Non fait** : installation
réelle dans le profil de l'utilisateur (menu Démarrer, PATH, clés `Software\Classes`) ; ces chemins
sont les mêmes que ceux testés, seules les clés de registre changent.

### Limites

- **Windows ne laisse pas un programme devenir l'éditeur par défaut d'un type de fichier** : après
  l'installation, ouvrir Paramètres ▸ Applications ▸ Applications par défaut ▸ PaulWrite et choisir
  les types de fichier (ou « Ouvrir avec ▸ Choisir une autre application ▸ Toujours » sur un fichier).
  Sans l'installation, un `paulwrite.exe` simplement copié n'est pas déclaré à Windows : il peut
  manquer dans la liste « Ouvrir avec ».
- **L'exécutable n'est pas signé** : SmartScreen peut afficher « éditeur inconnu » au premier
  lancement (Informations complémentaires ▸ Exécuter quand même). Une signature de code demande un
  certificat payant ; non entrepris.
- Windows bloque par défaut les scripts téléchargés : lancer `powershell -ExecutionPolicy Bypass
  -File .\install.ps1` (valable pour cette seule commande).
- Pas d'assistant graphique ni de `.msi`/`Setup.exe` : cela demanderait d'installer un outil
  (Inno Setup, WiX), pas fait sans votre accord.

## Publication d'une version

`.github/workflows/release.yml` se déclenche quand une étiquette `vX.Y.Z` est **poussée** : il lance
les tests, vérifie que l'étiquette égale la version de `Cargo.toml`, construit l'archive avec
`package.ps1` et crée la version GitHub (archive + somme de contrôle, notes générées). **Rien n'est
publié tant qu'aucune étiquette n'est poussée** ; ce workflow n'a pas encore été exécuté.

```bash
git tag v0.1.0
git push origin v0.1.0     # déclenche la publication
```
