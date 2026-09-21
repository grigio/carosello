project(
  'carosello',
  version: '1.0.6',
  meson_version: '>= 0.62.0',
  default_options: [
    'warning_level=2',
  ],
)

i18n = import('i18n')

gnome = import('gnome')

base_id = 'io.github.grigio.carosello'

dependency('glib-2.0', version: '>= 2.80')
dependency('gio-2.0', version: '>= 2.80')
dependency('gtk4', version: '>= 4.14.0')
dependency('libadwaita-1', version: '>= 1.6')

version = meson.project_version()

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
libdir = prefix / get_option('libdir')
localedir = datadir / 'locale'
pkgdatadir = datadir / meson.project_name()
pkglibdir = libdir / meson.project_name()
iconsdir = datadir / 'icons'
podir = meson.project_source_root() / 'po'
desktop_file = pkgdatadir / '@0@.desktop'.format(base_id)
appstream_file = pkgdatadir / '@0@.metainfo.xml'.format(base_id)
resources = gnome.compile_resources(
  '@0@-resources'.format(meson.project_name()),
  'data/@0@.gresource.xml'.format(base_id),
  gresource_bundle: true,
  install: true,
  install_dir: pkgdatadir,
  source_dir: meson.project_source_root() / 'data',
  dependencies: [],
)

subdir('data')
subdir('src')

meson.add_install_script(
  'build-aux/meson/postinstall.py',
  meson.project_name(),
)
