# AGENTS.md — Carosello

## Flatpak permissions

- GVfs needs **both** entries in `finish-args`, otherwise every `GFile`
  call logs `GVFS-WARNING ... missing --filesystem=xdg-run/gvfsd privileges`:
  `--filesystem=xdg-run/gvfs:ro` (mount points) +
  `--filesystem=xdg-run/gvfsd` (daemon sockets, no `:ro`).
- `--filesystem=host:rw` lets `read_dir(parent)` list siblings of an
  opened file. Narrower grants break sibling navigation. Read-write (not
  `:ro`) is required so Move to Trash (`GFile` trash) can delete.
- **Document portal limitation** (`$XDG_RUNTIME_DIR/doc/…`): double-clicking
  a file in Files, or picking a single file via the Open portal, exports
  **only that file** — its portal parent always lists 1 item, so "1 of 1"
  is by design, not a listing bug. Workarounds:
  - "Open Folder…" uses `FileDialog::select_folder`, which exports the
    whole directory (siblings visible).
  - Launching with a real path (`flatpak run … ~/Pictures/Dir`) works via
    `host:rw`.
  - `is_doc_portal_path()` detects portal paths; `show_file()` toasts a
    hint instead of failing silently.
- **Sandboxed drag-and-drop also arrives via the portal**: drops from Files
  come as `file:///run/user/1000/doc/<unique-id>/<name>`, each file in its
  **own** id dir — parent listing can never find siblings. The drop handler
  therefore browses all dropped files for portal paths (local drops still
  list the parent folder). Diagnose with `CAROSELLO_DEBUG=1`: the `drop:
  uri=…` + `collect_media: N media in …` lines show the scheme and which
  branch ran.
- **Dropped folders define their own scope**: accept `is_dir()` drops (they
  carry no media extension, so an `is_media`-only filter rejects them
  silently) and list each via `collect_dir_media()` (read_dir + GIO
  fallback). Verified: the portal exports a dropped folder's full subtree
  (12- and 64-photo sftp folders listed fine). `start_index` must handle
  `paths` being empty (folder-only drop).
- No new crates / system deps without regenerating `cargo-sources.json`
  (CI regenerates it from `Cargo.lock`; keep the lockfile committed).

## Versioning

- `Cargo.toml` is the single source of truth. `meson.build` derives its
  version via `build-aux/cargo-version.py`; the About dialog uses
  `env!("CARGO_PKG_VERSION")`. Never hardcode a version in `meson.build`.
- Bump with `build-aux/bump-version.sh <ver> ["notes"]` — updates
  `Cargo.toml`, prepends the metainfo `<release>`, sets `PKGBUILD` pkgver,
  regenerates `.SRCINFO`. Flatpak needs nothing (version shown comes from
  the metainfo releases). Then tag `v<ver>` and run `updpkgsums`.
- CI `versions` job fails on any drift between Cargo / metainfo / PKGBUILD /
  .SRCINFO / git tag.

## Panics / RefCell

- Never `if let Some(x) = state.borrow_mut().map.remove(k)` and then
  borrow `state` again in the body: the `RefMut` temporary lives for the
  whole `if`, so the inner borrow **panics**. Split into two statements.
  Same applies to `state.borrow().field` + `ref` bindings in scrutinees.
- `gdk_pixbuf::Pixbuf` is **not `Send`**: no `std::thread::spawn` with
  Pixbuf/`Rc<AppState>` captures. Use GIO async (`File::read_async` +
  `Pixbuf::from_stream_async`) — decode leaves the UI thread, everything
  stays on the main context.
- Release profile strips symbols (`strip`, `panic=abort`); to debug a
  flatpak crash, reproduce with the dev binary + `RUST_BACKTRACE=1`
  (Wayland is available, headless launch works).

## Verify

- `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
  (CI runs exactly this; `-D warnings` turns the prefetch-style `dead_code`
  and `unnecessary_sort_by` lints into failures).
- Reinstall: `flatpak-builder --user --install --force-clean build-dir
  io.github.grigio.carosello.yml`
- Smoke test headless, check stderr is empty (no panic, no GVFS warnings):
  `timeout 10 flatpak run io.github.grigio.carosello ~/Pictures/Screenshots`
  plus a portal path under `/run/user/1000/doc/…`.
