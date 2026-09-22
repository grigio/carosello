#!/bin/bash
# Bump the release version everywhere from the Cargo.toml single source of truth.
#
# Usage: build-aux/bump-version.sh 1.0.10 ["Release notes for the metainfo entry."]
#
# Updates:
#   Cargo.toml            (source of truth; Cargo.lock refreshes on next cargo build)
#   data/*.metainfo.xml   (prepends a <release> entry dated today)
#   PKGBUILD              (pkgver, resets pkgrel to 1)
#   .SRCINFO              (regenerated via makepkg --printsrcinfo)
#
# Flatpak needs nothing: it builds from the source dir and its user-visible
# version comes from the metainfo <release> entries. The in-app About dialog
# reads env!("CARGO_PKG_VERSION"), so it follows Cargo.toml automatically.
#
# After running: tag v$1, then run `updpkgsums` (tarball hash changes per tag).
set -euo pipefail

V="${1:?usage: bump-version.sh VERSION [NOTES]}"
NOTES="${2:-Release $V.}"
DATE="$(date +%F)"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
META="$ROOT/data/io.github.grigio.carosello.metainfo.xml"

# 1. Cargo.toml (source of truth)
sed -i "s/^version = \".*\"/version = \"$V\"/" "$ROOT/Cargo.toml"

# 2. Metainfo: prepend a <release> entry (kept in sync with Cargo by CI).
# Line-based insertion keeps the surrounding formatting untouched.
python3 - "$META" "$V" "$DATE" "$NOTES" <<'EOF'
import sys
from xml.sax.saxutils import escape
path, ver, date, notes = sys.argv[1:5]
with open(path) as f:
    lines = f.readlines()
if any(f'<release version="{ver}"' in line for line in lines):
    print(f"metainfo already has release {ver}, skipping")
    sys.exit(0)
entry = [
    f'    <release version="{ver}" date="{date}">\n',
    "      <description>\n",
    f"        <p>{escape(notes)}</p>\n",
    "      </description>\n",
    "    </release>\n",
]
idx = next(i for i, line in enumerate(lines) if "<releases>" in line)
lines[idx + 1:idx + 1] = entry
with open(path, "w") as f:
    f.writelines(lines)
print(f"metainfo: added release {ver}")
EOF

# 3. PKGBUILD + .SRCINFO (pkgver must stay literal for makepkg/AUR)
sed -i "s/^pkgver=.*/pkgver=$V/; s/^pkgrel=.*/pkgrel=1/" "$ROOT/PKGBUILD"
(cd "$ROOT" && makepkg --printsrcinfo > .SRCINFO)
echo "bumped to $V"
