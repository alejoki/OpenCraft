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
two archive names in the download table in sync with what the workflow builds
(they derive from the crate name and version).

Add the `### Downloading a release` line in `README.md` if this is a new minor
version.

## 3. Rehearse on a branch

Push the version commit to `milestone/M016` (or the current milestone branch).
That push triggers `release.yml`, but the publish job stays tag-gated: the
validate/package jobs run and upload artifacts, and nothing is published.
This is the whole point of the rehearsal — archive generation, the layout
check, and the two-OS smoke subset are exercised before a tag exists.

## 4. Read the run and verify the artifacts

```bash
WORKFLOW=release.yml bash scripts/ci_gate.sh
gh run download <run-id> --dir dist/rehearsal
bash scripts/verify_release_artifacts.sh dist/rehearsal 0.2.0
```

`verify_release_artifacts.sh` unpacks the archives and asserts that
`opencraft-<VERSION>-x86_64-unknown-linux-gnu.tar.gz` contains an executable
`opencraft`, that `opencraft-<VERSION>-x86_64-pc-windows-msvc.zip` contains
`opencraft.exe`, and that both carry `config/combat.toml`,
`worldgen/villages/plains.toml` and `LICENSE` at the archive root. It also runs
the Linux binary headlessly (`--demo-ui`, `--demo-village`, `--validate-pack`)
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
git tag -a v0.2.0 -m "OpenCraft 0.2.0"
git push origin v0.2.0
```

Always use an annotated tag. The tag name must match `v` + the `Cargo.toml`
version, or the workflow's version check fails before packaging. Pushing the
tag triggers the package jobs and the publish job for that commit.

## 7. Verify the published release

```bash
gh release view v0.2.0
gh release download v0.2.0 --dir dist/published
bash scripts/verify_release_artifacts.sh dist/published 0.2.0
bash scripts/verify_release_page.sh v0.2.0 0.2.0
```

Check by eye and by script:

- exactly two assets, `opencraft-0.2.0-x86_64-unknown-linux-gnu.tar.gz` and
  `opencraft-0.2.0-x86_64-pc-windows-msvc.zip`;
- the release body is the contents of `docs/RELEASE-NOTES-0.2.0.md`;
- `verify_release_page.sh` finds the archive names, `SmartScreen`, and
  `version 9` in the published body;
- on Linux, extract the tarball and run `./opencraft --demo-ui` headlessly: it
  must print the `ui_demo:` digest lines and exit before opening a window.

## 8. Recovery

The publish job is idempotent: it uses `gh release edit` for the body and
`gh release upload --clobber` for the assets, so re-running a green package
job repairs a partial release without hand-editing assets.

If the tag is wrong, delete the release and the tag, fix, and re-tag:

```bash
gh release delete v0.2.0 --cleanup-tag -y
# or, if the release never existed:
git push origin :refs/tags/v0.2.0
```

Never move a published tag without deleting the release first: downstream
clones and cached tarballs would otherwise disagree about what `v0.2.0` means.
After re-tagging, repeat section 7.

## Failure modes of this checklist

- **Bumping one Cargo file only:** `cargo metadata --locked` fails immediately;
  the rehearsal never starts.
- **Tag does not match the crate version:** the workflow's version guard fails
  before any packaging.
- **Missing `docs/RELEASE-NOTES-<VERSION>.md`:** the release-notes step fails
  the run; no release is created.
- **Pushing a tag on a red commit:** package jobs fail, publish is skipped, and
  the tag stays behind until it is deleted in section 8.
