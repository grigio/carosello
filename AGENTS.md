# AGENTS.md — Carosello

## Flatpak permissions

- GVfs needs **both** entries in `finish-args`, otherwise every `GFile`
  call logs `GVFS-WARNING ... missing --filesystem=xdg-run/gvfsd privileges`:
  `--filesystem=xdg-run/gvfs` (mount points, **read-write**) +
  `--filesystem=xdg-run/gvfsd` (daemon sockets, no `:ro`).
  `:ro` remounts the gvfs mount read-only inside the sandbox, which
  breaks the Move to Trash fallback (direct delete on filesystems with
  no Trash: sftp/smb return `G_IO_ERROR_NOT_SUPPORTED`, code 15) and
  in-place transforms on remote mounts.
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
  - Portal exports are **ephemeral**: `doc/<id>` goes stale the moment the
    exporting app closes (`gio info` → "No such file or directory").
    Never reuse an old id for the portal smoke test — re-export.
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
- **Flatpak has a private `/tmp`**: paths under `/tmp/…` are invisible
  inside the sandbox (the app shows "No Images Found"). Test media must
  live under `~/` (`host:rw`), or pass a real host path only if it is
  outside `/tmp`.
- No new crates / system deps without regenerating `cargo-sources.json`
  (CI regenerates it from `Cargo.lock`; keep the lockfile committed).
  The file itself is **gitignored** — before a local `flatpak-builder`,
  regenerate it with the Python snippet from `.github/workflows/ci.yml`
  (tomllib reads `Cargo.lock`, emits archive+checksum entries).

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
- **Runtime bump = 2 places**: `runtime-version: 'NN'` in the manifest and
  the CI image `ghcr.io/flathub-infra/flatpak-github-actions:gnome-NN`
  (same NN; `gnome-49/50/51` all exist on ghcr). Then
  `flatpak install --user flathub org.gnome.Platform//NN org.gnome.Sdk//NN`.
  `sdk-extensions: rust-stable` resolves against the **freedesktop** SDK
  branch underneath GNOME, *not* GNOME's number: GNOME 50 → freedesktop
  25.08 → `rust-stable//25.08`. There is no `rust-stable//50` — asking
  for it fails with "Can't find ref".

## Panics / RefCell

- Never `if let Some(x) = state.borrow_mut().map.remove(k)` and then
  borrow `state` again in the body: the `RefMut` temporary lives for the
  whole `if`, so the inner borrow **panics**. Split into two statements.
  Same applies to `state.borrow().field` + `ref` bindings in scrutinees.
- `gdk_pixbuf::Pixbuf` / `gdk::Texture` are **not `Send`**: no
  `std::thread::spawn` with Pixbuf/Texture/`Rc<AppState>` captures.
  Workers exchange plain `transform::Decoded { rgba, w, h }` bytes only
  (see Decode / workers below); textures are built on the main context.

## Decode / workers / logging

