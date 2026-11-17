//! Ressources Windows de l'exécutable : icône et informations de version (propriétés du
//! fichier, « Applications installées »). Le fichier de ressources est produit ici pour y
//! reprendre la version de Cargo.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=resources/paulwrite.ico");
    println!("cargo:rerun-if-changed=build.rs");
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let icon = dir.join("resources").join("paulwrite.ico");
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let mut parts = version
        .split(['.', '-'])
        .map(|p| p.parse::<u16>().unwrap_or(0));
    let numbers = [(); 4].map(|()| parts.next().unwrap_or(0));
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEOS 0x40004
FILETYPE 1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "ProductName", "PaulWrite"
      VALUE "FileDescription", "PaulWrite - fast text editor"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "paulwrite.exe"
      VALUE "InternalName", "paulwrite"
      VALUE "LegalCopyright", "MIT License"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "/"),
        a = numbers[0],
        b = numbers[1],
        c = numbers[2],
        d = numbers[3],
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("paulwrite.rc");
    std::fs::write(&out, rc).expect("écriture du fichier de ressources");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_required()
        .expect("compilation des ressources");
}
