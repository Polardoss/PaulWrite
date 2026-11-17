#!/usr/bin/env python3
"""Génère l'icône de PaulWrite : assets/paulwrite.png et
crates/paulwrite-app/resources/paulwrite.ico (compilée dans l'exécutable par build.rs).

Dessin original : un carré arrondi aux couleurs du thème (fond #303841) portant un « P »
orange (la couleur du curseur, #f9ae58) dont la panse est ouverte comme un curseur de texte.
Aucun élément d'un autre logiciel. Dépendance : Pillow.

    python scripts/make-icon.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

BG = (0x30, 0x38, 0x41, 255)
EDGE = (0x4e, 0x5a, 0x65, 255)
ACCENT = (0xf9, 0xae, 0x58, 255)
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
BIG = 1024  # dessin sur-échantillonné, puis réduit


def draw(size_px: int = BIG) -> Image.Image:
    img = Image.new("RGBA", (size_px, size_px), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    s = size_px / 1024
    # carré arrondi
    d.rounded_rectangle((32 * s, 32 * s, 992 * s, 992 * s), radius=210 * s, fill=BG)
    d.rounded_rectangle(
        (32 * s, 32 * s, 992 * s, 992 * s), radius=210 * s, outline=EDGE, width=max(1, int(14 * s))
    )
    # « P » : une hampe pleine et une panse (anneau arrondi à droite), d'un seul tenant
    stem_l, stem_r, top, bottom = 290 * s, 430 * s, 210 * s, 814 * s
    right, bowl_bottom, ring = 740 * s, 620 * s, 140 * s
    right_only = (False, True, True, False)
    d.rounded_rectangle(
        (stem_l, top, right, bowl_bottom), radius=195 * s, fill=ACCENT, corners=right_only
    )
    d.rounded_rectangle(
        (stem_r, top + ring, right - ring, bowl_bottom - ring),
        radius=55 * s,
        fill=BG,
        corners=right_only,
    )
    d.rectangle((stem_l, top, stem_r, bottom), fill=ACCENT)
    return img


def main() -> None:
    out = Path(__file__).resolve().parent.parent / "assets"
    out.mkdir(exist_ok=True)
    master = draw()
    master.resize((256, 256), Image.LANCZOS).save(out / "paulwrite.png")
    res = out.parent / "crates" / "paulwrite-app" / "resources"
    res.mkdir(parents=True, exist_ok=True)
    master.save(res / "paulwrite.ico", sizes=[(n, n) for n in SIZES])
    print("écrit :", out / "paulwrite.png", "et", res / "paulwrite.ico")


if __name__ == "__main__":
    main()