- **Display decode runs on a worker `std::thread`** (`spawn_decode` /
  `spawn_frame_worker` in `window.rs`): `std::fs::read` +
  `transform::decode_frame` (image-crate decode + EXIF orientation baked
  in) produce plain bytes; only the main thread wraps them in a
  `gdk::MemoryTexture` (`frame_texture`). Never assume `*_async` GIO or
  the old gdk-pixbuf stream callback decodes off-thread — they ran the
  actual decompression **on the UI thread** (that was IMPROVEMENTS #1).
- glib has no `MainContext::channel` and `idle_add` needs `Send`, so
  workers publish into an `Arc<Mutex<Option<Result<…>>>>` slot polled by
  `glib::timeout_add_local(Duration::from_millis(16), …)` (~30 s /
  1875-tick deadline) — the same pattern as the transform worker.
  Rounds are cancelled with `gio::Cancellable` (`state.decode_cancel` /
  `state.prefetch_cancel`, superseded on every navigation) plus the
  `image_gen` generation guard.
- **Media/slide signal closures must capture weakly** (`Rc::downgrade
  (&state)`, `widget.downgrade()`, `SlideCtxWeak`): `state → media_file →
  handler → state` and `media → handler → picture → video paintable →
  media` are real refcycles that used to leak a pipeline + widget graph
  per video. Prefetch holds at most **2 textures FIFO** (`prefetch_store`,
  oldest popped) and each navigation cancels the previous round; results
  re-check the target is still index±1 before storing.
- `debug_log` is a `macro_rules!` in `state.rs` re-exported via
  `pub(crate) use`, imported as `use crate::state::debug_log` and called
  with `!`: the `format!` argument lives *inside* the `debug_enabled()`
  gate (a `fn(&str)` formatted on every call even when disabled).
- Release profile strips symbols (`strip`, `panic=abort`); to debug a
  flatpak crash, reproduce with the dev binary + `RUST_BACKTRACE=1`
  (Wayland is available, headless launch works).

## Move to Trash / delete fallback

- "This filesystem has no Trash" is always `G_IO_ERROR_NOT_SUPPORTED`
  (code 15, `matches!(e.kind(), Some(gio::IOErrorEnum::NotSupported))`) —
  verified on a remote gvfs sftp mount (`Operation not supported`) and on
  `/tmp` (`Trashing on system internal mounts is not supported`). Every
  other trash error keeps the old toast; only code 15 falls through to
  `delete_current` (`delete_async`), and both success paths share
  `finish_removal` (drop entry → `index_after_removal` → `show_file`).
- Probe trash support **without the GUI**: `gio trash <file>` prints the
  message, or `python3` + `Gio.File.new_for_path(p).trash(None)` gives
  `e.code`/`e.domain` (PyGObject errors have `.code`, not `.gerror`).
  There is no `gio delete` — it's `gio remove`.
- `state.trashing` guards *both* phases: `delete_current` re-arms it, so a
  second Del during the fallback is still ignored.

## Verify

- `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
  (CI runs exactly this; `-D warnings` turns the prefetch-style `dead_code`
  and `unnecessary_sort_by` lints into failures).
- Reinstall: `flatpak-builder --user --install --force-clean build-dir
  io.github.grigio.carosello.yml`
- Smoke test headless, check stderr is empty (no panic, no GVFS warnings):
  `timeout 10 flatpak run io.github.grigio.carosello ~/Pictures/Screenshots`
  plus a portal path under `/run/user/1000/doc/…`. Run **without**
  `CAROSELLO_DEBUG` for this check — the var prints `[carosello-debug]`
  traces to stderr; use `CAROSELLO_DEBUG=1` separately to trace behavior
  (collect/drop/display/transform decisions).

## Transforms (rotate / mirror + autosave)

- Three flat header buttons (`object-rotate-left/right-symbolic`,
  `object-flip-horizontal-symbolic`), enabled only while showing a real
  image with no save in flight (`sync_transform_buttons`: video / empty
  folder / `state.saving` ⇒ disabled). **`AdwHeaderBar::pack_end`
  prepends** — pack order is the exact reverse of the wanted look; the
  pack block in `window.rs` documents the contract (visual order
  `[rotL][rotR][mirror][⊖][fit][⊕][≡]`).
- Pipeline = `src/transform.rs` (pure, unit-tested, Send-only data):
  decode → EXIF-orient → transform → encode. `image::imageops::rotate90`
  is **clockwise** (used for RotateRight). Saved pixels are
  display-oriented and the JPEG APP1 Orientation is patched back to `1`
  (thumbnail/IFD1 dropped) so a reload never double-rotates. JPEG
  re-encodes at q95 ⇒ bytes change even for an identity pair (R then L).
  Animated GIF/WebP are refused (`reject_animation`); videos never reach
  the pipeline (button disabled).
- Worker → main bridge: glib 0.20 has no `MainContext::channel` and
  `idle_add` needs `Send`, so the worker stores
  `Arc<Mutex<Option<Result<(Vec<u8>, Option<Permissions)>, String>>>>`
  and a `glib::timeout_add_local(Duration::from_millis(16), …)` poller
  takes it (signature: `(Duration, FnMut() -> ControlFlow)`), with a
  ~30 s / 1875-tick deadline. `glib` is **not a direct dependency** —
  reference it as `gtk::glib` (also in `transform.rs` tests).
- The write stays on the main context: `gio::File::replace_contents_async`
  (callback `Ok((contents, etag))` / `Err((contents, error))`, Rc captures
  fine). The atomic replace **resets file mode** — restore the
  `std::fs::Permissions` captured before the write. Failures are toasts
  (`Cannot read …` worker-side, `Cannot save …` write-side + portal hint
  via `is_doc_portal_path`).
- While `state.saving`: `trash_current` refuses (would recreate a trashed
  file at its old path) and `prefetch_neighbors` early-returns (a decode
  racing the replace could cache pre-transform pixels). On success: drop
  the stale `prefetch` entry, re-enable buttons, re-run `show_image()`
  only if the path is still the current index.

## Zoom anchor

- `GestureClick::pressed` x/y are **per-device** on Wayland while the pointer
  cursor is shared: clicking with a device that hasn't moved (touchpad after
  mouse, or vice versa) reports stale/`(0,0)` coords and the zoom lands
  top-left. When `device.has_cursor()`, anchor on the motion-tracked
  `mouse_x/mouse_y` (overlay coords == viewport coords, same as the pinch
  path); touch (no cursor) keeps event coords + `compute_point`. Diagnose
  with `CAROSELLO_DEBUG=1`: the `dblclick: press=… cursor=…` line shows both.

## GUI automation (interactive tests on this machine)

- `ydotoold` already runs in the user session:
  `export YDOTOOL_SOCKET=/run/user/1000/.ydotool_socket`.
  **`ydotool click 0x00` is a documented no-op — a real left click is
  `ydotool click 0xC0`** (0x40=down, 0x80=up). `mousemove` requires
  `-x/-y` flags; positional args print usage.
- grim captures the buffer at **3120×2080 with output scale 2.0** ⇒
  Wayland pointer/logical coords = grim pixels ÷ 2. Use `grim -c` (adds
  the cursor) and **close the loop with screenshots before every click**:
  synthetic relative moves overshoot ~15 % (libinput accel), so computed
  positions drift.
- Revealing Carosello's header: move into the **top (or bottom) 25 % of
  the window** (it hides again 3 s after motion outside those zones). The
  hovered button shows a grey pill — diffing it against a no-hover
  baseline PNG (grey rows where baseline is black) identifies the target
  button exactly; its center is the *button* center, not the cursor.
- `wtype -k Left` sends keys (window must be focused). Kill the app with
  `pkill -x carosello` — `pkill -f <path/pattern>` also matches your own
  shell's command line and kills the test script mid-flight.
- **Screenshots lie, logs don't**: labwc raises other windows (htop,
  Chromium) over Carosello between two `grim` shots, so a "window
  vanished" PNG proves nothing. Close the loop with
  `pgrep -cx carosello` + the `CAROSELLO_DEBUG=1` lines around every key
  (`fullscreen-exit: key Escape`, `trash failed for …`) instead — a
  launched window does take keyboard focus, so `wtype -k F11/Escape/
  Delete` reaches it without a click.
