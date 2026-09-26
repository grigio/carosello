# Carosello

Fast, minimalist image and video viewer for Linux, written in Rust with GTK4/libadwaita.

![Carosello Screenshot](carosello.gif)

## Features

- Navigates with arrows or gestures, three-finger swipe (optional two-finger); slides when the prefetched frame is ready, cuts otherwise
- Decodes on worker threads, prefetches neighbors, applies EXIF orientation, fits to window with aspect ratio kept
- Zooms with keyboard, pinch, or double-click; double-click anchors at the pointer, drag pans while zoomed
- Rotates left/right and mirrors in place from header buttons; JPEG re-encoded at q95 with EXIF normalized, no undo; animated GIF/WebP and video are view-only
- Plays video muted, autoplaying, looped, with play/pause, seek, volume, mute, elapsed and total time
- Deletes with `Delete`: Trash first, direct delete where Trash is unsupported (remote mounts, portal paths)
- Auto-hides header and video controls, supports fullscreen.

## Supported formats

| Type | Formats |
|------|---------|
| Images | JPEG (`.jpg`, `.jpeg`), PNG, WebP, GIF, BMP, TIFF (`.tif`, `.tiff`) |
| Video | MP4, WebM, Matroska (`.mkv`) |

Extensions match case-insensitively. Image decoding reads file contents; video codecs come from installed GStreamer plugins.

## Installation

### Arch Linux (build from source)

Not in the AUR. A per-user Meson install under `~/.local` avoids touching Pacman files and needs no `sudo` to install.

Install the build and runtime dependencies:

```bash
sudo pacman -S --needed \
  base-devel git meson rust gtk4 libadwaita \
  hicolor-icon-theme desktop-file-utils
```

For broad video-codec support, also install GStreamer's common plugin sets:

```bash
sudo pacman -S --needed \
  gstreamer gst-plugins-good gst-plugins-bad \
  gst-plugins-ugly gst-libav
```

Clone, build, and install under `~/.local`:

```bash
git clone https://github.com/grigio/carosello.git
cd carosello
meson setup builddir --prefix="$HOME/.local"
meson compile -C builddir
meson install -C builddir
```

Make sure `~/.local/bin` is in your `PATH`, then launch Carosello from the
application menu or run:

```bash
carosello
```

This installs binary, desktop entry, AppStream metadata, and icon under `~/.local`. After pulling changes, rerun:

```bash
meson compile -C builddir
meson install -C builddir
```

### Flatpak artifact

Download the latest `.flatpak` file from
[GitHub Releases](https://github.com/grigio/carosello/releases) and install it:

```bash
flatpak install --user ./carosello.flatpak
flatpak run io.github.grigio.carosello
```

CI artifacts are x86_64 only.

### Flatpak from this source tree

Needs GNOME 50 runtime plus freedesktop 25.08 Rust extension:

```bash
sudo pacman -S --needed flatpak flatpak-builder
flatpak install --user flathub \
  org.gnome.Platform//50 org.gnome.Sdk//50 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08
```

Also generate the ignored `cargo-sources.json` from `Cargo.lock` with the `Generate Cargo sources for Flatpak` command in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml), then:

```bash
flatpak-builder --user --install --force-clean \
  build-dir io.github.grigio.carosello.yml
```

The Flatpak has host and GVfs read/write access for remote folders, in-place edits, and Trash. A single file picked through the document portal exposes only that file; use Open Folder… to browse siblings.

### Nix (flake)

No packaging step, version comes from `Cargo.toml`:

```bash
nix run github:grigio/carosello              # run straight from GitHub
nix profile install github:grigio/carosello  # install
```

From a checkout: `nix build` / `nix run`. Only `flake.lock` moves, via a weekly workflow that builds before opening the PR; every push also builds through the `Nix` workflow.

### Cargo development build

To compile without installing desktop integration:

```bash
cargo build --release
./target/release/carosello
```

## Usage

```bash
# View supported media in the current directory
carosello

# View supported media in a specific directory
carosello /path/to/directory

# View a specific local file and its supported siblings
carosello /path/to/image.jpg
```

