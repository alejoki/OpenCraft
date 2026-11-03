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

The committed `packs/uat-sound/` folder is the per-file layout example: it shows the flat "one file per override, named exactly" layout a pack uses (its files happen to be sounds, so a mob texture pack keeps the same layout with the `pig_body.png`-style names from the table above).

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
- Sounds apply live when you cycle the **Texture Pack** row; no restart is needed.
- Cycling back to **Default** restores all nine built-in sounds.

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

### Runnable example: `packs/uat-sound/`

The project repository ships a ready-made example pack at `packs/uat-sound/`. It
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
`uat-sound/` folder into your game's working directory as `packs/uat-sound/` —
creating the `packs/` folder yourself first if it does not exist — then cycle the
**Texture Pack** row to `uat-sound`. Break and place a block and finish a mining
sequence to hear the custom sounds; hit a mob and land an arrow to hear the pack's
`mob_hurt` and `arrow_impact` files; open the settings menu to see the corrupt-file
warning while the built-in click keeps playing. Cycling back to **Default** restores
the built-in sounds.

## Selecting a pack in game

- Open **Settings** and click the **Texture Pack** row. Each click advances through `Default` and every pack folder found under `packs/`, wrapping back to `Default`.
- The change applies immediately — no restart is needed. The logo, the menu tint, the block textures, and the pack's sounds update live.
- The choice is saved in `settings.toml` under `[packs]` as `texture_pack = "folder-name"`, so it comes back on the next launch.
- If the selected pack folder is deleted or renamed, the game keeps running with the built-in Default textures and shows an in-menu warning naming the pack. Cycle the row back to `Default` (or to the new folder name) to clear the stale selection.
- Settings only shows packs that are folders under `packs/`; loose files in that directory are ignored.

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
| `my-pack: pack folder ... is missing` | `settings.toml` points at a pack folder that no longer exists. | Restore the folder, or cycle the Texture Pack row back to Default. |
| `break.wav: failed to decode sound file ...` | The file is not readable audio in a supported format. | Re-export it as WAV, Ogg Vorbis, FLAC, MP3, or M4A. |
| `break.wav: sound file is ... bytes, the limit is 1048576 bytes` | The sound file is larger than the 1 MiB cap. | Shorten it or export at a lower bitrate. |
| `break.wav: sound file decoded to zero samples` | The file has a valid header but contains no audio frames. | Export the sound again with actual audio data. |
