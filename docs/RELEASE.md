# Releasing OpenCraft

This is the maintainer checklist for cutting an OpenCraft release. It assumes
the release is cut from `main` and published by
`.github/workflows/release.yml`, which runs on two GitHub-hosted runners
(`ubuntu-latest` and `windows-latest`) and only publishes on an annotated
`v*` tag.

## 1. Prerequisites

- `gh auth status` shows a logged-in account with `repo` and `workflow`
  scopes. The publish job needs to create and edit the GitHub release.
- The working tree is clean (`git status --short` prints nothing) and you are
  on the commit you intend to tag.
- `rust-toolchain.toml` is committed and pins the toolchain (currently
  `1.99.1`). CI uses the pinned toolchain; do not bump it as part of a version
  bump unless that is the point of the release.

## 2. Bump the version

The version lives in exactly two files and both must change in the **same
commit**:

| File | What changes |
| --- | --- |
| `Cargo.toml` | `[package] version` |
| `Cargo.lock` | the single root `[[package]] name = "opencraft"` entry |

Never bump one without the other: every release command uses `--locked`, and
`cargo metadata --locked` fails when the two disagree.

Hand-edit the lock entry and verify only that line moved:

```bash
cargo metadata --locked --no-deps --format-version 1 >/dev/null
git diff Cargo.lock   # only the opencraft version line
```

Do **not** run a bare `cargo update`; it churns unrelated dependency versions.

Then write `docs/RELEASE-NOTES-<VERSION>.md`. The workflow looks up exactly
`docs/RELEASE-NOTES-${VERSION}.md` and fails the run if it is missing. Keep the
three asset names in the download table in sync with what the workflow builds
(they derive from the crate name and version): both portable archives plus
`opencraft-<VERSION>-x86_64-pc-windows-msvc-setup.exe`. Document the Windows
installer next to the zip, and keep the unsigned/SmartScreen caveat for both.

Add the `### Downloading a release` line in `README.md` if this is a new minor
version.

## 3. Rehearse on a branch

`.github/workflows/release.yml` is tags-only (`on.push.tags: ["v*"]`), so a
plain branch push does not run it. To rehearse, temporarily re-add a branch
trigger under `on.push`, immediately after the `tags:` entry:

```yaml
    # TEMPORARY rehearsal trigger: removed before the tag.
    branches:
      - "milestone/M016"
```

Commit that with the version bump, push the branch, and let the `version` and
`build` jobs run on both OSes. The Windows job additionally builds
`installer/opencraft.iss` with Inno Setup 6.3+ (installed via Chocolatey when
the runner does not provide `ISCC.exe`), silently installs it into
`dist/install-smoke`, and runs the installed `--demo-ui` seam before uploading
`opencraft-installer`. The `publish` job stays tag-gated by
`if: startsWith(github.ref, 'refs/tags/v')`, so the packaged artifacts are
uploaded and nothing is published. This is the whole point of the rehearsal —
archive generation, installer compile and install, the layout check, and the
two-OS build are exercised before a tag exists.

There is no `workflow_dispatch` path: `release.yml` does not exist on `main`,
so a dispatch trigger would not be usable. Delete the `branches` entry in the
final pre-tag commit, before tagging.

## 4. Read the run and verify the artifacts

```bash
WORKFLOW=release.yml bash scripts/ci_gate.sh
gh run download <run-id> --dir dist/rehearsal
bash scripts/verify_release_artifacts.sh dist/rehearsal 0.2.2
```

`verify_release_artifacts.sh` unpacks the archives and asserts that
`opencraft-<VERSION>-x86_64-unknown-linux-gnu.tar.gz` contains an executable
`opencraft`, that `opencraft-<VERSION>-x86_64-pc-windows-msvc.zip` contains
`opencraft.exe`, and that both carry `config/combat.toml`,
`worldgen/villages/plains.toml` and `LICENSE` at the archive root. It also
rejects a Windows `opencraft.exe` that still imports `VCRUNTIME140.dll` (D290
builds the release with a static CRT), and requires
`opencraft-<VERSION>-x86_64-pc-windows-msvc-setup.exe` to be a PE image whose
version resource names OpenCraft and the release version. The script runs the
Linux binary headlessly (`--demo-ui`, `--demo-village`, `--validate-pack`)
from a scratch directory, and rejects any `packs/`, `saves/` or `settings.toml`
entry in the Windows zip.

If the rehearsal is red, fix forward and push again. Do not tag a red
rehearsal.

## 5. Final pre-tag gate

On the exact commit you will tag, confirm the ordinary CI gate is green:

```bash
WORKFLOW=ci.yml bash scripts/ci_gate.sh
```

Then run the release-profile smoke subset locally:

