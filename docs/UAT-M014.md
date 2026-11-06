# M014 human UAT runbook — creator tuning and live content reload

**Status: NEEDS-HUMAN — no owner verdict recorded.**

M014's remaining acceptance surface is human-sensory: whether live tuning feels responsive
across all five gameplay domains, whether block/mob/built-in item art is actually visible,
whether warnings and the F6 overlay are readable, whether the digest notice is clear, and
whether the restart boundary is understandable. Executable seams and smokes can prove the
mechanics, but only an operator can record the experience (MEM140/MEM178/MEM294). This file
is a runbook, not a verdict. S10 records the owner pass; **no agent evidence is promoted to a
human PASS**.

## What this runbook covers

| M014 criterion | Where it is judged here |
| --- | --- |
| 10 — owner human tuning pass across five domains, overlay readability, invalid-edit clarity, digest clarity, restart clarity, live texture visibility | Checklist items 2-10 |
| 11 — no agent evidence promoted to human PASS | The status line, the "Executable evidence" section, and the empty owner verdict block |
| 9 — release has no watcher, overlay, creator flags, or test packs | Checklist item 11 (the executable half is S09's release contract) |

## Build and revision

| Field | Value |
| --- | --- |
| Build commands | `cargo build --locked` (debug) and `cargo build --locked --release` |
| Debug binary | `target/debug/opencraft` |
| Debug SHA-256 | `<fill: sha256sum target/debug/opencraft at run time>` |
| Release binary | `target/release/opencraft` |
| Release SHA-256 | `<fill: sha256sum target/release/opencraft at run time>` |
| Revision under test | `<fill: git rev-parse HEAD>` |
| Pack stack under test | `creator-uat` (the `mymod-ruby` fixture plus live art and a `[builtin_icons]` entry) |
| Config tree under test | A byte copy of the checked-in `config/` |
| Host / display | `<fill: desktop, compositor, Wayland/XWayland, output resolution>` |
| Capture tool | `<fill: e.g. spectacle -b -n -a -o FILE, or the host's screenshot tool>` |

Run every command from the repository checkout unless a step says otherwise. The scratch
directory keeps the real `saves/`, `settings.toml`, `packs/`, and `config/` untouched.

## Fixture

Two working directories are needed: `creator/` selects the pack and config tree, and
`plain/` selects no packs so a frozen v8 save can prove the config notice on its own.

```sh
cd /home/aleksi/minecraft-clone
cargo build --locked
cargo build --locked --release
GAME="$PWD/target/debug/opencraft"
RELEASE="$PWD/target/release/opencraft"
UAT_DIR=/tmp/m014-uat
rm -rf "$UAT_DIR"
mkdir -p "$UAT_DIR/creator" "$UAT_DIR/plain" "$UAT_DIR/evidence"

# 1. The creator config tree lives beside settings.toml in the working directory.
cp -R config "$UAT_DIR/creator/config"
cp -R config "$UAT_DIR/plain/config"

# 2. One pack carries block art, mob art, a built-in item sprite, and the recipe
#    that will be edited. The fixed-name texture files go directly in the pack
#    folder, not under textures/.
mkdir -p "$UAT_DIR/creator/packs/creator-uat"
cp -R tests/fixtures/packs/mymod-ruby/. "$UAT_DIR/creator/packs/creator-uat/"
printf '\n[builtin_icons]\napple = "apple.png"\n' >> "$UAT_DIR/creator/packs/creator-uat/pack.toml"

cat > "$UAT_DIR/creator/settings.toml" <<'EOF'
[packs]
stack = ["creator-uat"]
EOF

# 3. Distinct 16x16 RGBA art for the three live-visible surfaces.
python3 - <<'PY'
import struct
import zlib
from pathlib import Path

def png(path, rgba):
    width = height = 16
    raw = b"".join(b"\x00" + bytes(rgba) * width for _ in range(height))

    def chunk(tag, data):
        body = tag + data
        return (struct.pack(">I", len(data)) + body
                + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF))

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    Path(path).write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )

pack = Path("/tmp/m014-uat/creator/packs/creator-uat")
png(pack / "stone.png", (220, 60, 60, 255))     # block layer 3
png(pack / "pig_body.png", (60, 180, 90, 255))  # mob body part
png(pack / "apple.png", (40, 90, 240, 255))     # built-in item sprite, apple = 1
PY

# 4. The frozen v8 witness for the config-digest notice.
cp tests/fixtures/saves/v8-baseline.mcvx "$UAT_DIR/plain/digest.mcvx"
```

The `creator-uat` manifest is the reference `mymod-ruby` pack (two blocks, two items, a
ruby tier, two tools, two recipes including `mymod:ruby_stew`, and two worldgen rules) plus
the `[builtin_icons]` table appended above. Leave the pack unedited until item 9.

## Preflight

```sh
cd "$UAT_DIR/creator"
"$GAME" --check-config > "$UAT_DIR/evidence/item-01-check-config.txt"
echo "check-config exit=$?"
DISPLAY= WAYLAND_DISPLAY= "$GAME" --validate-pack creator-uat
```

Expected: six `ok: config/<file>: <N> values` lines, the summary prefix
`config: files=6 missing=0 warnings=0 errors=0 keys=85 schema=1 digest=0x`, and exit 0.
`--validate-pack` reports `0 errors, 0 warnings` for `creator-uat` (12 content entries plus
one `builtin_icon apple` entry).

## Checklist

Record exactly one verdict per item, `PASS` or `FAIL`, in the owner verdict block below.
`NEEDS-HUMAN` is the pre-pass state and must be replaced by the owner.

### 1. Debug startup and the creator overlay

From `$UAT_DIR/creator`, launch a real window in Survival. Session A is used for the
Survival and combat rows in item 2 and the invalid-edit pass in item 3:

```sh
"$GAME" --new --survival --save "$UAT_DIR/creator/domains.mcvx"
```

Press **F6**. Expected: a read-only overlay with a header shaped
`CREATOR (dev) digest=0x... schema=1 files=6 pending=0 warnings=0 errors=0`, a legend line
naming `*` as the default marker, a `last:` line, and six `config/<file>` sections listing
each curated value with its file/default marking. F6 again hides it. There are no editing
controls. Suggested evidence: `$UAT_DIR/evidence/item-01-overlay-open.txt` (notes) and
`.gsd/uat/M014/item-01-overlay-1280x720.png`.

### 2. Five-domain live responsiveness

Live rules apply in every mode, but the survival and combat rules only tick in Survival: a
Creative player is deliberately inert (no hunger, air, fall, or melee). Judge the Survival
rows in session A, then quit it and launch session B for the remaining domains. Keep
session B running for items 3-7 and 9-10.

**Session A (Survival, already open)**

| Domain | File | Key | Default → test value | What to look for |
| --- | --- | --- | --- | --- |
| Survival | `config/survival.toml` | `survival.hunger_drain_secs` | 30.0 → 2.0 | The first hunger pip drops within a couple of seconds and the bar visibly drains |
| Combat | `config/combat.toml` | `melee.reach` | 3.0 → 8.0 | A Pig can be punched from about six blocks away; with 3.0 the same swing misses |

Restore both files to their defaults and confirm the behavior returns, then quit session A
(Save and Quit or close the window).

**Session B (Creative, for the texture and recipe pass)**

```sh
cd "$UAT_DIR/creator"
"$GAME" --new --creative --save "$UAT_DIR/creator/recipe.mcvx"
```

| Domain | File | Key | Default → test value | What to look for |
| --- | --- | --- | --- | --- |
| Player movement | `config/player.toml` | `movement.walk_speed` | 4.6 → 8.0 | Walking is clearly faster |
| Player camera | `config/player.toml` | `camera.z_far` | 1200.0 → 512.0 | Distant terrain clips/fades closer |
| World | `config/world.toml` | `clock.day_seconds` | 1200.0 → 120.0 | Sky and clock advance about ten times faster |
| Mining | `config/mining.toml` | `tools.wood_speed_multiplier` | 2 → 16 | With a Wooden Pickaxe granted from the Creative palette (**C**), Stone breaks much faster |
| Render/UI | `config/render.toml` | `ui.selected_name_secs` | 2.0 → 10.0 | The selected-item name strip lingers |

Every edit should apply within about a second after the file settles; the overlay's `last:`
line and the affected section should update, and the game must keep running. Restore each
file to its default afterwards. Suggested evidence: `$UAT_DIR/evidence/item-02-domains.txt`
plus one screenshot per domain.

### 3. Invalid-edit warning clarity

With session B open, set `movement.walk_speed = 0` in `config/player.toml`. Expected:

- The walk speed keeps its last good value; the world stays playable.
- The overlay shows one error for the file and the reason, and the counts move to
  `warnings=... errors=1`.
- One transient in-game message shaped
  `config/player.toml: movement.walk_speed: value 0 is below minimum 0.5`.
- In another terminal, `"$GAME" --check-config` exits 1 and prints exactly
  `error: config/player.toml: movement.walk_speed: value 0 is below minimum 0.5`.

Restore `walk_speed = 4.6`; the error clears and the value applies live. Judge whether the
message is clear enough to fix the edit without reading the source. Suggested evidence:
`$UAT_DIR/evidence/item-03-invalid.txt` and `.gsd/uat/M014/item-03-invalid-edit.png`.

### 4. Block, mob, and built-in item texture visibility

In session B (Creative), confirm all three art surfaces are the fixture colors, not built-in
art:

- **Block:** place or mine Stone. It should be the solid red `stone.png`, not the built-in
  stone texture (layer 3).
- **Mob:** fly in daylight until a Pig is in view (passive mobs spawn on lit grass in both
  modes). Its body should be the solid green `pig_body.png`.
- **Built-in item sprite:** press **C**, search `Apple`, and grant it. The hotbar and
  inventory icon should be the solid blue `apple.png` (`[builtin_icons]` override), not the
  procedural apple.

Judge whether each is actually visible and clearly distinct. Suggested evidence:
`.gsd/uat/M014/item-04-stone.png`, `.gsd/uat/M014/item-04-pig.png`,
`.gsd/uat/M014/item-04-apple-icon.png`.

### 5. Live texture edit and malformed-PNG rejection

With session B still open, rewrite the art in place (different colors, same 16x16 RGBA
form; reuse the `png()` snippet from the fixture with new colors):

```sh
python3 - <<'PY'
import struct, zlib
from pathlib import Path

def png(path, rgba):
    width = height = 16
    raw = b"".join(b"\x00" + bytes(rgba) * width for _ in range(height))
    def chunk(tag, data):
        body = tag + data
        return (struct.pack(">I", len(data)) + body
                + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF))
    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    Path(path).write_bytes(b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b""))

pack = Path("/tmp/m014-uat/creator/packs/creator-uat")
png(pack / "stone.png", (250, 220, 40, 255))
png(pack / "pig_body.png", (200, 40, 220, 255))
png(pack / "apple.png", (40, 220, 200, 255))
PY
```

Expected: within about a second the Stone blocks, the Pig body, and the Apple icon change
color live, and the overlay's `packs:` line reports the applied files. Then write junk bytes
to `stone.png` (`printf 'not a png' > .../stone.png`). Expected: one rejected outcome
naming `packs/creator-uat/stone.png` and a reason, the layer keeps the last good yellow
image, the game stays playable, and no panic or freeze occurs. Suggested evidence:
`.gsd/uat/M014/item-05-live-texture.png` and `.gsd/uat/M014/item-05-rejected.png`.

### 6. F6 overlay readability at 1280x720

Resize the window to 1280x720 (or relaunch at the default size) and open F6 with the overlay
over a bright biome. Expected: the header, legend, `last:` line, `packs:` line, and all six
file sections are legible; no line is clipped at the right edge; long values and reasons
wrap instead of overflowing; the overlay does not hide the value being tuned. Suggested
evidence: `.gsd/uat/M014/item-06-overlay-1280x720.png`.

### 7. F6 overlay readability at 1920x1080

Resize to 1920x1080 (or capture fullscreen) and repeat item 6. Expected: the same content is
legible at the larger surface, lines do not stretch into unreadable spacing, nothing is
cropped, and the overlay still reads as one grouped block per file. Suggested evidence:
`.gsd/uat/M014/item-07-overlay-1920x1080.png`.

### 8. Digest mismatch notice clarity

Use the plain directory so the v8 save has no pack-stack difference:

```sh
cd "$UAT_DIR/plain"
"$GAME" --save "$UAT_DIR/plain/digest.mcvx"
```

Expected on the first launch: the frozen v8 save loads with no config notice (it carries no
config record). Press **F5** to quick save, so the file becomes version 9 with the default
config digest, then quit. Edit `$UAT_DIR/plain/config/player.toml`
`movement.walk_speed = 4.6` to `6.0`, and relaunch the same save. Expected: exactly one
non-blocking notice, `config changed since this world was saved`, shown after the world is
open and playable; movement uses 6.0; the world never blocks or re-shows the notice. Restore
4.6 and relaunch: no config notice. Judge whether the message names the cause clearly enough
to know the save and config disagree. Suggested evidence:
`$UAT_DIR/evidence/item-08-digest-notice.txt` and `.gsd/uat/M014/item-08-digest-notice.png`.

The content notice (`content definitions changed`) is judged in item 10.

### 9. Restart-required clarity

Return to session B (relaunch `"$GAME" --new --creative --save "$UAT_DIR/creator/recipe.mcvx"`
if it was closed). While the world is open, edit **only** the `[[recipes]]`
`mymod:ruby_stew` block's `count = 1` to `count = 2`.
Do not use a naive range or first-match edit: `mymod:ruby_stew` also appears in `[[items]]`,
and `count = 1` also appears in the pickaxe recipe.

```sh
python3 - <<'PY'
from pathlib import Path

path = Path("/tmp/m014-uat/creator/packs/creator-uat/pack.toml")
lines = path.read_text().splitlines()
out, in_block, in_recipe, changed = [], False, False, 0
for line in lines:
    stripped = line.strip()
    if stripped.startswith("[[") and stripped.endswith("]]"):
        in_block = stripped == "[[recipes]]"
        in_recipe = False
    elif in_block and stripped == 'id = "mymod:ruby_stew"':
        in_recipe = True
    elif in_block and in_recipe and stripped.startswith("count = "):
        out.append("count = 2")
        changed += 1
        continue
    out.append(line)
assert changed == 1, f"expected one recipe count edit, changed {changed}"
path.write_text("\n".join(out) + "\n")
PY
```

Expected: within about a second the overlay shows
`restart required: creator-uat: recipe changed; restart to apply`, the `packs:` line carries
the same reason, the Packs screen shows `content changes apply when the world is loaded
again`, and one transient notice says the same. The live registry stays frozen: an inventory
preview of `RR` over `WW` still yields Ruby Stew ×1 in this session, and no texture layer
moves. Judge whether "restart to apply" is unambiguous about what to do. Suggested evidence:
`.gsd/uat/M014/item-09-restart-overlay.png` and `.gsd/uat/M014/item-09-restart-packs.png`.

### 10. Same-save recipe restart

Quit the session with **Save and Quit** (or quick save and close), then relaunch the same
save:

```sh
cd "$UAT_DIR/creator"
"$GAME" --save "$UAT_DIR/creator/recipe.mcvx"
```

Expected: the world opens and shows exactly one `content definitions changed` notice, then
stays playable. Open the inventory 2x2 grid and place two Ruby over two Wheat (`RR` / `WW`):
the preview now shows Ruby Stew ×2, and taking it yields 2. Confirm the world still plays
normally; the notice is consumed once, so a later relaunch under the same pack is silent.
Judge whether the content notice plus the working recipe makes the restart boundary obvious.
Suggested evidence: `$UAT_DIR/evidence/item-10-restart.txt` and
`.gsd/uat/M014/item-10-craft-2.png`.

As an executable cross-check (not a human PASS), the same two-run proof is available
headlessly. It needs an unedited `mymod-ruby` stack, so use a separate scratch cwd:

```sh
mkdir -p "$UAT_DIR/demo/packs"
cp -R tests/fixtures/packs/mymod-ruby "$UAT_DIR/demo/packs/"
printf '[packs]\nstack = ["mymod-ruby"]\n' > "$UAT_DIR/demo/settings.toml"
cd "$UAT_DIR/demo"
"$GAME" --demo-restart --new --save demo-recipe.mcvx   # writes the fixture save
# Edit only the [[recipes]] mymod:ruby_stew count from 1 to 2, then:
"$GAME" --demo-restart --save demo-recipe.mcvx         # content notice, preview=2, crafted=2
```

### 11. Release build has no creator tools and no test packs

From an empty scratch directory:

```sh
mkdir -p "$UAT_DIR/release"
cd "$UAT_DIR/release"

DISPLAY= WAYLAND_DISPLAY= "$RELEASE" --check-config
# Expected: exactly one line, `check-config: dev build only`, exit 1.
DISPLAY= WAYLAND_DISPLAY= "$RELEASE" --creator-trace
# Expected: exactly one line, `creator-trace: dev build only`, exit 1.
test ! -e settings.toml && test ! -e saves && test ! -e packs && test ! -e config
# Expected: exit 0 — a rejected release launch creates nothing.
```

Then launch a bounded release world with no config or packs and press/hold **F6**:

```sh
timeout 5 "$RELEASE" > "$UAT_DIR/evidence/item-11-release-launch.txt" 2>&1
# Expected: exit 124 (the timeout killed a live window), no panic, no `creator:` line,
# and still no packs/ or config/ directory in the scratch cwd.
```

Expected: F6 does nothing in release, there is no overlay, no watcher output, and no
`creator:` trace. Confirm the checkout's `packs/` directory contains no `mymod-ruby`,
`ruby-overlay`, or `uat-sound` fixture folders (pass when `packs/` itself is absent):

```sh
cd /home/aleksi/minecraft-clone
find packs -maxdepth 1 \( -name mymod-ruby -o -name ruby-overlay -o -name uat-sound \) 2>/dev/null
# Expected: no output.
```

Release still loads config and packs for gameplay, and the headless demo seams still work:

```sh
cd "$UAT_DIR/creator"
timeout 5 "$RELEASE" --new --save release-live.mcvx   # expected exit 124, no creator output, no panic
"$RELEASE" --validate-pack creator-uat                # expected 0 errors
cp /home/aleksi/minecraft-clone/tests/fixtures/saves/v8-baseline.mcvx v8copy.mcvx
"$RELEASE" --demo-save-v9 --save v8copy.mcvx          # expected the load/resave/match legs and save_v9: ok
"$RELEASE" --demo-restart --save release-recipe.mcvx  # expected the save leg and restart: ok
```

Expected: the release demo seams print their stable lines and exit 0; no test pack is
created. Judge whether the release build feels like a normal player build. Suggested
evidence: `$UAT_DIR/evidence/item-11-release-launch.txt`.

## Evidence and capture slots

Every item above has a suggested evidence filename. Keep the machine-readable captures in
`$UAT_DIR/evidence/` and the screenshots in the git-ignored `.gsd/uat/M014/` directory
(create it with `mkdir -p .gsd/uat/M014`). Slot names:

| Item | Capture / evidence |
| --- | --- |
| 1 | `$UAT_DIR/evidence/item-01-check-config.txt`, `.gsd/uat/M014/item-01-overlay-1280x720.png` |
| 2 | `$UAT_DIR/evidence/item-02-domains.txt`, one screenshot per domain |
| 3 | `$UAT_DIR/evidence/item-03-invalid.txt`, `.gsd/uat/M014/item-03-invalid-edit.png` |
| 4 | `.gsd/uat/M014/item-04-stone.png`, `item-04-pig.png`, `item-04-apple-icon.png` |
| 5 | `.gsd/uat/M014/item-05-live-texture.png`, `item-05-rejected.png` |
| 6 | `.gsd/uat/M014/item-06-overlay-1280x720.png` |
| 7 | `.gsd/uat/M014/item-07-overlay-1920x1080.png` |
| 8 | `$UAT_DIR/evidence/item-08-digest-notice.txt`, `.gsd/uat/M014/item-08-digest-notice.png` |
| 9 | `.gsd/uat/M014/item-09-restart-overlay.png`, `item-09-restart-packs.png` |
| 10 | `$UAT_DIR/evidence/item-10-restart.txt`, `.gsd/uat/M014/item-10-craft-2.png` |
| 11 | `$UAT_DIR/evidence/item-11-release-launch.txt` |

## Executable evidence (not a human verdict)

The following are useful pre-checks but **must not** be copied into the owner verdict as a
human PASS:

- `bash scripts/creator_smoke.sh` and `SMOKE_PROFILE=release bash scripts/creator_smoke.sh`
  prove the executable legs in isolated temp cwds (config check/apply/reject, live block and
  mob and built-in item textures, restart-required plus same-save restart, v8-to-v9, release
  rejection and bounded launch).
- `cargo test --locked` covers the config registry/loader/watcher, pack reload, save v9, and
  the real-binary CLI contracts.
- The existing slice UAT records under `.gsd/phases/14-creator-tuning-and-live-content-reload/`
  carry the per-slice executable evidence.

## Owner verdict

Fill this block only after playing the checklist above. Until the date, operator, and
per-item verdicts are filled, the M014 human UAT remains **NEEDS-HUMAN**.

| Field | Value |
| --- | --- |
| Date | `<fill>` |
| Operator | `<fill>` |
| Build SHA-256 (debug) | `<fill>` |
| Build SHA-256 (release) | `<fill>` |
| Display / output / window size | `<fill>` |
| Item 1 — debug startup and overlay | `<PASS/FAIL>` |
| Item 2 — five-domain live responsiveness | `<PASS/FAIL>` |
| Item 3 — invalid-edit warning clarity | `<PASS/FAIL>` |
| Item 4 — block/mob/built-in item texture visibility | `<PASS/FAIL>` |
| Item 5 — live texture edit and malformed-PNG rejection | `<PASS/FAIL>` |
| Item 6 — F6 overlay readability 1280x720 | `<PASS/FAIL>` |
| Item 7 — F6 overlay readability 1920x1080 | `<PASS/FAIL>` |
| Item 8 — digest mismatch notice clarity | `<PASS/FAIL>` |
| Item 9 — restart-required clarity | `<PASS/FAIL>` |
| Item 10 — same-save recipe restart | `<PASS/FAIL>` |
| Item 11 — release has no creator tools or test packs | `<PASS/FAIL>` |
| Verbatim notes / surprises | `<fill>` |

## Out of scope

- Authored worldgen prefabs, `.mcvx` capture/import, socket-based or full village
  generation, live recipe/content reload without a restart, live terrain/worldgen tuning, a
  second content tree, and an in-game config editor are explicitly outside M014.
- Live pack icon/logo/tint/sound reload and a brand-new pack folder appearing without a
  Packs toggle/restart are documented limitations, not checklist failures.
