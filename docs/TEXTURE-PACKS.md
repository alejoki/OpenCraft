# Texture Packs

A texture pack replaces the game's block textures and can also supply a custom title logo and a menu tint. You do not need to write Rust, compile the engine, or know how the renderer works. If you can make PNG images and edit a small text file, you can build a pack.

## Quickstart

1. Find the folder that contains the game's `settings.toml` (the game's working directory).
2. Create a folder named `packs/` next to `settings.toml` — the game does not ship one and never creates it for you — and inside it a folder named `my-first-pack/`.
3. Save the file below as `pack.toml` inside `packs/my-first-pack/`.
4. Launch the game, open **Settings**, and cycle the **Texture Pack** row until it shows your pack.

```toml
name = "My First Pack"
description = "A minimal pack that tints the menu"

[menu]
overlay_tint = [24, 32, 48, 96]
```

That pack is valid on its own: with no image files it simply keeps every built-in block texture and adds the darker menu tint. Add texture files from the sections below to change what you see.

## Where packs live

Packs are folders under `packs/`, relative to the game's working directory — the same place the game keeps `saves/` and `settings.toml`.

- The game does not ship a `packs/` folder and never creates one itself. If it does not exist yet, you create it in the game's working directory, and pack folders go directly inside it.
- The pack folder name is the pack's fallback display name in the Settings menu when `pack.toml` does not set `name`.
- Every pack should include a `pack.toml`. A folder with no manifest still appears, but it loads with default values and shows a warning naming `pack.toml`.
- The game only reads packs. It never writes files inside `packs/`.
- The Default entry always exists and is the built-in procedural texture set.

Packs can be stacked in the Settings menu, and a stack applies later-pack-wins per authored file and field: a later pack that ships one PNG replaces only that layer, while earlier packs keep every other layer, logo, tint, description, and sound they authored.

The repository ships three example packs as test fixtures under `tests/fixtures/packs/`: `mymod-ruby` (a complete Format v2 content pack), `ruby-overlay` (a texture-only overlay), and `uat-sound` (the sound fixture). Copy the folders you want into your own `packs/` directory. These are examples and test fixtures, not part of a normal install; the game never creates `packs/`.

## pack.toml reference

A pack folder works without `pack.toml` — a missing or broken manifest never blocks launch — but you want one: it carries the pack name, description, and presentation settings.

- `name` — a string shown in the Settings menu. Falls back to the pack folder name.
- `description` — a string describing the pack.
- `[menu]` — a table for menu presentation.
  - `logo` — a PNG file name inside the pack folder, for example `logo.png`.
  - `overlay_tint` — exactly four integers from 0 to 255, in RGBA order. The colour dims the panorama behind the menu; the fourth value is opacity.

Rules that apply to every field:

- A field that is present but invalid is ignored on its own — the rest of the manifest still works.
- Unknown keys, unknown tables, and unknown files are ignored silently, so future versions can add features without breaking older ones.
- A missing or unreadable `pack.toml` uses built-in default values and shows a warning that names the pack.

## Format v2

Set `format_version = 2` to add blocks, items, custom tool tiers and tools, shaped recipes, per-field patches over built-in content, and declarative worldgen from the same `pack.toml`. Version 1 packs keep working unchanged: without `format_version = 2`, a content array is ignored with one warning while the pack's textures and sounds still load. A `format_version` newer than 2 is ignored the same way, with one warning.

```toml
format_version = 2
name = "Ruby Mod"
version = "1.0.0"

[[blocks]]
id = "mymod:ruby_ore"
name = "Ruby Ore"
textures = { all = "textures/ruby_ore.png" }
hardness = 3.0
drop = "mymod:ruby"
tool = "pickaxe"
stack = 64

[[items]]
id = "mymod:ruby_stew"
name = "Ruby Stew"
icon = "textures/ruby_stew.png"
stack = 16
nutrition = 7
eat_seconds = 0.8

[[tiers]]
id = "mymod:ruby"
name = "Ruby"
speed = 9.0
durability = 900
damage = 9

[[tools]]
id = "mymod:ruby_pickaxe"
name = "Ruby Pickaxe"
kind = "pickaxe"
tier = "mymod:ruby"
icon = "textures/ruby_pickaxe.png"

[[recipes]]
id = "mymod:ruby_pickaxe"
pattern = ["RRR", " S ", " S "]
key = { R = "mymod:ruby", S = "stick" }
output = "mymod:ruby_pickaxe"
count = 1

[[block_patches]]
id = "stone"
name = "Hard Stone"
hardness = 3.5
stack = 64

[[recipe_patches]]
id = "shortbow"
enabled = false

[[worldgen]]
type = "ore"
block = "mymod:ruby_ore"
replace = "stone"
min_y = 4
max_y = 40
vein = 4
attempts = 8
```

