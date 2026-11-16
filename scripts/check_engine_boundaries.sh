#!/usr/bin/env bash
# Fail the build if an engine crate reaches back into the game or picks up
# dependencies it must not own (M017/S01).
#
# Rules for every engine member under crates/ (all seven opencraft-* members
# and the engine-reuse-proof consumer):
#   - no dependency on the root `opencraft` game package (direct or transitive)
#   - publish = false and no version.workspace (the release guard reads the
#     root package version; members version independently)
#   - opencraft-core, opencraft-physics, opencraft-pathfinding,
#     opencraft-storage, opencraft-content, opencraft-worldgen: no wgpu,
#     wgpu-core, wgpu-types, winit or raw-window-handle
#   - opencraft-gfx: no winit (wgpu is allowed), and no direct winit or
#     raw-window-handle manifest entry
#   - opencraft-storage: no opencraft-content, opencraft-worldgen, toml or png
#   - opencraft-content: no opencraft-storage or opencraft-worldgen
#   - opencraft-worldgen: no opencraft-storage (worldgen -> content is allowed)
#   - engine-reuse-proof: no direct wgpu, winit, raw-window-handle, pollster,
#     bytemuck, serde or tempfile manifest entry; it consumes the engine crates
#     only and the no-game-package rule above still applies
#
# Dependencies: bash, cargo, grep, sed. Deliberately no jq or Python so the
# gate also runs on the Windows runner.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

fail() {
    echo "FAIL: engine boundaries: $*" >&2
    exit 1
}

# Echo the first violated rule for a cargo-tree text, or return 1 when clean.
tree_violation() {
    local name="$1" tree="$2"
    # `opencraft v` must not match opencraft-core/opencraft-gfx; the space and
    # version digit pin the game package name exactly.
    if grep -Eq '(^|[[:space:]])opencraft v[0-9]' <<<"$tree"; then
        echo "depends on the opencraft game package"
        return 0
    fi
    case "$name" in
        opencraft-gfx)
            if grep -Eq '(^|[[:space:]])winit v[0-9]' <<<"$tree"; then
                echo "depends on winit"
                return 0
            fi
            ;;
        opencraft-core|opencraft-physics|opencraft-pathfinding|opencraft-storage|opencraft-content|opencraft-worldgen)
            if grep -Eq '(^|[[:space:]])(wgpu|wgpu-core|wgpu-types|winit|raw-window-handle) v[0-9]' <<<"$tree"; then
                echo "depends on a GPU/windowing crate"
                return 0
            fi
            ;;
    esac
    case "$name" in
        opencraft-storage)
            if grep -Eq '(^|[[:space:]])(opencraft-content|opencraft-worldgen) v[0-9]' <<<"$tree"; then
                echo "depends on opencraft-content or opencraft-worldgen"
                return 0
            fi
            ;;
        opencraft-content)
            if grep -Eq '(^|[[:space:]])(opencraft-storage|opencraft-worldgen) v[0-9]' <<<"$tree"; then
                echo "depends on opencraft-storage or opencraft-worldgen"
                return 0
            fi
            ;;
        opencraft-worldgen)
            if grep -Eq '(^|[[:space:]])opencraft-storage v[0-9]' <<<"$tree"; then
                echo "depends on opencraft-storage"
                return 0
            fi
            ;;
    esac
    return 1
}

# Echo the first violated manifest rule, or return 1 when clean.
manifest_violation() {
    local manifest="$1" name="$2" forbidden
    if ! grep -Eq '^[[:space:]]*publish[[:space:]]*=[[:space:]]*false[[:space:]]*$' "$manifest"; then
        echo "does not set publish = false"
        return 0
    fi
    if grep -Eq 'version\.workspace' "$manifest"; then
        echo "uses version.workspace"
        return 0
    fi
    case "$name" in
        opencraft-gfx)
            forbidden='winit|raw-window-handle'
            ;;
        opencraft-core|opencraft-physics|opencraft-pathfinding)
            forbidden='wgpu|wgpu-core|wgpu-types|winit|raw-window-handle'
            ;;
        opencraft-storage)
            forbidden='wgpu|wgpu-core|wgpu-types|winit|raw-window-handle|opencraft-content|opencraft-worldgen|toml|png'
            ;;
        opencraft-content)
            forbidden='wgpu|wgpu-core|wgpu-types|winit|raw-window-handle|opencraft-storage|opencraft-worldgen'
            ;;
        opencraft-worldgen)
            forbidden='wgpu|wgpu-core|wgpu-types|winit|raw-window-handle|opencraft-storage'
            ;;
        engine-reuse-proof)
            forbidden='wgpu|winit|raw-window-handle|pollster|bytemuck|serde|tempfile'
            ;;
        *)
            forbidden=''
            ;;
    esac
    if [[ -n "$forbidden" ]] && grep -Eq "($forbidden)" "$manifest"; then
        echo "mentions a forbidden dependency in the manifest"
        return 0
    fi
    return 1
}

