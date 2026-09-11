#!/usr/bin/env python3
"""Generate fixtures/sample.png, the 160x144 fixture, within Game Boy Color limits.

The scene (a cartoon house, an apple tree with a tire swing, grass, sky,
one cloud, the sun) is drawn on the GBC's 8x8 tile grid and checked
against the hardware's rules before the PNG is written:

- every colour is representable in RGB555 (5 bits per channel);
- each background tile uses at most 4 colours, and all background tiles
  together fit in at most 8 four-colour palettes;
- each sprite is 8x8, uses at most 3 colours plus transparency, all
  sprites fit in 8 three-colour palettes, no scanline crosses more than
  10 sprites, and there are at most 40 of them.

Pure Python, no dependencies; the PNG is written by hand. The output is
this repository's own work and carries its licence (0BSD).

    scripts/gen-sample.py            # writes fixtures/sample.png
    scripts/gen-sample.py OUT.png    # writes elsewhere
"""

import struct
import sys
import zlib
from pathlib import Path

W, H = 160, 144
TILE = 8
MAX_BG_PALETTES = 8
MAX_OBJ_PALETTES = 8
MAX_SPRITES = 40
MAX_SPRITES_PER_LINE = 10


def rgb555(r, g, b):
    """A 5-bit-per-channel colour, expanded to 8 bits the way hardware does."""
    for v in (r, g, b):
        assert 0 <= v <= 31, (r, g, b)
    expand = lambda v: (v << 3) | (v >> 2)
    return (expand(r), expand(g), expand(b))


# Colours (5-bit channels).
SKY = rgb555(13, 21, 31)
WHITE = rgb555(31, 31, 31)
CLOUD_SHADE = rgb555(23, 26, 31)
SUN = rgb555(31, 29, 6)
SUN_RIM = rgb555(31, 20, 2)
GRASS = rgb555(8, 24, 6)
GRASS_DARK = rgb555(4, 16, 4)
GRASS_LIGHT = rgb555(14, 28, 8)
WALL = rgb555(30, 26, 18)
WALL_SHADE = rgb555(23, 18, 11)
DOOR = rgb555(14, 8, 3)
WINDOW = rgb555(18, 26, 31)
ROOF = rgb555(24, 6, 6)
ROOF_DARK = rgb555(15, 3, 3)
TRUNK = rgb555(12, 7, 3)
TRUNK_DARK = rgb555(8, 4, 2)
LEAF = rgb555(5, 18, 5)
LEAF_DARK = rgb555(3, 12, 3)
LEAF_LIGHT = rgb555(10, 24, 7)
ROPE = rgb555(26, 22, 12)
APPLE = rgb555(30, 4, 4)
APPLE_DARK = rgb555(18, 2, 2)
TIRE = rgb555(5, 5, 6)
TIRE_HI = rgb555(11, 11, 12)

TRANSPARENT = None


class Layer:
    def __init__(self, fill):
        self.px = [[fill] * W for _ in range(H)]

    def put(self, x, y, c):
        if 0 <= x < W and 0 <= y < H:
            self.px[y][x] = c

    def rect(self, x0, y0, x1, y1, c):
        for y in range(y0, y1):
            for x in range(x0, x1):
                self.put(x, y, c)

    def disc(self, cx, cy, r, c):
        for y in range(cy - r, cy + r + 1):
            for x in range(cx - r, cx + r + 1):
                if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
                    self.put(x, y, c)

    def ring(self, cx, cy, r_out, r_in, c):
        for y in range(cy - r_out, cy + r_out + 1):
            for x in range(cx - r_out, cx + r_out + 1):
                d = (x - cx) ** 2 + (y - cy) ** 2
                if r_in * r_in < d <= r_out * r_out:
                    self.put(x, y, c)

    def tile_colours(self, tx, ty):
        return {
            self.px[y][x]
            for y in range(ty * TILE, ty * TILE + TILE)
            for x in range(tx * TILE, tx * TILE + TILE)
        }