A block needs `id`, a `textures` table that resolves one image for every face, and a finite `hardness` from 0 to 3600. `textures.all` is the fallback for every face; `textures.side` overrides the four side faces, and `textures.top` and `textures.bottom` override those faces individually. `name` is optional and a readable display name is derived from the id when it is absent. `drop` names the item produced on break and defaults to the block itself; `tool` (`pickaxe`, `axe`, `shovel`, `hoe`) marks the block as efficiently broken by that tool kind; `stack` sets the block item's maximum stack from 1 to 999 (default 999).

An item needs `id` and `icon`. `stack` defaults to 64; `nutrition` from 1 to 20 turns the item into food, and an optional `eat_seconds` (0.1 to 10, default 1.2) sets how long the hold-to-eat gesture takes. A tier needs `id`, `speed`, `durability` and `damage`; the tool's durability comes from the tier. A tool needs `kind` (`pickaxe`, `axe`, `shovel`, `hoe` or `sword`), `tier` and `icon`; `bow` is not authorable in this format. `name` is optional on items, tiers and tools.

A recipe needs a 1-to-4-row `pattern` of 1-to-4 characters, a `key` mapping each pattern character to an item, an `output` and a `count` from 1 to 999. A `[[block_patches]]` entry changes one built-in block without redefining it: `id` is the built-in short name (`stone`, `iron_ore`, ...), and the entry must set at least one of `name`, `hardness`, `drop`, `tool` or `stack`. A patch can only set fields; it cannot clear a built-in value, create a block, change a block's identity, or target a pack-defined block. A `[[recipe_patches]]` entry takes a built-in recipe short name in `id` and an `enabled` boolean; `enabled = false` removes that recipe from crafting while the pack is active, and a later patch can turn it back on.

