#!/usr/bin/env python3
"""Verify the durable M014 human-UAT package.

The runbook (`docs/UAT-M014.md`) is M014's only remaining human acceptance
surface. This checker proves the package around it is internally sound:

* the runbook has 11 numbered checklist headings and 11 owner verdict rows,
  declares human-experience UAT mode, names all three mining multipliers in
  the item-2 table, and has a filled build/revision table;
* the `creator-uat` fixture preflight passes in an isolated temp cwd:
  default `--check-config` (six files, pinned digest), `--validate-pack`
  (13 entries), the six safe live edits (pinned digest), and the invalid
  lone-wood mining edit (pinned cross-field rejection);
* the release binary rejects both creator switches without creating
  settings, saves, packs, or config.

It never launches a window and never reads `.gsd/`. The human verdict itself
stays a dated owner record: a NEEDS-HUMAN runbook passes as "human pass
pending", while a complete dated owner PASS passes as "owner PASS recorded".

Invocation: python3 scripts/verify_m014_uat.py
"""

import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUNBOOK = ROOT / "docs" / "UAT-M014.md"
DEBUG = ROOT / "target" / "debug" / "opencraft"
RELEASE = ROOT / "target" / "release" / "opencraft"
CONFIG_FILES = (
    "player.toml",
    "survival.toml",
    "combat.toml",
    "world.toml",
    "mining.toml",
    "render.toml",
)
DEFAULT_DIGEST = "0x1bf613464d108311"
EDITED_DIGEST = "0x18d98c480e113ab1"
DEFAULT_SUMMARY = (
    "config: files=6 missing=0 warnings=0 errors=0 keys=86 schema=1 digest="
    + DEFAULT_DIGEST
)
EDITED_SUMMARY = (
    "config: files=6 missing=0 warnings=0 errors=0 keys=86 schema=1 digest="
    + EDITED_DIGEST
)
INVALID_MINING_ERROR = (
    "error: config/mining.toml: tools.stone_speed_multiplier: tool speed "
    "multipliers must satisfy tools.hand_speed_multiplier (1.0) <= "
    "tools.wood_speed_multiplier (16.0) <= tools.stone_speed_multiplier (3.0) <= "
    "tools.iron_speed_multiplier (5.0)"
)
SAFE_EDITS = (
    ("config/player.toml", "walk_speed = 4", "walk_speed = 8.0"),
    (
        "config/survival.toml",
        "hunger_drain_secs = 30.0",
        "hunger_drain_secs = 2.0",
    ),
    ("config/combat.toml", "reach = 3", "reach = 8.0"),
    ("config/world.toml", "day_seconds = 1200.0", "day_seconds = 120.0"),
    ("config/mining.toml", "table_reach = 8.0", "table_reach = 16.0"),
    (
        "config/render.toml",
        "selected_name_secs = 2.0",
        "selected_name_secs = 10.0",
    ),
)


class Failure(Exception):
    """A deterministic package failure with a path-free reason."""


def fail(reason):
    raise Failure(reason)


def read_text(path):
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        fail("cannot read {}: {}".format(path.name, type(exc).__name__))


def run(binary, cwd, *args):
    """Run one headless binary invocation without a display server."""
    command = [
        "env",
        "-u",
        "DISPLAY",
        "-u",
        "WAYLAND_DISPLAY",
        str(binary),
    ]
    command.extend(args)
    try:
        return subprocess.run(
            command,
            cwd=str(cwd),
            text=True,
            capture_output=True,
            timeout=120,
        )
    except subprocess.TimeoutExpired:
        fail("{} timed out".format(binary.name))
    except OSError as exc:
        fail("cannot run {}: {}".format(binary.name, type(exc).__name__))


def table_value(text, field):
    match = re.search(
        r"^\|\s*" + re.escape(field) + r"\s*\|\s*(.*?)\s*\|\s*$",
        text,
        re.MULTILINE,
    )
    if not match:
        return None
    return match.group(1).strip().strip("`").strip()


def item_two_section(text):
    match = re.search(
        r"^###\s+2\.\s.*?(?=^###\s+3\.\s)",
        text,
        re.MULTILINE | re.DOTALL,
    )
    if not match:
        fail("runbook checklist item 2 section is missing")
    return match.group(0)


