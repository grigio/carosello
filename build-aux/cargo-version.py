#!/usr/bin/env python3
"""Print the package version from Cargo.toml (single source of truth).

Used by meson.build so the Meson project version always matches Cargo:
    version: run_command('python3',
        files('build-aux/cargo-version.py'), files('Cargo.toml'),
        check: true).stdout().strip(),
"""

import sys
import tomllib

with open(sys.argv[1], "rb") as f:
    sys.stdout.write(tomllib.load(f)["package"]["version"])
