# M013 human UAT runbook — 720p/1080p UI polish

**Status: NEEDS-HUMAN — prepared by an autonomous agent; no human PASS is claimed.**

M013 criterion 8 is human-sensory: icon recognizability, visual separation, table art,
tooltip legibility, a visible fade, and vitals spacing. An autonomous agent can prepare
the fixtures, capture reference frames, and prove the executable digests, but it cannot
record a human owner's PASS (MEM140/MEM178/MEM294). This file is a runbook, not a verdict.

## Build and revision

| Field | Value |
| --- | --- |
| Build commands | `cargo build --locked` (debug) and `cargo build --locked --release` (release); both were clean no-ops at capture time on 2026-11-02 |
| Debug binary | `target/debug/opencraft` |
| Debug SHA-256 | `023724c2f99cfb3a071a86daaf90164fef40e1a7233df1afb149be7e3dd0320c` |
| Release binary | `target/release/opencraft` |
| Release SHA-256 | `97b652cb2490bfbd6531df70f47650530ff3c5b09a45072e780aa1948c9eda4e` |
| Texture pack | Default (no `packs/` selection; built-in layers, including crafting-table layers 23/24) |
| Host / display | KDE Plasma (KWin) on Wayland, `WAYLAND_DISPLAY=wayland-0` with XWayland `DISPLAY=:0` |
| Capture tool | `spectacle -b -n -f -o <file>` for fullscreen; `spectacle -b -n -a -o <file>` for the active window |
| Screenshot caveat | `grim` is absent on this host; ImageMagick `import -window root` captures a black Xwayland root (MEM262). Use `spectacle` for evidence. |

The game requests a **1280x720** game surface at launch. On this host KWin reports the live
window frame as **1920x1080 physical** and active-window PNGs are **2050x1210** including
transparent shadow margins. The checklist below inspects the 1280x720 logical layout in
that window; a separate 1920x1080-logical capture remains for the operator if the window
manager permits it.

## Fixture

Use a scratch directory so the real `saves/` and `settings.toml` are never touched:

```sh
UAT_DIR=/tmp/m013-uat
mkdir -p "$UAT_DIR"
cd /home/aleksi/minecraft-clone
cargo build --locked

# Canonical combat witness: populated inventory/hotbar from the real binary.
target/debug/opencraft --new --seed 1 --demo-combat --save "$UAT_DIR/scratch.mcvx"

# Relaunch the witness interactively for the human pass.
target/debug/opencraft --save "$UAT_DIR/scratch.mcvx"
```

The canonical witness is **218 bytes**, SHA-256
`fe8c55ada60ed65507ddb0fa78afefbadf4e2fad1231253c5b318e474a29ad97`. It has a populated
hotbar/inventory (wood sword, bow, arrows, drops) and full vitals. It also holds two
hostile mobs near the player, who can die quickly (a skeleton arrow); respawn or use the
derived mob-free observation copy below for a calmer inventory pass.

Derived observation copy (not the canonical witness): the v7 tail is exactly a 4-byte
mob count plus 2 x 19-byte mob records, so keep the first 176 bytes and append a zero
count. This preserves the player, inventory, hotbar, and vitals while removing the two
mobs.

```sh
python3 - <<'PY'
from pathlib import Path
src = Path('/tmp/m013-uat/scratch.mcvx').read_bytes()
assert len(src) == 218
mob_count_offset = len(src) - 4 - 2 * 19
assert mob_count_offset == 176
assert src[mob_count_offset:mob_count_offset + 4] == (2).to_bytes(4, 'little')
derived = src[:mob_count_offset] + (0).to_bytes(4, 'little')
Path('/tmp/m013-uat/scratch-mobfree.mcvx').write_bytes(derived)
PY

target/debug/opencraft --save "$UAT_DIR/scratch-mobfree.mcvx"
```

