# M010 human UAT runbook — data-only content packs

**Status: PASS — owner runbook completed 2026-11-04. All five items re-tested after the palette, name and tooltip fixes.**

M010's remaining acceptance surface is human-sensory: whether the Creative palette is discoverable and searchable, whether placeholders read clearly, whether the changed-stack warning is understandable, whether a ruby tool feels different from iron, and whether the stew's hold-to-eat feedback reads correctly. Executable digests and smoke scripts can prove the mechanics, but only an operator can record the experience (MEM140/MEM178/MEM294). This file is a runbook, not a verdict.

## Build and revision

| Field | Value |
| --- | --- |
| Build command | `cargo build --locked` (debug) |
| Debug binary | `target/debug/opencraft` |
| Binary SHA-256 | record with `sha256sum target/debug/opencraft` at run time |
| Pack stack under test | `mymod-ruby` then `ruby-overlay` |
| Host / display | record the desktop, compositor, and output resolution used |
| Capture tool | `spectacle -b -n -a -o FILE` for the active window, or the host's screenshot tool |

Run every command below from the repository checkout unless a step says otherwise. The scratch directory keeps the real `saves/`, `settings.toml`, and `packs/` untouched.

## Fixtures

Create one scratch tree with two working directories: one that selects the reference stack and one that selects no packs.

```sh
cd /home/aleksi/minecraft-clone
cargo build --locked
GAME="$PWD/target/debug/opencraft"
UAT_DIR=/tmp/m010-uat
rm -rf "$UAT_DIR"
mkdir -p "$UAT_DIR/with-pack/packs" "$UAT_DIR/no-pack"

cp -R tests/fixtures/packs/mymod-ruby "$UAT_DIR/with-pack/packs/"
cp -R tests/fixtures/packs/ruby-overlay "$UAT_DIR/with-pack/packs/"

cat > "$UAT_DIR/with-pack/settings.toml" <<'EOF'
[packs]
stack = ["mymod-ruby", "ruby-overlay"]
EOF

cat > "$UAT_DIR/no-pack/settings.toml" <<'EOF'
# No [packs] section: the default, empty stack.
EOF
```

Generate the placeholder/notice fixture. It writes a real version 8 save with two authored block edits and one authored item stack, then exits without opening a window:

```sh
cd "$UAT_DIR/with-pack"
"$GAME" --demo-presentation --save "$UAT_DIR/placeholder.mcvx"
```

Create the interactive Creative and Survival worlds:

```sh
cd "$UAT_DIR/with-pack"
"$GAME" --new --creative --seed 4242 --save "$UAT_DIR/ruby-creative.mcvx"
```

```sh
cd "$UAT_DIR/with-pack"
"$GAME" --new --survival --seed 4242 --save "$UAT_DIR/ruby-survival.mcvx"
```

The Creative world is where the palette and tool comparison live. The Survival world is where the stew feedback lives. Save and quit each world normally so the fixture paths keep their state.

## Executable pre-checks

These confirm the fixtures are sound before the human pass. They are **not** human evidence:

```sh
cd "$UAT_DIR/with-pack"
"$GAME" --validate-pack mymod-ruby
"$GAME" --validate-pack ruby-overlay
"$GAME" --demo-palette
cd /home/aleksi/minecraft-clone
bash scripts/modding_smoke.sh
```

Any rejected entry, failed digest leg, or smoke failure blocks the human pass; fix the fixture first.

## Checklist

Record exactly one verdict per item, `PASS` or `FAIL`, in the owner verdict block below. `NEEDS-HUMAN` is the pre-pass state and must be replaced by the owner.

1. **Palette discoverability and search.** In the Creative world, press **C**. The palette opens with a visible entry stock that includes the built-in blocks/items/tools and the two ruby blocks, two ruby items, and two ruby tools. Type `ruby`: the rows narrow to the ruby content, and the stew and both tools are easy to identify by name. Clear the search and type `mymod`: the same rows match by namespace. Click `mymod:ruby_stew`; a full authored stack (up to 16) lands in the hotbar. Type `placeholder` or a name that does not exist: the list empties cleanly instead of showing checker entries. Suggested evidence: `/tmp/m010-uat/palette-ruby.png`, `/tmp/m010-uat/palette-mymod.png`.

2. **Placeholder readability.** Quit the Creative world. From the no-pack working directory, load the placeholder fixture:

   ```sh
   cd "$UAT_DIR/no-pack"
   "$GAME" --save "$UAT_DIR/placeholder.mcvx"
   ```

   The world opens. Where the fixture saved `mymod:ruby_ore` and `mymod:ruby_block`, the blocks render as the checker placeholder, look solid, and cannot be broken or harvested. The saved ruby stack in the hotbar shows the checker icon and remains movable and stackable. Nothing crashes, and the world stays playable. Suggested evidence: `/tmp/m010-uat/placeholders-world.png` and a framed view of the checker blocks.

