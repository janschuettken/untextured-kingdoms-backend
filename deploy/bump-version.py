#!/usr/bin/env python3
"""Zählt die Version des Servers hoch (Cargo.toml [workspace.package] und Cargo.lock).

Aufruf:  python3 deploy/bump-version.py [patch|minor|major]   (Standard: patch)
Gibt die neue Version aus. Läuft automatisch bei jedem Push auf dev
(.github/workflows/version.yml); von Hand nur für einen größeren Sprung (minor/major).
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGES = ("server", "protocol")


def bump(version: str, part: str) -> str:
    major, minor, patch = (int(x) for x in version.split("."))
    if part == "major":
        return f"{major + 1}.0.0"
    if part == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def main() -> None:
    part = sys.argv[1] if len(sys.argv) > 1 else "patch"
    if part not in ("patch", "minor", "major"):
        sys.exit(f"Unbekannt: {part} (patch, minor oder major)")

    toml_path = ROOT / "Cargo.toml"
    toml = toml_path.read_text(encoding="utf-8")
    m = re.search(r'(\[workspace\.package\][^\[]*?\nversion\s*=\s*")(\d+\.\d+\.\d+)(")', toml)
    if not m:
        sys.exit("Keine Version unter [workspace.package] in Cargo.toml gefunden")
    old, new = m.group(2), bump(m.group(2), part)
    toml_path.write_text(toml[: m.start(2)] + new + toml[m.end(2) :], encoding="utf-8")

    lock_path = ROOT / "Cargo.lock"
    lock = lock_path.read_text(encoding="utf-8")
    for name in PACKAGES:
        lock, n = re.subn(
            rf'(\[\[package\]\]\nname = "{name}"\nversion = ")[^"]*(")', rf"\g<1>{new}\g<2>", lock
        )
        if n != 1:
            sys.exit(f"Paket {name} nicht (eindeutig) in Cargo.lock gefunden")
    lock_path.write_text(lock, encoding="utf-8")
    print(new)
    print(f"{old} -> {new}", file=sys.stderr)


if __name__ == "__main__":
    main()