Worldgen entries use `type = "ore"` (with `vein` 1 to 8 and `attempts` 1 to 32) or `type = "patch"` (with a non-empty `biomes` list of `plains`, `forest`, `mountains` or `ocean`, and a `chance` from 0 to 1). Both kinds need `block`, `replace`, and `min_y`/`max_y` within -64 to 128 with `min_y < max_y`. The schema is described here; [Declarative worldgen](#declarative-worldgen) explains what those rules do to a world.

Entry rules:

- New ids are namespaced (`mymod:ruby_ore`) with lowercase letters, digits and `_`; the `opencraft`, `builtin` and `minecraft` namespaces are reserved. Blocks, items and tools share one namespace; tiers and recipes are independent. Content ids are limited to 255 bytes.
- References (`drop`, recipe `key`/`output`, tool `tier`, worldgen `block`/`replace`, patch `drop`) must name a built-in or an entry in the same pack. Cross-pack references are not supported in this format.
- One bad entry never fails the pack: a missing or wrong field, malformed or duplicate id, unknown reference, or over-limit array rejects only that entry and reports a diagnostic while its valid siblings load. Unknown keys are ignored.
- A duplicate id inside one pack keeps the first definition and rejects the later one. The same id authored by a later pack in the stack replaces the earlier definition as a whole and reports a warning; fields are never merged across two definitions.
- `[[block_patches]]` and `[[recipe_patches]]` apply in stack order, and a present field wins over the same field from an earlier patch. An absent field leaves the earlier or built-in value alone.
- A later pack's authored file wins for that layer only. Earlier packs keep every other authored layer, logo, tint, description and sound, so stacking a texture-only overlay changes exactly the files it authors.
- Asset paths are relative to the pack folder. Absolute paths, `.`/`..` components and symlink escapes are rejected, each file is limited to 1 MiB, images must be square and 16 to 256 pixels per side, and a pack may load at most 8 MiB of raw asset bytes.

Pack content ids allocate deterministically: blocks take dense ids from 23 in sorted id order after the 23 built-in blocks, items and tools from 0, tiers after the three built-ins, and recipes after the 22 built-ins. Block face layers append from texture-array layer 48 (at most 160 pack layers) and item/tool icons from layer 32 (at most 192 icons), deduplicated per pack folder and authored path; patch entries allocate no ids. A registry digest (FNV-1a 64) covers pack identities and every definition field so a validation run and a game load agree, but it deliberately excludes image bytes: recolouring an existing PNG does not change content identity.

### Built-in item sprite overrides

A `format_version = 2` manifest can replace the built-in material and tool icons with its own art through one `[builtin_icons]` table. The table maps an unqualified built-in short name to a pack-relative PNG:

```toml
[builtin_icons]
apple = "textures/apple.png"
wood_pickaxe = "textures/pickaxe.png"
wood_bow = "textures/bow.png"
```

Keys are the built-in item sprite names: the 11 materials (`stick`, `apple`, `wheat`, `wheat_seeds`, `bread`, `leather`, `raw_porkchop`, `raw_beef`, `rotten_flesh`, `bone`, `arrow`) and the 16 tools (`wood_`/`stone_`/`iron_` pickaxe, axe, shovel, hoe and sword, plus `wood_bow`). Blocks, namespaced ids, and unknown names are rejected with a `builtin_icon <name>` error. Values follow the same asset rules as every other PNG: relative to the pack folder with no absolute or `.`/`..` components, at most 1 MiB per file, and square at 16 to 256 pixels per side. One pack may author at most 256 entries.

Overrides merge per name: a later pack's loaded image wins for that name, while earlier packs keep every other name. A missing or malformed PNG contributes nothing, so the lower pack's override — or the built-in procedural sprite when no pack authors one — stays visible. The table is presentation-only: it is excluded from the content digest, registry allocation, pack layer/icon counts, and restart identity. Adding, removing, or repointing an override is therefore a live visual edit, not a content change.

## Declarative worldgen

`[[worldgen]]` entries in a `format_version = 2` pack turn authored blocks into real terrain in newly generated chunks. The fields are part of the Format v2 schema described above; this section describes what they do to a world.

```toml
[[worldgen]]
type = "ore"
block = "mymod:ruby_ore"
replace = "stone"
min_y = 4
max_y = 40
vein = 4
attempts = 8

[[worldgen]]
type = "patch"
block = "mymod:ruby_ore"
replace = "grass"
biomes = ["plains", "forest"]
min_y = 40
max_y = 110
chance = 0.02
```

**Applied order.** A chunk is generated in this order, all before it is cached or streamed: built-in terrain, caves and trees; built-in coal and iron; pack ore veins; trees and ruins; pack surface patches. Every rule in the selected stack runs, in pack order, and each rule is independent of the others.

**Ore rules** (`type = "ore"`) replace the `replace` block only where the cell still holds that block, strictly inside the stone band (above bedrock and at least four blocks below that column's surface) and inside `min_y..=max_y`. Each vein starts with up to `vein` cells and grows through in-chunk neighbours; up to `attempts` starts are tried per chunk. An ore rule can never punch through the surface.

**Patch rules** (`type = "patch"`) replace only the column's own surface cell, only in the authored `biomes`, only when the surface height `h` is inside `min_y..=max_y` and the surface still holds `replace`, and then only with probability `chance` per column. Trees, leaves, ruin shells and loot chests sit at or above the surface and are never touched.

**Season and biome coupling.** The surface block depends on the season and terrain: it is `grass` in Spring, Summer and Autumn but `snow` in Winter, and beaches or altitude caps change it too. A patch authored with `replace = "grass"` therefore does not fire in Winter. Ore rules carry no biome field — biomes are a surface-patch concept only.

**Caps.** At most 8 worldgen rules load per stack; a rule beyond the cap reports an error diagnostic and is dropped while its valid siblings load. `vein` is 1 to 8, `attempts` is 1 to 32, and `min_y`/`max_y` must sit inside -64 to 128 with `min_y < max_y`.

**Determinism.** Each rule draws from its own seed-derived stream, so the same seed and stack always produce the same chunks, and adding or removing a rule never moves built-in coal or iron. `air` is a legal authored `block` or `replace`: as a `block` it fills air, and as a `replace` it clears the host block.

**Drift.** Generated content is not saved. Unedited chunks regenerate from the currently selected stack, so removing a pack removes its generated ore and patches, while player edits survive as placeholders. Changing the stack requires a world reload to take effect.

**Observable.** `opencraft --demo-worldgen` prints a deterministic digest of the selected stack (rule counts, authored ore cells and y band, patch cells and biomes, coal/iron stability versus no rules, and empty-rule byte identity), and `scripts/worldgen_smoke.sh` pins that digest plus bounded real-binary launches with the pack present and removed.

## Reference packs (test fixtures)

The repository ships two content-oriented examples under `tests/fixtures/packs/` next to the sound fixture. Copy both folders into the game's `packs/` directory and select them in this order:

1. `tests/fixtures/packs/mymod-ruby`
2. `tests/fixtures/packs/ruby-overlay`

`mymod-ruby` is a complete `format_version = 2` pack. It authors:

- Two blocks: `mymod:ruby_ore` (hardness 3, pickaxe, drops `mymod:ruby`) and `mymod:ruby_block` (hardness 4, pickaxe, drops itself).
- Two items: `mymod:ruby` and `mymod:ruby_stew` (food, 7 nutrition, 0.8 s to eat, stack 16).
- One tier `mymod:ruby` (speed 9, durability 900, damage 9) and two tools made from it: `mymod:ruby_pickaxe` and `mymod:ruby_sword`.
- Two shaped recipes: the ruby pickaxe (`RRR` over `S` over `S`) and ruby stew (`RR` over `WW`, which fits the 2x2 player grid).
- One recipe patch that disables the built-in shortbow while the pack is selected.
- Two worldgen rules: a ruby-ore vein rule in stone from y 4 to 40, and a ruby-ore surface patch on plains and forest grass from y 40 to 110.

`ruby-overlay` is a version 1 manifest with no content arrays and a single authored file, `stone.png`. Stacked after `mymod-ruby`, it replaces the stone block layer and leaves every other pack file, definition and field alone.

With that stack selected, these headless commands exercise the same code paths a world does:

```bash
./target/debug/opencraft --demo-presentation
./target/debug/opencraft --demo-crafting
./target/debug/opencraft --demo-worldgen
./target/debug/opencraft --demo-palette
./target/debug/opencraft --validate-pack mymod-ruby
./target/debug/opencraft --validate-pack ruby-overlay
bash scripts/modding_smoke.sh
```

`--demo-presentation` prints composed layer/icon counts, the authored block names, a production mesh, a production break, and the placeholder policy. `--demo-crafting` crafts the ruby pickaxe at a table and the stew in the 2x2 grid, breaks ruby ore with ruby and iron pickaxes, eats the stew, exercises the disabled shortbow, and saves/reloads the pickaxe. `--demo-worldgen` prints rule counts and authored ore/patch cells for a pinned seed. `--demo-palette` prints the entry stock, layout, filter, grant, use, placeholder and key-leg results. The four demo digests read the selected stack from `settings.toml`; with `mymod-ruby` missing they fail closed instead of printing a misleading success. `--validate-pack` checks one named folder. `scripts/modding_smoke.sh` is the real-binary proof of the validation exit codes, a bounded launch with a broken pack, and the docs reader-test.

## Content packs and saves

Content definitions are not baked into a world file. A version 9 save stores every block, item and tool reference by name in a sorted, save-local table; built-in names are unqualified and pack content uses its namespaced id. Loading reads those names against the currently selected stack. The v9 config schema and effective-value digest tail is separate from this content identity: it describes the gameplay tuning that wrote the file, not the pack stack.

When a saved name is well-formed but absent from the selected stack, the load succeeds and keeps the reference as a placeholder:

- An unknown block name becomes a placeholder block. It is solid, unbreakable and drops nothing, uses the checker texture, and stays in the world exactly where it was saved.
- An unknown generic item name becomes a placeholder item. It uses the checker icon, is movable and stackable in the inventory, and is unusable: it is not food, not a tool, and not placeable.
- A pack tool whose name disappears degrades to a count-one placeholder item. Restoring the pack returns it as the live tool at its full authored durability: wear is not carried through the placeholder.
- The saved names survive a second save. Removing a pack never rewrites the world to a different id, and restoring the pack resolves the same names to the real definitions again.
- A name that is malformed, uses a reserved namespace but does not exist, or refers to a liquid is not a placeholder: the whole load fails closed with a named error.

A changed stack shows one readable, non-blocking notice after the world is open and playable. It names the missing or added packs and reports how many saved content names are unavailable and render as placeholders. The notice never blocks saving or shutdown.

Generated worldgen is not saved. Unedited chunks regenerate from the currently selected stack, so removing a pack removes its generated ore and patches, while player edits survive as placeholders. Restoring the pack brings its generated content back on the next load. A stack change takes effect when the world is loaded again, not while the current session is running.

## Block textures

Save each block texture directly inside the pack folder, using these exact file names and in this order (the order is the block layer contract; do not reorder them):

1. `grass_top.png`
2. `grass_side.png`
3. `dirt.png`
4. `stone.png`
5. `cobblestone.png`
6. `sand.png`
7. `log_top.png`
8. `log_side.png`
9. `leaves.png`
10. `planks.png`
11. `brick.png`
12. `snow_top.png`
13. `bedrock.png`
14. `snow_side.png`
15. `coal_ore.png`
16. `iron_ore.png`
17. `farmland.png`
18. `wheat_stage0.png`
19. `wheat_stage1.png`
20. `wheat_stage2.png`
21. `wheat_stage3.png`
22. `chest.png`
23. `loot_chest.png`
24. `crafting_table_top.png`
25. `crafting_table_side.png`

The texture array has 48 layers. Layers 0 to 24 are the twenty-five block names above, in the order listed. Layers 25 to 31 are reserved block placeholders that cannot be overridden. Layers 32 to 44 are the thirteen mob part names in [Mob textures](#mob-textures) below. Layers 45 to 47 are reserved spare layers that cannot be overridden. A file named `reserved_25.png` or any other name that is not in one of the two lists is ignored.

A pack that authors only the original 16 names still loads: the newer blocks keep their built-in procedural art, and no warning is shown for the files a pack does not include. The same is true for packs that shipped only the original 23 block names — `crafting_table_top.png` (layer 23) and `crafting_table_side.png` (layer 24) are optional additions, so such packs remain valid unchanged. Partial packs may add one texture or all twenty-five.

Image rules:

- Use square PNG files, 16 to 256 pixels per side.
- Any size in that range is resampled to the game's 16-by-16 texture grid, so you can author at 16, 32, 64, 128, or 256 pixels and still get a crisp result.
- A missing file is fine and silently keeps the built-in texture for that block.
- A corrupt, non-square, or out-of-range file also keeps the built-in texture, but the menu shows a warning that names the file.
- Partial packs always work — add one texture or all twenty-five.

## Mob textures

Mob part art shares the same texture array as block art, starting at layer 32. To give the pig your own hide, override one mob part file — for example `pig_body.png` — and leave the other twelve alone. Save each file directly in the pack folder, using these exact names and in this order:

| File | Array layer | Mob part |
|------|-------------|----------|
| `pig_body.png` | 32 | Pig body |
| `pig_head.png` | 33 | Pig head |
| `pig_leg.png` | 34 | Pig legs |
| `cow_body.png` | 35 | Cow body |
| `cow_head.png` | 36 | Cow head |
| `cow_leg.png` | 37 | Cow legs |
| `zombie_body.png` | 38 | Zombie body |
| `zombie_head.png` | 39 | Zombie head |
| `zombie_leg.png` | 40 | Zombie legs |
| `skeleton_body.png` | 41 | Skeleton body |
| `skeleton_head.png` | 42 | Skeleton head |
| `skeleton_leg.png` | 43 | Skeleton legs |
| `arrow.png` | 44 | Arrow projectile |

The same image rules as block textures apply to every mob file: square PNG, 16 to 256 pixels per side, resampled to 16x16.

The committed `tests/fixtures/packs/uat-sound/` folder is the per-file layout example: it shows the flat "one file per override, named exactly" layout a pack uses (its files happen to be sounds, so a mob texture pack keeps the same layout with the `pig_body.png`-style names from the table above).

- Each file is optional and overrides only its own layer.
- A missing file silently keeps the built-in procedural art for that part.
- A corrupt, non-square, or out-of-range file also keeps the built-in art, but shows a warning that names the bare file name.
- A pack may add one mob file or all thirteen. Mob files and block files are independent.
- Layers 25 to 31 and 45 to 47 are reserved and cannot be overridden. Files named after them, such as `reserved_45.png`, are ignored silently.

## Title logo and menu tint

- `logo` may be any PNG at its native size and aspect ratio. Non-square logos are fine because the game preserves the image shape; keep each side at 1024 pixels or less.
- A missing or corrupt logo does not break the menu: the game falls back to the built-in OpenCraft text title and shows a warning naming the logo file.
- `overlay_tint` dims the whole panorama behind the menu. Use four values in RGBA order, 0 to 255; for example `[24, 32, 48, 96]` is a dark, mostly transparent blue-grey. An invalid tint is ignored and a warning is shown.

## Sounds

A pack can replace the game's nine built-in sounds. Save audio files flat in the pack folder, next to the textures, using these exact base names:

| Base name | When it plays |
| --- | --- |
| `break` | breaking a block |
| `place` | placing a block |
| `mine_complete` | finishing a block's mining time |
| `ui_click` | clicking a menu row or button |
| `mob_idle` | a mob's idle grunt |
| `mob_hurt` | a mob taking damage |
| `mob_death` | a mob dying |
| `bow_release` | releasing a drawn bow |
| `arrow_impact` | an arrow hitting a block or mob |

The first four are the original block and menu sounds; a pack written before mobs existed keeps overriding exactly the same slots, because new sounds are only ever appended to the end of the table.

Each sound may use any of these formats. If several files share a base name, only the highest-priority one is read:

1. `.wav` (WAV)
2. `.ogg` (Ogg Vorbis)
3. `.flac` (FLAC)
4. `.mp3` (MP3)
5. `.m4a` (M4A/MP4)

For example, `break.wav` or `ui_click.ogg`. Each file must be 1 MiB (1,048,576 bytes) or smaller.

Sound rules:

- A missing sound file is fine and silently keeps the built-in synthesized sound for that slot.
- A corrupt, oversized, or empty (zero-sample) file also keeps the built-in sound, but the menu shows a warning that names the file.
- Partial packs always work — add one sound or all nine, with or without textures.
- Sounds apply live when you toggle a pack on the **Packs** screen; no restart is needed.
- Disabling every pack (back to **Default**) restores all nine built-in sounds.

## A complete sound pack

A sound pack is a `pack.toml` manifest plus up to nine optional sound files. Copy
this manifest and save it as `packs/my-sounds/pack.toml`:

```toml
name = "My Sounds"
description = "Custom break, place, mine-complete, and UI-click audio"
```

The folder looks like this — a pack may contain any subset of the sound files:

```
packs/my-sounds/
  pack.toml
  break.wav         # optional - block broken
  place.wav         # optional - block placed
  mine_complete.wav # optional - mining sequence finished
  ui_click.wav      # optional - menu interaction
  mob_idle.wav      # optional - mob idle grunt
  mob_hurt.wav      # optional - mob damaged
  mob_death.wav     # optional - mob died
  bow_release.wav   # optional - bow released
  arrow_impact.wav  # optional - arrow landed
```

Every sound file is optional. A missing slot silently keeps the built-in sound for
that event, so you do not need placeholder files. Any of the five accepted
extensions (`.wav`, `.ogg`, `.flac`, `.mp3`, `.m4a`) works; see [Sounds](#sounds)
for the accepted formats and the 1 MiB per-file cap.

### Runnable example: `tests/fixtures/packs/uat-sound/`

The project repository ships a ready-made example pack at `tests/fixtures/packs/uat-sound/`. It
holds seven files: the manifest plus six sounds, five valid and one deliberately
corrupt.

| File | State | What it proves |
| --- | --- | --- |
| `pack.toml` | valid | the manifest and the menu tint |
| `break.wav` | valid | a replaced block-break sound |
| `place.wav` | valid | a replaced block-place sound |
| `mine_complete.wav` | valid | a replaced mining-complete sound |
| `mob_hurt.wav` | valid | a replaced mob-damage sound (a byte copy of `break.wav`) |
| `arrow_impact.wav` | valid | a replaced arrow-landing sound (a byte copy of `place.wav`) |
| `ui_click.wav` | deliberately corrupt ASCII | per-file fallback: the menu reports the decode failure and keeps the built-in tick |

It is a test fixture, not part of a normal game install. To try it, copy the
`tests/fixtures/packs/uat-sound/` folder into your game's working directory as `packs/uat-sound/` —
creating the `packs/` folder yourself first if it does not exist — then open
**Settings → Packs** and enable `uat-sound`. Break and place a block and finish a
mining sequence to hear the custom sounds; hit a mob and land an arrow to hear the
pack's `mob_hurt` and `arrow_impact` files; open the settings menu to see the
corrupt-file warning while the built-in click keeps playing. Disabling every pack
(back to **Default**) restores the built-in sounds.

## Validating a pack

Check a pack without launching a world, opening a window, or touching `settings.toml`, `saves/`, or any pack file:

```bash
opencraft --validate-pack my-first-pack
```

The folder name is the same one used under `packs/`. Each accepted authored entry prints an `ok:` line, a rejected entry prints an `error:` line naming the entry and reason, and non-fatal fallbacks (a corrupt PNG, a v1 pack declaring v2 content arrays) print a `warning:` line:

```text
ok: block mymod:ruby_ore
ok: item mymod:ruby
warning: pack.toml: content arrays require format_version = 2
validate-pack: my-first-pack: 2 entries, 0 errors, 1 warnings
```

An accepted built-in override prints its own `ok:` line and is counted in the summary. A missing override PNG fails validation with an `error: asset ...` line while still explaining the fallback:

```text
error: asset textures/missing.png: failed to read asset `textures/missing.png`: No such file or directory (os error 2)
ok: builtin_icon apple
warning: builtin_icon apple: icon `textures/missing.png` is not a loaded asset; the built-in sprite stays visible
validate-pack: my-first-pack: 1 entries, 1 errors, 1 warnings
```

The exit code is `0` when there are no errors (warnings still pass) and `1` when an entry was rejected or the pack folder is missing. Validation builds the same registry a normal load builds over the built-in content, so references must name a built-in block/item or an entry in the same pack — cross-pack references are not supported.

## Selecting packs in game

- Open **Settings** and click the **Packs** row to open the Packs screen.
- Click a row to toggle that pack. Enabled packs are listed first in stack order; a later pack wins per authored file and field.
- Use the `^` and `v` actions at the right edge of an enabled row to move it one step up or down the stack.
- Presentation applies immediately — no restart is needed. The logo, the menu tint, the block textures, and the pack's sounds update live.
- The stack is saved in `settings.toml` under `[packs]` as `stack = ["folder-name", ...]`, so it comes back on the next launch. A legacy file with `texture_pack = "folder-name"` is read as a one-entry stack and rewritten in the `stack` form on the next save.
- Content definitions (blocks, items, tiers, tools, recipes, and worldgen) are loaded once when a world session starts. If a change moves the content digest, the Packs screen shows `content changes apply when the world is loaded again`; presentation still updates immediately and the message never blocks saving or shutdown.
- If an enabled pack folder is deleted or renamed, the game keeps running with the remaining packs and shows a warning naming the missing pack. The missing row stays in the stack, so restoring the folder makes it apply again.
- Settings only shows packs that are folders under `packs/`; loose files in that directory are ignored.

## Live texture reload (dev builds)

Development builds watch the active pack stack while the game is running, so you can tune artwork without relaunching. The watcher checks the files about every 100 ms (ten times per second) and applies an edit only after the file has stayed unchanged for 250 ms; an editor's atomic save or a burst of saves therefore becomes one apply. Release builds have no file watcher: a direct file edit appears on the next launch or after a Packs action.

**Applies live**

- The 25 block textures and 13 mob textures listed above. A fixed-name file that was missing from a loaded pack can be created later and immediately replaces that layer. When the edited pack is the visible author of the layer, only that one texture-array layer is re-uploaded.
- The block-face images that a `format_version = 2` stack loaded and allocated when it was applied. Each image keeps its allocated layer index and only its bytes are replaced, so the live path can never move allocation.
- Deleting one of the 38 fixed-name files is live too: the layer falls back to the next pack in the stack that authors it, or to the built-in procedural art when no pack does.
- A corrupt, non-square, or out-of-range PNG (outside 16 to 256 pixels per side) is rejected. The layer keeps its last good image and the game shows one message naming the file and reason; a valid edit settling in the same poll still applies.
- Built-in item sprites overridden by `[builtin_icons]`. Editing the authored PNG recomposes and re-uploads the single 256-layer item sprite atlas; only that name's pinned layer changes. The pinned layers are `apple` = 1, `wood_pickaxe` = 11 and `wood_bow` = 26; reserved layers 27–31 are never overridable, and pack-defined icons at layers 32 and above are untouched.
- Adding, removing, or repointing a `[builtin_icons]` entry in `pack.toml` applies without a restart. A malformed PNG keeps the last good override, and deleting the authored file (or removing the entry) reveals the next lower pack's override or the procedural built-in sprite.

**Applies when the world is loaded again**

Editing `pack.toml` is compared against the content identity captured when the stack was applied. An edit that moves content — blocks, items, tiers, tools, recipes, worldgen, or the pack block-face/icon allocation that follows from them — sets `restart required: <folder>: content changed; restart to apply` in the F6 overlay and shows the same reason on its `packs:` line. The Packs screen and a transient notice add `content changes apply when the world is loaded again`. No live layer index moves and no layer is re-uploaded. Quit and relaunch the game, then load the same save, to use the new registry. Editing the manifest back to the installed content clears the requirement. `[builtin_icons]` edits are the exception: the table is presentation-only and reconciles live even when another field in the same manifest edit still requires a restart.

**Not live-texture-reloaded**

- Icons declared by pack-defined `[[items]]` and `[[tools]]` entries are part of the world session's frozen content registry. The live watcher does not re-upload them; a Packs action captures new icon art for the next world session. Built-in overrides from `[builtin_icons]` are different: they are presentation-only and apply live.
- A manifest's `logo` and `overlay_tint` apply when you toggle or reorder packs on the Packs screen (or relaunch), not from a direct manifest edit.
- Sounds also apply on a Packs action, not from a watched file edit.

In a development build, the F6 creator overlay reports the last pack outcome on its `packs:` line. Launching with `--creator-trace` prints one stable line for each applied texture, each rejected edit, and each manifest edit that raises a restart requirement:

```text
creator: texture applied path=packs/my-first-pack/stone.png layer=3 digest=0x0123456789abcdef
creator: texture applied path=packs/my-first-pack/textures/apple.png layer=1 digest=0x0123456789abcdef
creator: texture rejected path=packs/my-first-pack/stone.png reason=failed to decode PNG
creator: restart-required reason=my-first-pack: content changed; restart to apply
```

A removed override is not an applied texture: the F6 `packs:` line reports `builtin icon <name> restored` instead, so a fallback to a lower pack or the procedural sprite is distinguishable from a successful PNG decode.

Release builds contain no pack watcher, no creator overlay, and no trace output.

## Packs screen

The dedicated **Packs** screen lists enabled packs first in stack order, then every remaining pack folder alphabetically, greyed out and disabled. Click a row to toggle that pack: disabling removes it from the stack, enabling appends it at the end. Enabled rows carry `^` and `v` actions at the right edge to move one step up or down the stack, so the last enabled pack wins. **Escape** or the **Back** row leaves the screen. A pack folder that no longer exists stays listed and marked `(missing)` while it is in the stack, so restoring the folder makes it apply again without re-adding it.

Toggling and reordering save immediately, so the next launch restores the same order. Texture, logo, tint, and sound changes apply at once; content-definition changes apply when the world is loaded again and the screen says so.

## Troubleshooting

Warnings appear in the Settings menu as `name: reason` lines, joined together if there are several. Each one is a fallback, not a crash.

| Warning you see | What it means | Fix |
| --- | --- | --- |
| `pack.toml: failed to read ...` | The pack folder has no readable `pack.toml`. | Add `pack.toml` to the pack folder, or check that the file is readable. |
| `pack.toml: invalid TOML: ...` | The manifest is not valid TOML, so every manifest value was ignored. | Check quoting, brackets, and table headers against the Quickstart example. |
| ``pack.toml: expected `name` to be a string ...`` | A known field has the wrong type; other fields still load. | Put the value in quotes, as in `name = "My Pack"`. |
| ``pack.toml: expected `menu` to be a table ...`` | `menu` was written as a value instead of a `[menu]` table. | Use a `[menu]` header on its own line. |
| ``pack.toml: expected `menu.logo` to be a string ...`` | `logo` is not a file name string. | Write `logo = "logo.png"`. |
| ``pack.toml: expected `menu.overlay_tint` ...`` | The tint is not an array of four integers in 0 to 255. | Use exactly four values, for example `overlay_tint = [24, 32, 48, 96]`. |
| `stone.png: failed to read PNG header ...` or `failed to decode PNG ...` | The file is not a readable PNG. | Re-export the image as PNG. |
| `stone.png: image is 32x16, layers must be square` | A block texture is not square. | Crop or resize it to equal width and height. |
| `stone.png: image is 8x8, layers must be between 16 and 256 pixels per side` | A block texture is outside the accepted size range. | Resize it to 16 to 256 pixels per side. |
| `logo.png: ... larger than the 1024 px logo limit` | The logo is too large. | Resize the logo to 1024 pixels or less per side. |
| `my-pack: pack folder ... is missing` | `settings.toml` points at a pack folder that no longer exists. | Restore the folder, or disable that pack on the Packs screen. |
| `break.wav: failed to decode sound file ...` | The file is not readable audio in a supported format. | Re-export it as WAV, Ogg Vorbis, FLAC, MP3, or M4A. |
| `break.wav: sound file is ... bytes, the limit is 1048576 bytes` | The sound file is larger than the 1 MiB cap. | Shorten it or export at a lower bitrate. |
| `break.wav: sound file decoded to zero samples` | The file has a valid header but contains no audio frames. | Export the sound again with actual audio data. |