# Unconditional self-test so a broken rule can never silently pass.
self_test() {
    local violation

    local neutral_tree=$'opencraft-core v0.1.0\n└── glam v0.27.0'
    if violation=$(tree_violation opencraft-core "$neutral_tree"); then
        fail "self-test rejected a neutral tree: $violation"
    fi

    local gfx_wgpu_tree=$'opencraft-gfx v0.1.0\n└── wgpu v0.19.0'
    if violation=$(tree_violation opencraft-gfx "$gfx_wgpu_tree"); then
        fail "self-test rejected allowed wgpu in opencraft-gfx: $violation"
    fi

    local game_edge_tree=$'opencraft-core v0.1.0\n└── opencraft v0.2.2'
    if ! violation=$(tree_violation opencraft-core "$game_edge_tree"); then
        fail "self-test accepted a dependency on the opencraft game package"
    fi

    local wgpu_core_tree=$'opencraft-core v0.1.0\n└── wgpu v0.19.0'
    if ! violation=$(tree_violation opencraft-core "$wgpu_core_tree"); then
        fail "self-test accepted wgpu in opencraft-core"
    fi

    local winit_gfx_tree=$'opencraft-gfx v0.1.0\n└── winit v0.30.0'
    if ! violation=$(tree_violation opencraft-gfx "$winit_gfx_tree"); then
        fail "self-test accepted winit in opencraft-gfx"
    fi

    local neutral_storage_tree=$'opencraft-storage v0.1.0\n├── opencraft-core v0.1.0\n└── tempfile v3.8.0'
    if violation=$(tree_violation opencraft-storage "$neutral_storage_tree"); then
        fail "self-test rejected a neutral opencraft-storage tree: $violation"
    fi

    local storage_content_tree=$'opencraft-storage v0.1.0\n└── opencraft-content v0.1.0'
    if ! violation=$(tree_violation opencraft-storage "$storage_content_tree"); then
        fail "self-test accepted a storage -> content edge"
    fi

    local storage_wgpu_tree=$'opencraft-storage v0.1.0\n└── wgpu v0.19.0'
    if ! violation=$(tree_violation opencraft-storage "$storage_wgpu_tree"); then
        fail "self-test accepted wgpu in opencraft-storage"
    fi

    local content_worldgen_tree=$'opencraft-content v0.1.0\n└── opencraft-worldgen v0.1.0'
    if ! violation=$(tree_violation opencraft-content "$content_worldgen_tree"); then
        fail "self-test accepted a content -> worldgen edge"
    fi

    echo "PASS: boundary self-test"
}

echo "engine boundaries: repo=$REPO_ROOT"

if ! cargo metadata --locked --no-deps --format-version 1 >/dev/null; then
    fail "cargo metadata --locked failed (stale Cargo.lock or malformed manifest)"
fi

for crate in opencraft-core opencraft-gfx opencraft-physics opencraft-pathfinding opencraft-storage opencraft-content opencraft-worldgen engine-reuse-proof; do
    [[ -f "crates/$crate/Cargo.toml" ]] || fail "missing crates/$crate/Cargo.toml"
done

self_test

shopt -s nullglob
manifests=(crates/opencraft-*/Cargo.toml crates/engine-reuse-proof/Cargo.toml)
shopt -u nullglob
[[ ${#manifests[@]} -gt 0 ]] || fail "no crates/opencraft-*/Cargo.toml manifests found"

for manifest in "${manifests[@]}"; do
    name=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | head -n 1)
    [[ -n "$name" ]] || fail "$manifest has no package name"

    if violation=$(manifest_violation "$manifest" "$name"); then
        fail "$manifest $violation"
    fi

    if ! tree=$(cargo tree --locked -p "$name" --edges normal,build,dev 2>&1); then
        fail "cargo tree --locked -p $name failed: $tree"
    fi
    if violation=$(tree_violation "$name" "$tree"); then
        fail "$name $violation"
    fi
    echo "PASS: $name boundaries"
done

echo "PASS: engine boundaries"