def check_runbook():
    """Check runbook structure. Return True when a valid owner PASS is recorded."""
    if not RUNBOOK.is_file():
        fail("docs/UAT-M014.md is missing")
    text = read_text(RUNBOOK)

    headings = [int(value) for value in re.findall(r"^###\s+(\d+)\.\s", text, re.MULTILINE)]
    if headings != list(range(1, 12)):
        fail("runbook checklist headings are not exactly 1 through 11")

    verdict_rows = re.findall(
        r"^\|\s*Item\s+(\d+)\s+—[^|]*\|\s*([^|]*?)\s*\|\s*$",
        text,
        re.MULTILINE,
    )
    verdict_numbers = [int(number) for number, _value in verdict_rows]
    if verdict_numbers != list(range(1, 12)):
        fail("runbook owner verdict block is not exactly Item 1 through Item 11")

    if "UAT mode: human-experience" not in text:
        fail("runbook is missing the UAT mode: human-experience declaration")

    for name in (
        "wood_speed_multiplier",
        "stone_speed_multiplier",
        "iron_speed_multiplier",
    ):
        if name not in text:
            fail("runbook mining guidance does not name " + name)

    section_two = item_two_section(text)
    mining_rows = [
        line for line in section_two.splitlines() if line.startswith("| Mining |")
    ]
    if len(mining_rows) != 1:
        fail("runbook item 2 must have exactly one Mining row")
    for name in (
        "wood_speed_multiplier",
        "stone_speed_multiplier",
        "iron_speed_multiplier",
    ):
        if name not in mining_rows[0]:
            fail("runbook Mining row does not edit " + name)
    invariant = (
        "tools.wood_speed_multiplier <= tools.stone_speed_multiplier <= "
        "tools.iron_speed_multiplier"
    )
    if invariant not in section_two:
        fail("runbook item 2 does not state the wood <= stone <= iron invariant")

    for placeholder in ("<fill: sha256sum", "<fill: git rev-parse"):
        if placeholder in text:
            fail("runbook build table still contains a " + placeholder + " placeholder")
    for field in ("Debug SHA-256", "Release SHA-256"):
        value = table_value(text, field)
        if value is None or not re.fullmatch(r"[0-9a-f]{64}", value):
            fail("runbook {} is not a filled SHA-256".format(field))
    revision = table_value(text, "Revision under test")
    if revision is None or not re.fullmatch(r"[0-9a-f]{7,40}", revision):
        fail("runbook Revision under test is not a filled git revision")

    status_match = re.search(r"^\*\*Status:\s*([A-Za-z-]+)", text, re.MULTILINE)
    if not status_match:
        fail("runbook status line is missing")
    status = status_match.group(1).upper()
    if status == "NEEDS-HUMAN":
        return False
    if status != "PASS":
        fail("runbook status is neither NEEDS-HUMAN nor PASS")

    date = table_value(text, "Date")
    if date is None or not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date):
        fail("runbook status is PASS but Date is not a filled ISO date")
    operator = table_value(text, "Operator")
    if not operator:
        fail("runbook status is PASS but Operator is not filled")
    if operator.lower() in ("agent", "autonomous", "pending"):
        fail("runbook status is PASS but Operator is not a named human")
    for number, value in verdict_rows:
        if value.strip().strip("`").strip().upper() != "PASS":
            fail("runbook status is PASS but Item {} is not PASS".format(number))
    return True


def write_png(path, rgba):
    """Write one valid 16x16 RGBA PNG using only the standard library."""
    width = height = 16
    raw = b"".join(b"\x00" + bytes(rgba) * width for _ in range(height))

    def chunk(tag, data):
        body = tag + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def build_fixture(work):
    """Stage the creator-uat package exactly as the runbook describes."""
    config_dir = work / "config"
    config_dir.mkdir()
    for name in CONFIG_FILES:
        source = ROOT / "config" / name
        if not source.is_file():
            fail("checked-in config/" + name + " is missing")
        shutil.copy2(source, config_dir / name)

    pack_source = ROOT / "tests" / "fixtures" / "packs" / "mymod-ruby"
    if not pack_source.is_dir():
        fail("tests/fixtures/packs/mymod-ruby is missing")
    pack = work / "packs" / "creator-uat"
    shutil.copytree(pack_source, pack)
    with (pack / "pack.toml").open("a", encoding="utf-8") as handle:
        handle.write('\n[builtin_icons]\napple = "apple.png"\n')

    write_png(pack / "stone.png", (220, 60, 60, 255))
    write_png(pack / "pig_body.png", (60, 180, 90, 255))
    write_png(pack / "apple.png", (40, 90, 240, 255))

    (work / "settings.toml").write_text(
        '[packs]\nstack = ["creator-uat"]\n',
        encoding="utf-8",
    )