3. **Changed-stack warning clarity.** On the same no-pack load, one readable notice appears after the world is open. It names the missing pack(s) and reports that the saved content names are unavailable and render as placeholders. The notice does not block movement, saving, or quitting, and it disappears after being shown once. Restore the stack and reload the same save:

   ```sh
   cd "$UAT_DIR/with-pack"
   "$GAME" --save "$UAT_DIR/placeholder.mcvx"
   ```

   The checker blocks become the authored ruby ore and ruby block again, and the hotbar icon returns to the ruby icon. No pack was rewritten. Suggested evidence: `/tmp/m010-uat/changed-stack-notice.png` and `/tmp/m010-uat/placeholders-restored.png`.

4. **Ruby-versus-iron tool feel.** In the Creative world, open the palette, grant `mymod:ruby_ore`, `mymod:ruby_pickaxe`, and a built-in iron pickaxe. Place a ruby ore block, then mine it and a stone block with each pickaxe. The ruby pickaxe breaks the same block noticeably faster (its authored speed is 9, versus iron's 6), and its selected-name strip reports the authored 900-durability maximum. Compare the ruby sword with an iron sword on the same mob: the Creative one-shot rule makes both lethal, so judge the sword by the selected-name strip (ruby tier damage 9, durability 900) rather than by kill count. The difference should read as a deliberate high-tier tool, not a recoloured iron tool. Suggested evidence: `/tmp/m010-uat/ruby-tool-strip.png` and a short timing note.

5. **Ruby stew feedback.** In the Survival world, acquire one `mymod:ruby_stew`. Ruby ore also forms surface patches on plains and forest grass, so mine two ruby by hand; surface-ruin LootChests always include Wheat and Wheat Seeds, and two Wheat are enough. If you only find one Wheat, open a second ruin, or plant the seeds on Farmland and wait one growth stage; Winter pauses growth, so use another seed or world if the surface is snow. In the 2x2 player grid (press **E**), place `RR` over `WW` to craft the stew. Wait until hunger is below full, select the stew, and hold Right Click. A progress bar fills over the authored 0.8 seconds, the stack is consumed only on completion, hunger rises by 7, and releasing or moving cancels without eating. The stew's name, icon, stack limit of 16, and eat speed should read as distinct authored food rather than a renamed apple. Suggested evidence: `/tmp/m010-uat/stew-craft.png` and `/tmp/m010-uat/stew-eating.png`.

## Screenshot guidance

- Capture the active game window rather than the whole desktop when possible, so the HUD, hotbar, and notice are readable.
- The palette search, tool strip, and held-eat progress bar all need pointer or keyboard input. This autonomous lane cannot inject input, so those captures remain for the operator.
- Keep a note of the display resolution and whether the window was resized; the same checklist applies at 1280x720 and at the host's native resolution.
- Save evidence as PNG under `/tmp/m010-uat/`. A screenshot is supporting evidence, not a substitute for the written verdict.

## Owner verdict

Fill this block only after playing the checklist above. Until the date, operator, and per-item verdicts are filled, the M010 human acceptance remains **NEEDS-HUMAN**.

| Field | Value |
| --- | --- |
| Date | 2026-11-04 |
| Operator | aleksi (project owner) |
| Build SHA-256 (debug) | `3bbfdcfd4604725172af8d9641e2b861e40688307534be8674fe8d5e51316e7a` |
| Pack stack | `mymod-ruby`, `ruby-overlay` |
| Display / output / window size | Wayland session (`DISPLAY=:0`, `WAYLAND_DISPLAY=wayland-0`); window not resized during the pass; palette digest at 1280x720 |
| Item 1 — palette discoverability and search | PASS |
| Item 2 — placeholder readability | PASS |
| Item 3 — changed-stack warning clarity | PASS |
| Item 4 — ruby-versus-iron tool feel | PASS |
| Item 5 — ruby stew feedback | PASS |
| Verbatim notes / surprises | Initial palette search showed missing slot icons; root cause was filtered-index double-indexing in `palette_icon_vertices_with`, fixed and re-tested. Pack items read `Modded Item` in the selected-name strip and tooltips; registry-aware display names added and re-tested. The palette had no hover tooltip; added and re-tested. Creative-world ruby ore disappears when the pack is disabled because generated worldgen is not persisted (expected global-stack drift); the saved-edit placeholder fixture retained its edits throughout removal/restore. `mymod:ruby` is a gem and intentionally unplaceable; `mymod:ruby_block` placement passes. |
