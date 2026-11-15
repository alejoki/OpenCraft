#!/usr/bin/env bash
# Fail the build if an engine crate reaches back into the game or picks up
# dependencies it must not own (M017/S01).
#
# Rules for every crates/opencraft-* member:
#   - no dependency on the root `opencraft` game package (direct or transitive)
#   - publish = false and no version.workspace (the release guard reads the
#     root package version; members version independently)
#   - opencraft-core, opencraft-physics, opencraft-pathfinding: no wgpu,
#     wgpu-core, wgpu-types, winit or raw-window-handle
#   - opencraft-gfx: no winit (wgpu is allowed), and no direct winit or
#     raw-window-handle manifest entry
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
        opencraft-core|opencraft-physics|opencraft-pathfinding)
            if grep -Eq '(^|[[:space:]])(wgpu|wgpu-core|wgpu-types|winit|raw-window-handle) v[0-9]' <<<"$tree"; then
                echo "depends on a GPU/windowing crate"
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

    echo "PASS: boundary self-test"
}

echo "engine boundaries: repo=$REPO_ROOT"

if ! cargo metadata --locked --no-deps --format-version 1 >/dev/null; then
    fail "cargo metadata --locked failed (stale Cargo.lock or malformed manifest)"
fi

for crate in opencraft-core opencraft-gfx opencraft-physics opencraft-pathfinding; do
    [[ -f "crates/$crate/Cargo.toml" ]] || fail "missing crates/$crate/Cargo.toml"
done

self_test

shopt -s nullglob
manifests=(crates/opencraft-*/Cargo.toml)
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