The derived copy is **180 bytes**, SHA-256
`e027c7fa5d4d75892887e5eb349ac6473d282d661b74edb91ae497f1e55ed494`. The same two
fixtures are available in the workspace for convenience:

- `.gsd/uat/M013/s07-fixture-combat.mcvx` — 218 bytes
- `.gsd/uat/M013/s07-fixture-combat-mobfree.mcvx` — 180 bytes

## Capture commands

Active window (what the agent references used):

```sh
spectacle -b -n -a -o "$UAT_DIR/live-1280x720.png"
identify "$UAT_DIR/live-1280x720.png"
```

Fullscreen (use this when the owner wants a 1920x1080 capture on a 1920x1080 output):

```sh
spectacle -b -n -f -o "$UAT_DIR/fullscreen-1920x1080.png"
```

To request a larger logical game window, resize it with the window manager (or use the
game's video settings) before capturing. If the compositor restores a larger previous
geometry, resize back to the default or record the actual dimensions you captured. On
this host the default window's active-window capture is 2050x1210 with the game content
at 1920x1080 physical; no separate 1920x1080-logical capture was produced.

The inventory, tooltip, corner/edge flip, result-preview tooltip, selected-name strip,
and fade screenshots require pointer and keyboard input. This autonomous lane cannot
inject input, so those captures remain for the operator.

## Checklist

Record exactly one verdict per item, `PASS` or `FAIL`, in the owner verdict block below.
`NEEDS-HUMAN` is the pre-pass state and must be replaced by the owner.

1. **Inventory icons at 1280x720.** Press `E` and inspect all 36 inventory slots plus the
   nine hotbar slots. Every stack has a recognizable textured icon (blocks as three-face
   cubes, materials/tools as deterministic sprites); empty slots have no icon; no missing
   or magenta icon appears. Suggested evidence: `.gsd/uat/M013/s07-inventory-1280x720.png`.
2. **Inventory icons at 1920x1080.** Resize to 1920x1080 (or capture fullscreen) and repeat
   item 1. Icons stay crisp and correctly placed; nothing is cropped, stretched, or
   overlapping. Suggested evidence: `.gsd/uat/M013/s07-inventory-1920x1080.png`.
3. **Separated hotbar bottom row.** In the inventory panel, the nine hotbar slots are a
   visually separated bottom row below the three storage rows, with a visible gap/divider.
   Slot indices 0..8 still behave as the hotbar: drag, click, split, and stack transfer
   all work exactly as before. Suggested evidence:
   `.gsd/uat/M013/s07-hotbar-separated.png`.
4. **Crafting-table top/side art.** Place a crafting table beside oak planks. The table's
   top shows dedicated workbench/grid art visibly distinct from oak planks, and its sides
   and legs are distinct too. The inventory icon shows the same tabletop/side treatment.
   Suggested evidence: `.gsd/uat/M013/s07-table-art.png`.
5. **Tooltip on an occupied slot.** Hover a stack with a count or durability. A tooltip
   shows the item name and the count/durability detail, sits at a normal position, and
   disappears when the hover ends. Suggested evidence: `.gsd/uat/M013/s07-tooltip.png`.
6. **Tooltip at a corner/edge and result-slot preview.** Hover occupied slots near each
   edge and corner. The tooltip flips away from the edge and clamps fully inside the
   surface. Hover the crafting result slot and confirm the preview tooltip names the
   crafted item. Suggested evidence: `.gsd/uat/M013/s07-tooltip-corner.png`.
7. **Selected-name strip for a damaged tool.** Select a damaged tool (for example an Iron
   Pickaxe at 249/250) from the hotbar. A dark strip above the hotbar shows the item name
   and durability, and does not overlap the vitals. Suggested evidence:
   `.gsd/uat/M013/s07-selected-name.png`.
8. **Strip fades and restarts.** About two seconds after selecting, the strip is gone.
   Select a different item and it reappears; selecting an empty slot draws nothing; the
   strip stays hidden while a panel owns the pointer. Suggested evidence:
   `.gsd/uat/M013/s07-selected-name-faded.png`.
9. **Vitals at 1280x720.** In Survival, with some damage and hunger (and with the head
   submerged for air), hearts and hunger stay on one row with a visible center gap; the
   air row is its own line above hunger; all vitals sit above the hotbar and inside the
   surface; half-hearts and half-pips are half-width. Suggested evidence:
   `.gsd/uat/M013/s07-vitals-1280x720.png`.
10. **Vitals at 1920x1080.** Resize and repeat item 9. Icons, gaps, and rows scale
    proportionally with the wider hotbar; hearts and hunger never overlap, stay above the
    hotbar and inside the surface, and air remains on its own line. Suggested evidence:
    `.gsd/uat/M013/s07-vitals-1920x1080.png`.

## Agent reference captures

These are **agent references, not human observations**. They do not satisfy any checklist
item and must not be copied into the owner verdict as a PASS.

| File | Dimensions | What it shows | Fixture |
| --- | --- | --- | --- |
| `.gsd/uat/M013/s07-agent-vitals-1280x720.png` | 2050 x 1210 PNG (game content 1920 x 1080 physical in the default 1280x720 window, plus transparent window shadow) | Clean mob-free derived save: full heart/hunger row above the hotbar, nine populated hotbar slots with block/item icons, and the survival HUD. | `s07-fixture-combat-mobfree.mcvx` (180 bytes) |
| `.gsd/uat/M013/s07-agent-vitals-1280x720-combat.png` | 2050 x 1210 PNG (same window and scale) | Canonical `--demo-combat` witness about 1.6 s after relaunch: full vitals and populated hotbar with the Zombie/Skeleton scene before the player dies. | `s07-fixture-combat.mcvx` (218 bytes) |

No 1920x1080-logical capture, inventory panel, tooltip, corner-flip, result-preview,
selected-name, or faded-strip capture exists in this lane. Do not infer them from these
references; they are explicitly left for the operator.

Capture commands used for the agent references:

```sh
# Mob-free clean frame (s07-agent-vitals-1280x720.png)
cd /tmp/m013-uat
target/debug/opencraft --save /tmp/m013-uat/scratch-mobfree.mcvx > /tmp/m013-uat/live.log 2>&1 &
sleep 2
spectacle -b -n -a -o /tmp/m013-uat/live-mobfree-1280x720.png

# Canonical combat witness, captured quickly while alive (s07-agent-vitals-1280x720-combat.png)
target/debug/opencraft --save /tmp/m013-uat/scratch.mcvx > /tmp/m013-uat/live-early.log 2>&1 &
sleep 1.6
spectacle -b -n -a -o /tmp/m013-uat/live-early.png
```

Both captures used the display environment `WAYLAND_DISPLAY=wayland-0`,
`XDG_RUNTIME_DIR=/run/user/1000`, and `DISPLAY=:0` (the latter for `spectacle`'s X11
compatibility path). The captures were taken on 2026-11-02.

## Owner verdict

Fill this block only after playing the checklist above. Until the date, operator, and
per-item verdicts are filled, the M013 criterion-8 verdict remains **NEEDS-HUMAN**.

| Field | Value |
| --- | --- |
| Date | |
| Operator | |
| Build SHA-256 (debug) | |
| Build SHA-256 (release) | |
| Display / output / window size | |
| Item 1 — inventory icons 1280x720 | |
| Item 2 — inventory icons 1920x1080 | |
| Item 3 — separated hotbar row | |
| Item 4 — crafting-table art | |
| Item 5 — occupied-slot tooltip | |
| Item 6 — corner/edge and result tooltips | |
| Item 7 — selected-name strip | |
| Item 8 — fade and restart | |
| Item 9 — vitals 1280x720 | |
| Item 10 — vitals 1920x1080 | |
| Verbatim notes / surprises | |
