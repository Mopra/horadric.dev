"""Bakes the Horadric icon from stone.html, the ray marched rune stone.

Run it after changing the stone: `python crates/horadric-ui/assets/icon/bake.py`.
Needs Microsoft Edge (it renders the page headless, on the GPU) and Pillow.

Writes, beside this file:

- `layers.bin`: the small sizes as two layers, the stone and the light of its
  runes, so `icon.rs` can light them with any colour and the tray can breathe.
- `gold-N.png` and `red-N.png`: the large sizes finished, for the executable,
  the Mac icon and the Dock.
"""

import base64
import io
import os
import re
import subprocess
import sys

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
EDGE = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"

# Must match LAYERED in icon.rs.
LAYERED = [16, 20, 24, 32, 40, 48, 64, 96, 128]
FINISHED = [256, 512, 1024]
# Below this the runes are cut thicker, so the light still reads in the tray.
SIMPLE_BELOW = 24
# Icons fill their square, as the icons beside them in the taskbar do.
def zoom(size):
    return 1.33 if size <= 16 else 1.36


# Small, the stone is lit more, or its dark sides vanish into a dark taskbar
# and only the runes are left to see.
def bright(size):
    return 2.2 if size <= SIMPLE_BELOW else 1.6 if size <= 64 else 1.0


def render(px, light, simple=False, zoom=1.0, bright=1.0):
    """The page rendered at `px` square, premultiplied."""
    url = "file:///" + os.path.join(HERE, "stone.html").replace("\\", "/")
    url += f"?bake=1&px={px}&light={light}&simple={1 if simple else 0}&zoom={zoom}&bright={bright}"
    out = subprocess.run(
        [EDGE, "--headless=new", "--use-angle=d3d11", "--ignore-gpu-blocklist",
         "--virtual-time-budget=20000", "--dump-dom", url],
        capture_output=True, text=True, check=True).stdout
    found = re.search(r"data:image/png;base64,([A-Za-z0-9+/=]+)", out)
    if not found:
        sys.exit(f"no image from the page for {px} {light}")
    im = Image.open(io.BytesIO(base64.b64decode(found.group(1)))).convert("RGBA")
    if im.size != (px, px):
        sys.exit(f"asked for {px}, got {im.size}")
    return im.convert("RGBa")


def shrink(im, size):
    """Box filtered, in premultiplied space, so the edge does not darken."""
    return im.reduce(im.width // size)


def main():
    layers = bytearray()
    for size in LAYERED:
        simple = size <= SIMPLE_BELOW
        stone = shrink(render(size * 4, "none", simple, zoom(size), bright(size)), size)
        lit = shrink(render(size * 4, "white", simple, zoom(size), bright(size)), size)
        s, w = stone.tobytes(), lit.tobytes()
        for i in range(0, len(s), 4):
            sr, sg, sb, sa = s[i:i + 4]
            wr, wg, wb, wa = w[i:i + 4]
            # The rune light is what white light adds; the halo is the alpha
            # it adds beyond the stone.
            # Brighter when small, where a rune is only a pixel or two wide.
            glow = min(255, max(0, (wr - sr + wg - sg + wb - sb) // 3) * (3 if simple else 2) // 2)
            halo = max(0, wa - sa)
            layers += bytes([sr, sg, sb, sa, glow, halo])
        print(f"layers {size}", flush=True)
    with open(os.path.join(HERE, "layers.bin"), "wb") as f:
        f.write(layers)

    for light, name in [("gold", "gold"), ("red", "red")]:
        for size in FINISHED:
            im = shrink(render(size * 2, light, zoom=zoom(size)), size).convert("RGBA")
            im.save(os.path.join(HERE, f"{name}-{size}.png"), optimize=True)
            print(f"{name} {size}", flush=True)


if __name__ == "__main__":
    main()
