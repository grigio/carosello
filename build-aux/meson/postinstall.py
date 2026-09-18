#!/usr/bin/env python3

import os
import subprocess

prefix = os.environ.get('MESON_INSTALL_PREFIX', '/usr/local')
datadir = os.path.join(prefix, 'share')
destdir = os.environ.get('DESTDIR', '')

# Package managers set this so we don't need to run
if not destdir:
    schemas_dir = os.path.join(datadir, 'glib-2.0', 'schemas')
    if os.path.isdir(schemas_dir):
        print('Compiling GSettings schemas...')
        subprocess.call(['glib-compile-schemas', schemas_dir])
    else:
        print('No GSettings schemas to compile (skipping)')

    print('Updating icon cache...')
    subprocess.call(['gtk-update-icon-cache', '-qtf',
                     os.path.join(datadir, 'icons', 'hicolor')])

    print('Updating desktop database...')
    subprocess.call(['update-desktop-database', '-q',
                     os.path.join(datadir, 'applications')])
