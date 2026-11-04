"""Bakes the Horadric icon from stone.html, the ray marched rune stone.

Run it after changing the stone: `python crates/horadric-ui/assets/icon/bake.py`,
and `bake.py turning` for the turning stone, `bake.py tumbling` for its
forward tumble.
Needs Microsoft Edge or Google Chrome (it renders the page headless, on the GPU) and Pillow.

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
BROWSERS = [
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
]

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


def render(px, light, simple=False, zoom=1.0, bright=1.0, yaw=None, tumble=None):
    """The page rendered at `px` square, premultiplied."""
    url = "file:///" + os.path.join(HERE, "stone.html").replace("\\", "/")
    url += f"?bake=1&px={px}&light={light}&simple={1 if simple else 0}&zoom={zoom}&bright={bright}"
    if yaw is not None:
        url += f"&yaw={yaw}"
    if tumble is not None:
        url += f"&tumble={tumble}"
    found = None
    # Edge's headless mode sometimes returns an empty page while Edge itself
    # runs, so Chrome is the fallback.
    for browser in BROWSERS:
        if not os.path.exists(browser):
            continue
        out = subprocess.run(
            [browser, "--headless=new", "--use-angle=d3d11", "--ignore-gpu-blocklist",
             "--user-data-dir=" + os.path.join(os.environ.get("TEMP", HERE), "horadric-bake-" + os.path.basename(browser)),
             "--virtual-time-budget=20000", "--dump-dom", url],
            capture_output=True, text=True, check=True).stdout
        found = re.search(r"data:image/png;base64,([A-Za-z0-9+/=]+)", out)
        if found:
            break
    if not found:
        sys.exit(f"no image from the page for {px} {light}")
    im = Image.open(io.BytesIO(base64.b64decode(found.group(1)))).convert("RGBA")
    if im.size != (px, px):
        sys.exit(f"asked for {px}, got {im.size}")
    return im.convert("RGBa")


def shrink(im, size):
    """Box filtered, in premultiplied space, so the edge does not darken."""
    return im.reduce(im.width // size)


# The turning stone of a hireling's portrait: TURN_FRAMES turns of the icon
# stone, TURN_STEP degrees apart from its own angle, TURN_SIZE square. Must
# match turning.rs.
TURN_FRAMES = 72
TURN_STEP = 5
TURN_SIZE = 64
REST_YAW = 52


def frame(yaw, tumble=None):
    """One frame: per pixel, the stone premultiplied as red, green, blue and
    alpha, then its runes' light."""
    stone = shrink(render(TURN_SIZE * 4, "none", zoom=1.3, bright=1.6, yaw=yaw, tumble=tumble), TURN_SIZE)
    lit = shrink(render(TURN_SIZE * 4, "white", zoom=1.3, bright=1.6, yaw=yaw, tumble=tumble), TURN_SIZE)
    out = bytearray()
    s, w = stone.tobytes(), lit.tobytes()
    for j in range(0, len(s), 4):
        sr, sg, sb, sa = s[j:j + 4]
        wr, wg, wb, _ = w[j:j + 4]
        glow = min(255, max(0, (wr - sr + wg - sg + wb - sb) // 3) * 2)
        out += bytes([sr, sg, sb, sa, glow])
    return out


def turning():
    """Writes `turning.bin`, the stone turned round."""
    out = bytearray()
    for i in range(TURN_FRAMES):
        out += frame(REST_YAW + i * TURN_STEP)
        print(f"turn {i + 1}/{TURN_FRAMES}", flush=True)
    with open(os.path.join(HERE, "turning.bin"), "wb") as f:
        f.write(out)


# The tiles' stone tumbling forward once round from each quarter of its
# turn, TUMBLE_STEPS frames apart. Must match turning.rs.
TUMBLE_QUARTERS = 4
TUMBLE_STEPS = 18


def tumbling(only=None):
    """Writes `tumbling.bin`: for each quarter turn, the stone tumbling once
    forward, leaving out the first frame, the stone standing, which
    turning.bin has. `only` renders just that many frames of the first
    quarter, to look at, and writes them as PNGs instead."""
    out = bytearray()
    for q in range(TUMBLE_QUARTERS):
        for i in range(1, TUMBLE_STEPS):
            if only is not None:
                if q > 0 or i > only:
                    return
                im = shrink(render(TURN_SIZE * 4, "white", zoom=1.3, bright=1.6, yaw=REST_YAW, tumble=i * 360 / TUMBLE_STEPS), TURN_SIZE)
                im.convert("RGBA").save(os.path.join(os.environ.get("TEMP", HERE), f"tumble-{i}.png"))
                continue
            out += frame(REST_YAW + q * 90, tumble=i * 360 / TUMBLE_STEPS)
            print(f"tumble {q + 1}.{i}/{TUMBLE_STEPS - 1}", flush=True)
    with open(os.path.join(HERE, "tumbling.bin"), "wb") as f:
        f.write(out)


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
    if sys.argv[1:] == ["turning"]:
        turning()
    elif sys.argv[1:2] == ["tumbling"]:
        tumbling(int(sys.argv[2]) if len(sys.argv) > 2 else None)
    else:
        main()