def check_default_fixture(work):
    """Prove the untouched fixture loads six files with the default digest."""
    result = run(DEBUG, work, "--check-config")
    if result.returncode != 0:
        fail("default --check-config exited {}, expected 0".format(result.returncode))
    lines = result.stdout.splitlines()
    if len(lines) != 7:
        fail("default --check-config printed {} lines, expected 7".format(len(lines)))
    if any(not line.startswith("ok: config/") for line in lines[:6]):
        fail("default --check-config did not print six ok: config/ lines")
    if lines[6] != DEFAULT_SUMMARY:
        fail("default --check-config summary is not {}".format(DEFAULT_SUMMARY))

    result = run(DEBUG, work, "--validate-pack", "creator-uat")
    if result.returncode != 0:
        fail("--validate-pack exited {}, expected 0".format(result.returncode))
    lines = result.stdout.splitlines()
    if "ok: builtin_icon apple" not in lines:
        fail("--validate-pack did not accept the builtin_icon apple override")
    summary = "validate-pack: creator-uat: 13 entries, 0 errors, 0 warnings"
    if lines.count(summary) != 1:
        fail("--validate-pack summary is not {}".format(summary))


def apply_line_edit(work, relative, old_line, new_line):
    path = work / relative
    text = read_text(path)
    pattern = re.compile(r"^" + re.escape(old_line) + r"$", re.MULTILINE)
    if len(pattern.findall(text)) != 1:
        fail("fixture edit anchor is not unique in " + relative)
    path.write_text(pattern.sub(lambda _match: new_line, text, count=1), encoding="utf-8")


def check_safe_edits(work):
    """Apply the six smoke edits and prove the pinned non-default digest."""
    for relative, old_line, new_line in SAFE_EDITS:
        apply_line_edit(work, relative, old_line, new_line)
    result = run(DEBUG, work, "--check-config")
    if result.returncode != 0:
        fail("edited --check-config exited {}, expected 0".format(result.returncode))
    lines = result.stdout.splitlines()
    if len(lines) != 7:
        fail("edited --check-config printed {} lines, expected 7".format(len(lines)))
    if lines[6] != EDITED_SUMMARY:
        fail("edited --check-config summary is not {}".format(EDITED_SUMMARY))


def check_invalid_mining(work):
    """A lone wood=16 edit must reject the whole file at the cross-field invariant."""
    apply_line_edit(
        work,
        "config/mining.toml",
        "wood_speed_multiplier = 1.5",
        "wood_speed_multiplier = 16",
    )
    result = run(DEBUG, work, "--check-config")
    if result.returncode != 1:
        fail("invalid-mining --check-config exited {}, expected 1".format(result.returncode))
    if INVALID_MINING_ERROR not in result.stdout.splitlines():
        fail("invalid-mining --check-config did not print the exact cross-field error")


def check_release_rejection(work):
    """Release must reject both creator switches without startup side effects."""
    release_dir = work / "release"
    release_dir.mkdir()
    expectations = (
        ("--check-config", "check-config: dev build only"),
        ("--creator-trace", "creator-trace: dev build only"),
    )
    for switch, expected in expectations:
        result = run(RELEASE, release_dir, switch)
        if result.returncode != 1:
            fail("release {} exited {}, expected 1".format(switch, result.returncode))
        if result.stdout.splitlines() != [expected]:
            fail("release {} did not print exactly one dev-only line".format(switch))
        if result.stderr:
            fail("release {} wrote to stderr".format(switch))
    for name in ("settings.toml", "saves", "packs", "config"):
        if (release_dir / name).exists():
            fail("release rejection created " + name)


def main():
    try:
        owner_recorded = check_runbook()
        if not DEBUG.is_file():
            fail("target/debug/opencraft is missing; build both profiles first")
        if not RELEASE.is_file():
            fail("target/release/opencraft is missing; build both profiles first")

        work = None
        try:
            work = Path(tempfile.mkdtemp(prefix="m014-uat-"))
        except OSError as exc:
            fail("cannot create the temp fixture directory: " + type(exc).__name__)
        try:
            try:
                build_fixture(work)
            except OSError as exc:
                fail("cannot stage the creator-uat fixture: " + type(exc).__name__)
            check_default_fixture(work)
            check_safe_edits(work)
            check_invalid_mining(work)
            check_release_rejection(work)
        finally:
            shutil.rmtree(work, ignore_errors=True)
    except Failure as exc:
        print("M014 UAT PACKAGE FAIL: " + str(exc))
        return 1
    except Exception as exc:  # pragma: no cover - deliberate deterministic fallback
        print("M014 UAT PACKAGE FAIL: unexpected " + type(exc).__name__)
        return 1

    if owner_recorded:
        print("M014 UAT PACKAGE PASS (owner PASS recorded)")
    else:
        print("M014 UAT PACKAGE PASS (human pass pending)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