```bash
SMOKE_PROFILE=release bash scripts/presentation_smoke.sh
SMOKE_PROFILE=release bash scripts/crafting_smoke.sh
SMOKE_PROFILE=release bash scripts/worldgen_smoke.sh
SMOKE_PROFILE=release bash scripts/modding_smoke.sh
SMOKE_PROFILE=release bash scripts/combat_smoke.sh
```

All five must exit 0. This is the last chance to notice a release-only
regression (the release build drops the creator watcher, overlay, and trace).

## 6. Tag

```bash
git tag -a v0.2.2 -m "OpenCraft 0.2.2"
git push origin v0.2.2
```

Always use an annotated tag. The tag name must match `v` + the `Cargo.toml`
version, or the workflow's version check fails before packaging. Pushing the
tag triggers the package jobs and the publish job for that commit.

## 7. Verify the published release

```bash
gh release view v0.2.2
gh release download v0.2.2 --dir dist/published
bash scripts/verify_release_artifacts.sh dist/published 0.2.2
bash scripts/verify_release_page.sh v0.2.2 0.2.2
```

Check by eye and by script:

- exactly three assets, `opencraft-<VERSION>-x86_64-unknown-linux-gnu.tar.gz`,
  `opencraft-<VERSION>-x86_64-pc-windows-msvc.zip` and
  `opencraft-<VERSION>-x86_64-pc-windows-msvc-setup.exe` (releases published
  before D290, including v0.2.2, carry only the two portable archives; verify
  those with the verifier from that tag);
- the release body is the contents of `docs/RELEASE-NOTES-0.2.2.md`;
- `verify_release_page.sh` finds the three asset names, `SmartScreen`, and
  `version 9` in the published body;
- on Linux, extract the tarball and run `./opencraft --demo-ui` headlessly: it
  must print the `ui_demo:` digest lines and exit before opening a window.

## 8. Fresh-clone verification

The tag-triggered Release run is the fresh-clone, two-OS proof: both `build`
jobs check out the tag into a clean tree and run `cargo test --locked` and
`cargo build --release --locked` there. Record the run URL with the release
checklist.

To repeat the fresh-clone build locally on Linux:

```bash
tmp=$(mktemp -d)
git clone --depth 1 --branch v0.2.2 https://github.com/alejoki/OpenCraft.git "$tmp/opencraft"
cd "$tmp/opencraft"
PKG_CONFIG_PATH=/home/aleksi/minecraft-clone/.toolchain/alsa/usr/lib64/pkgconfig cargo test --locked
PKG_CONFIG_PATH=/home/aleksi/minecraft-clone/.toolchain/alsa/usr/lib64/pkgconfig cargo build --release --locked
```

The tracked `.cargo/config.toml` sets only a relative `PKG_CONFIG_PATH`
(`.toolchain/alsa/usr/lib64/pkgconfig`), and `.toolchain/` is gitignored, so a
fresh clone has no ALSA development files. An exported `PKG_CONFIG_PATH`
overrides that non-forced `[env]` value; CI instead installs `libasound2-dev`
via `scripts/install_linux_build_deps.sh`.

## 9. Recovery

The publish job is idempotent: it uses `gh release edit` for the body and
`gh release upload --clobber` for the assets, so re-running a green package
job repairs a partial release without hand-editing assets.

If the tag is wrong, delete the release and the tag, fix, and re-tag:

```bash
gh release delete v0.2.2 --cleanup-tag -y
# or, if the release never existed:
git push origin :refs/tags/v0.2.2
```

Never move a published tag without deleting the release first: downstream
clones and cached tarballs would otherwise disagree about what `v0.2.2` means.
After re-tagging, repeat section 7.

## Failure modes of this checklist

- **Bumping one Cargo file only:** `cargo metadata --locked` fails immediately;
  the rehearsal never starts.
- **Tag does not match the crate version:** the workflow's version guard fails
  before any packaging.
- **Missing `docs/RELEASE-NOTES-<VERSION>.md`:** the release-notes step fails
  the run; no release is created.
- **ISCC.exe missing or older than 6.3:** the Windows job's
  `scripts/build_windows_installer.sh` installs Inno Setup via Chocolatey and
  fails when `ISCC.exe` is still absent; `ArchitecturesAllowed=x64compatible`
  is rejected by Inno Setup older than 6.3.
- **Windows exe still imports `VCRUNTIME140.dll`:** the release build lost its
  `RUSTFLAGS: -C target-feature=+crt-static` matrix entry (D290), so
  `verify_release_artifacts.sh` rejects the zip before publishing.
- **A Windows dependency refuses the static CRT:** if the link fails, restore a
  dynamic-CRT build and chain `VC_redist.x64.exe` from the installer's `[Run]`
  section instead; replace the verifier's `VCRUNTIME140.dll` rejection with a
  redistributable presence check in the same change.
- **Pushing a tag on a red commit:** package jobs fail, publish is skipped, and
  the tag stays behind until it is deleted in section 9.
