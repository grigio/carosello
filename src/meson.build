cargo_options = ['--manifest-path', meson.project_source_root() / 'Cargo.toml']
cargo_options += ['--target-dir', meson.project_build_root() / 'target']

if get_option('profile') == 'dev'
  cargo_options += ['--debug']
  cargo_env = ['CARGO_HOME=' + meson.project_build_root() / 'cargo-home']
else
  cargo_options += ['--release']
  cargo_env = ['CARGO_HOME=' + meson.project_build_root() / 'cargo-home']
endif

cargo_build = custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: bindir,
  command: [
    'env', cargo_env,
    meson.project_source_root() / 'build-aux/cargo.sh',
    meson.project_name(),
    meson.current_source_dir(),
    '@OUTPUT@',
  ] + cargo_options,
)