Takes one positional path; a second launch presents the existing window. Open… (`Ctrl+O`) picks a file, Open Folder… (`Ctrl+Shift+O`) picks a directory. Under the Flatpak portal, Open Folder… is the way to get siblings and a writable export.

## Image editing

Header buttons rotate left/right and mirror. Each edit overwrites the original and refreshes the view, with JPEG orientation normalized so reloads do not rotate twice. Edits are destructive with no undo, and JPEG bytes change even for a reversed operation.

## Preferences

In the app menu. Slide animation defaults on (falls back to an instant cut if the frame is not prefetched); two-finger swipe defaults off (replaces three-finger nav when on). Stored in `settings.conf` under `~/.config/carosello`.

## File association

The desktop entry registers JPEG, PNG, WebP, GIF, BMP, TIFF, MP4, WebM, and Matroska types, mirrored in AppStream `<provides>` plus legacy BMP/TIFF aliases.

To make it the default for all supported types:

```bash
xdg-mime default io.github.grigio.carosello.desktop \
  image/jpeg image/png image/webp image/gif image/bmp \
  image/x-bmp image/x-ms-bmp image/tiff image/x-tiff \
  video/mp4 video/webm video/x-matroska
```

Or set one type through GIO:

```bash
gio mime image/jpeg io.github.grigio.carosello.desktop
```

## Keyboard shortcuts

### Navigation

| Key | Action |
|-----|--------|
| `Left` / `Right` | Previous / next file |
| `Page Up` / `Page Down` | Previous / next file |
| `Home` / `End` | First / last file |

### Zoom and view

| Key | Action |
|-----|--------|
| `Ctrl++` / `Ctrl+=` | Zoom in |
| `Ctrl+-` / `Ctrl+_` | Zoom out |
| `Ctrl+0` | Reset zoom to fit |
| `Escape` | Exit fullscreen; otherwise reset zoom when zoomed |
| `Double-click` | Toggle between fit and 2.5× zoom at the pointer |
| `F11` / `F` | Toggle fullscreen |

### Application and files

| Key | Action |
|-----|--------|
| `Ctrl+O` | Open a file |
| `Ctrl+Shift+O` | Open a folder |
| `Delete` | Move to Trash, or delete directly if the filesystem has no Trash |
| `Ctrl+?` / `Ctrl+/` / `Ctrl+K` | Show keyboard shortcuts |
| `F1` | About Carosello |
| `Ctrl+W` | Close the window |
| `Ctrl+Q` | Quit |

### Video controls

| Key | Action |
|-----|--------|
| `Space` / `K` | Play / pause |
| `M` | Mute / unmute |
| `[` | Seek backward 5 seconds |
| `]` | Seek forward 5 seconds |
| Click seek bar | Seek to position |

## Gestures

| Gesture | Action |
|---------|--------|
| Pinch | Zoom in or out |
| `Ctrl` + scroll | Zoom in or out |
| Scroll | Pan while zoomed; horizontal two-finger scroll can navigate at fit when enabled in Preferences |
| Three-finger swipe left | Next file |
| Three-finger swipe right | Previous file |
| Drag while zoomed | Pan around the image or video |
| Double-click | Toggle fit and pointer-anchored 2.5× zoom |
| Drag and drop | Open dropped files or directories |

With Two-finger swipe on, two-finger gestures replace three-finger ones.

## Design principles

Fast (decode off the UI thread, prefetch neighbors, never block on a missing frame), minimal (no browser, thumbnails, or metadata viewer), keyboard-first (`Ctrl` + scroll, pinch, shortcuts for zoom).

## Tech stack

Rust, GTK4 + libadwaita, `image` crate on worker threads, GDK `MemoryTexture`, GTK `MediaFile` with GStreamer, kamadak-exif.

## License

GPL-3.0-or-later

## Donations

- Monero: `88LyqYXn4LdCVDtPWKuton9hJwbo8ZduNEGuARHGdeSJ79BBYWGpMQR8VGWxGDKtTLLM6E9MJm8RvW9VMUgCcSXu19L9FSv`
- Bitcoin: `bc1q6mh77hfv8x8pa0clzskw6ndysujmr78j6se025`