def draw_background():
    bg = Layer(SKY)

    # Sun, top right: a disc with a rim, all inside tiles that hold only sky.
    bg.disc(140, 13, 9, SUN_RIM)
    bg.disc(140, 13, 7, SUN)

    # One cloud, top left, on tile rows 2-3.
    bg.rect(28, 20, 56, 32, WHITE)
    bg.disc(30, 24, 6, WHITE)
    bg.disc(41, 20, 7, WHITE)
    bg.disc(52, 23, 6, WHITE)
    bg.rect(28, 28, 56, 32, CLOUD_SHADE)
    bg.rect(26, 30, 58, 32, SKY)

    # Ground: tile rows 12-17.
    bg.rect(0, 96, W, H, GRASS)
    bg.rect(0, 96, W, 98, GRASS_DARK)
    for y in range(102, H, 6):
        for x in range((y // 6) % 4, W, 9):
            if y >= 104 or not 96 <= x < 128:  # keep the tree's root tiles to 4 colours
                bg.put(x, y, GRASS_LIGHT)
                bg.put(x + 1, y, GRASS_LIGHT)
    for y in range(105, H, 8):
        for x in range(4 + (y // 8) % 5, W, 11):
            bg.put(x, y, GRASS_DARK)

    # House: walls on tile rows 8-11, columns 2-8 (x 16..72).
    bg.rect(16, 64, 72, 96, WALL)
    bg.rect(16, 64, 18, 96, WALL_SHADE)
    bg.rect(70, 64, 72, 96, WALL_SHADE)
    bg.rect(16, 64, 72, 66, WALL_SHADE)
    # Door on columns 4-5 (x 32..48), rows 9-11.
    bg.rect(34, 74, 46, 96, DOOR)
    bg.rect(34, 74, 46, 76, WALL_SHADE)
    bg.rect(34, 74, 36, 96, WALL_SHADE)
    bg.rect(44, 74, 46, 96, WALL_SHADE)
    bg.rect(42, 85, 44, 87, WALL_SHADE)
    # Window on column 7 (x 56..64), rows 8-9.
    bg.rect(57, 68, 63, 78, WINDOW)
    bg.rect(56, 67, 64, 68, WALL_SHADE)
    bg.rect(56, 78, 64, 79, WALL_SHADE)
    bg.rect(56, 67, 57, 79, WALL_SHADE)
    bg.rect(63, 67, 64, 79, WALL_SHADE)
    bg.rect(59, 68, 61, 78, WALL_SHADE)
    bg.rect(57, 72, 63, 74, WALL_SHADE)
    # Roof: a triangle on tile rows 5-7 (y 40..63), apex above the middle.
    for y in range(40, 64):
        half = (y - 40) * 32 // 24 + 4
        bg.rect(44 - half, y, 44 + half, y + 1, ROOF)
    bg.rect(12, 60, 76, 64, ROOF_DARK)
    for y in range(40, 60):
        half = (y - 40) * 32 // 24 + 4
        bg.put(44 - half, y, ROOF_DARK)
        bg.put(44 + half - 1, y, ROOF_DARK)
    # Chimney on column 7, rows 4-5, inside sky-and-roof tiles.
    bg.rect(58, 34, 64, 50, WALL_SHADE)

    # Canopy: tile rows 3-7 (y 24..63), columns 11-17 (x 88..144).
    bg.rect(96, 32, 136, 64, LEAF)
    bg.disc(100, 40, 10, LEAF)
    bg.disc(116, 32, 12, LEAF)
    bg.disc(132, 40, 10, LEAF)
    bg.disc(100, 54, 9, LEAF)
    bg.disc(132, 54, 9, LEAF)
    # Shaded underside: rounded lobes rather than a flat band.
    bg.disc(100, 58, 9, LEAF_DARK)
    bg.disc(116, 60, 11, LEAF_DARK)
    bg.disc(132, 58, 9, LEAF_DARK)
    bg.rect(94, 58, 138, 64, LEAF_DARK)
    bg.disc(112, 36, 6, LEAF_LIGHT)
    bg.disc(128, 44, 4, LEAF_LIGHT)
    bg.disc(100, 46, 3, LEAF_LIGHT)
    # The canopy ends on its tile row; the lobes above may overshoot it.
    bg.rect(80, 64, 160, 72, SKY)

    # Trunk on columns 13-14 (x 104..120), rows 8-12, drawn after the
    # canopy so its tiles hold only trunk and sky.
    bg.rect(106, 64, 118, 104, TRUNK)
    bg.rect(106, 64, 108, 104, TRUNK_DARK)
    bg.rect(115, 64, 118, 104, TRUNK_DARK)
    bg.rect(104, 100, 120, 104, TRUNK_DARK)
    bg.rect(102, 102, 122, 104, TRUNK)

    # Branch out to the right on tile row 7. The tile where it leaves the
    # canopy holds only dark leaf, sky, and trunk, so it shares the trunk's
    # palette; the rope starts on the next tile row so its tiles hold only
    # rope and sky.
    bg.rect(136, 56, 144, 64, SKY)
    bg.rect(136, 56, 141, 62, LEAF_DARK)
    bg.rect(140, 57, 158, 61, TRUNK)
    bg.rect(149, 61, 153, 64, TRUNK)
    bg.rect(150, 64, 152, 84, ROPE)
    return bg


def sprite(rows, palette):
    """An 8x8 sprite from 8 strings of digits; 0 is transparent."""
    assert len(rows) == 8 and all(len(r) == 8 for r in rows)
    px = [[TRANSPARENT if ch == "0" else palette[int(ch) - 1] for ch in r] for r in rows]
    return px


APPLE_SPRITE = sprite(
    [
        "00020000",
        "00020000",
        "01111100",
        "11111110",
        "11111110",
        "11111110",
        "01111100",
        "00000000",
    ],
    [APPLE, APPLE_DARK],
)

TIRE_SPRITE = sprite(
    [
        "00111100",
        "01122110",
        "11200211",
        "11200211",
        "11200211",
        "11200211",
        "01122110",
        "00111100",
    ],
    [TIRE, TIRE_HI],
)


def draw_sprites():
    """(x, y, pixels) for each sprite, composited over the background."""
    apples = [(94, 40), (110, 44), (124, 50), (104, 56), (118, 30)]
    placed = [(x, y, APPLE_SPRITE) for x, y in apples]
    placed.append((147, 84, TIRE_SPRITE))
    return placed


def composite(bg, sprites):
    out = Layer(SKY)
    out.px = [row[:] for row in bg.px]
    for sx, sy, px in sprites:
        for dy in range(TILE):
            for dx in range(TILE):
                c = px[dy][dx]
                if c is not TRANSPARENT:
                    out.put(sx + dx, sy + dy, c)
    return out


def pack_palettes(colour_sets, size, limit, what):
    """Group per-tile colour sets into at most `limit` palettes of `size`
    colours, or fail. Backtracking, so a grouping is found whenever one
    exists; the inputs are a few dozen sets, which is instant."""
    sets = sorted(colour_sets, key=len, reverse=True)
    for cs in sets:
        assert len(cs) <= size, f"{what}: a tile uses {len(cs)} colours: {sorted(cs)}"

    def place(index, palettes):
        if index == len(sets):
            return palettes
        cs = sets[index]
        if any(cs <= p for p in palettes):
            return place(index + 1, palettes)
        for k, p in enumerate(palettes):
            if len(p | cs) <= size:
                trial = palettes[:k] + [p | cs] + palettes[k + 1 :]
                found = place(index + 1, trial)
                if found is not None:
                    return found
        if len(palettes) < limit:
            return place(index + 1, palettes + [set(cs)])
        return None

    palettes = place(0, [])
    assert palettes is not None, f"{what}: no grouping into {limit} palettes of {size} exists"
    return palettes


def check(bg, sprites):
    all_colours = {c for row in bg.px for c in row}
    for _, _, px in sprites:
        all_colours |= {c for row in px for c in row if c is not TRANSPARENT}
    for r, g, b in all_colours:
        for v in (r, g, b):
            assert (v >> 3) << 3 | (v >> 3) >> 2 == v, f"not RGB555: {(r, g, b)}"

    bg_sets = {frozenset(bg.tile_colours(tx, ty)) for ty in range(H // TILE) for tx in range(W // TILE)}
    bg_pal = pack_palettes(bg_sets, 4, MAX_BG_PALETTES, "background")

    assert len(sprites) <= MAX_SPRITES
    obj_sets = set()
    per_line = [0] * H
    for sx, sy, px in sprites:
        cs = frozenset(c for row in px for c in row if c is not TRANSPARENT)
        obj_sets.add(cs)
        for y in range(sy, min(sy + TILE, H)):
            per_line[y] += 1
    obj_pal = pack_palettes(obj_sets, 3, MAX_OBJ_PALETTES, "sprites")
    assert max(per_line) <= MAX_SPRITES_PER_LINE, f"{max(per_line)} sprites on one scanline"
    return len(bg_pal), len(obj_pal)


def write_png(path, layer):
    raw = b"".join(b"\x00" + bytes(c for px in row for c in px) for row in layer.px)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    Path(path).write_bytes(png)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "fixtures" / "sample.png"
    bg = draw_background()
    sprites = draw_sprites()
    n_bg, n_obj = check(bg, sprites)
    write_png(out, composite(bg, sprites))
    print(f"wrote {out}: {W}x{H}, {n_bg} background palettes, {len(sprites)} sprites in {n_obj} palettes")


if __name__ == "__main__":
    main()
