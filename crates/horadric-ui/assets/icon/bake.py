"""Bakes the Horadric icon from stone.html, the ray marched rune stone.

Run it after changing the stone: `python crates/horadric-ui/assets/icon/bake.py`,
and `bake.py turning` for the turning stone, `bake.py poses` for the tiles'
stone in every pose it can stand in.
Needs Microsoft Edge or Google Chrome (it renders the page headless, on the GPU) and Pillow.

Writes, beside this file:

- `layers.bin`: the small sizes as two layers, the stone and the light of its
  runes, so `icon.rs` can light them with any colour and the tray can breathe.
- `gold-N.png` and `red-N.png`: the large sizes finished, for the executable,
  the Mac icon and the Dock.
"""

import base64
import io
import math
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


def render(px, light, simple=False, zoom=1.0, bright=1.0, yaw=None, poses=None, cols=1):
    """The page rendered at `px` square, premultiplied, or with `poses` an
    atlas of them, `cols` square, each (turn, pose) as POSES says."""
    url = "file:///" + os.path.join(HERE, "stone.html").replace("\\", "/")
    url += f"?bake=1&px={px}&light={light}&simple={1 if simple else 0}&zoom={zoom}&bright={bright}"
    if yaw is not None:
        url += f"&yaw={yaw}"
    if poses is not None:
        url += f"&cols={cols}&poses=" + ";".join(
            ",".join(f"{v:.6g}" for v in [t, *(x for row in m for x in row)]) for t, m in poses)
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
    if im.size != (px * cols, px * cols):
        sys.exit(f"asked for {px * cols}, got {im.size}")
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


def frame_bytes(stone, lit):
    """Per pixel, the stone premultiplied as red, green, blue and alpha,
    then its runes' light."""
    out = bytearray()
    s, w = stone.tobytes(), lit.tobytes()
    for j in range(0, len(s), 4):
        sr, sg, sb, sa = s[j:j + 4]
        wr, wg, wb, _ = w[j:j + 4]
        glow = min(255, max(0, (wr - sr + wg - sg + wb - sb) // 3) * 2)
        out += bytes([sr, sg, sb, sa, glow])
    return out


def frame(yaw):
    stone = shrink(render(TURN_SIZE * 4, "none", zoom=1.3, bright=1.6, yaw=yaw), TURN_SIZE)
    lit = shrink(render(TURN_SIZE * 4, "white", zoom=1.3, bright=1.6, yaw=yaw), TURN_SIZE)
    return frame_bytes(stone, lit)


def turning():
    """Writes `turning.bin`, the stone turned round."""
    out = bytearray()
    for i in range(TURN_FRAMES):
        out += frame(REST_YAW + i * TURN_STEP)
        print(f"turn {i + 1}/{TURN_FRAMES}", flush=True)
    with open(os.path.join(HERE, "turning.bin"), "wb") as f:
        f.write(out)


# The tiles' stone stands in any of a cube's 24 poses: it turns aside about
# the upright and rolls away about the level axis nearest the view's right,
# a quarter at a time. Every pose is one of these six turned about the
# upright, by which face is up, and one of the other six rolled, by which
# face is on that axis. Each matrix takes the stone's own space to the
# world's, row by row. Must match tilecube.rs.
SPUN = [
    [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[1, 0, 0], [0, -1, 0], [0, 0, -1]],
    [[1, 0, 0], [0, 0, 1], [0, -1, 0]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 1, 0], [-1, 0, 0], [0, 0, 1]],
]
ROLLED = [
    [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
    [[0, 0, 1], [0, 1, 0], [-1, 0, 0]],
    [[-1, 0, 0], [0, 1, 0], [0, 0, -1]],
    [[0, 0, -1], [0, 1, 0], [1, 0, 0]],
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[1, 0, 0], [0, 0, 1], [0, -1, 0]],
]
# A roll's frames, ROLL_STEP degrees apart: a roll is quick, so they can be
# further apart than a spin's. Must match turning.rs.
ROLL_STEP = 10
ATLAS = 8


def roll_by(deg, m):
    """`m` rolled away `deg` degrees: about the world's z, the level axis
    nearest the view's right at REST_YAW."""
    a = math.radians(deg)
    c, s = math.cos(a), math.sin(a)
    rz = [[c, -s, 0], [s, c, 0], [0, 0, 1]]
    return [[sum(rz[i][k] * m[k][j] for k in range(3)) for j in range(3)] for i in range(3)]


def poses(only=None):
    """Writes `poses.bin`: the turn of each pose group but the first, which
    turning.bin has, then each roll group's whole roll. `only` renders that
    many frames of the first roll as one PNG to look at instead."""
    frames = [(REST_YAW + k * TURN_STEP, m) for m in SPUN[1:] for k in range(TURN_FRAMES)]
    rolls = [(REST_YAW, roll_by(k * ROLL_STEP, m)) for m in ROLLED for k in range(360 // ROLL_STEP)]
    if only is not None:
        im = render(TURN_SIZE * 2, "white", zoom=1.3, bright=1.6, poses=rolls[:only], cols=math.ceil(math.sqrt(only)))
        im.convert("RGBA").save(os.path.join(os.environ.get("TEMP", HERE), "rolls.png"))
        return
    frames += rolls
    out = bytearray()
    per = ATLAS * ATLAS
    big = TURN_SIZE * 4
    for start in range(0, len(frames), per):
        batch = frames[start:start + per]
        layers = [render(big, light, zoom=1.3, bright=1.6, poses=batch, cols=ATLAS) for light in ("none", "white")]
        for i in range(len(batch)):
            x, y = (i % ATLAS) * big, (i // ATLAS) * big
            stone, lit = (shrink(l.crop((x, y, x + big, y + big)), TURN_SIZE) for l in layers)
            out += frame_bytes(stone, lit)
        print(f"poses {start + len(batch)}/{len(frames)}", flush=True)
    with open(os.path.join(HERE, "poses.bin"), "wb") as f:
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
    elif sys.argv[1:2] == ["poses"]:
        poses(int(sys.argv[2]) if len(sys.argv) > 2 else None)
    else:
        main()
