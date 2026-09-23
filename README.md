# Carosello

A fast, minimalist image and video viewer for Linux.

Built with **Rust** and **GTK4/libadwaita** for native Wayland/X11 support.

![Carosello Screenshot](carosello.gif)

## Features

- Opens images and videos from a directory
- Navigates between files with keyboard or gestures
- Fit-to-window display with aspect ratio preservation
- Zoomable via pinch-to-zoom, scroll wheel, or keyboard
- Video playback with controls (play/pause, seek, volume, mute)
- Auto-hide controls for distraction-free viewing
- EXIF orientation handling for rotated images
- Drag and drop files or directories
- Toast notifications for load errors
- Fullscreen mode

## Supported Formats

| Type | Formats |
|------|---------|
| Images | JPEG, PNG, WebP, GIF, BMP, TIFF |
| Video | MP4, WebM, MKV |

## Installation

### Arch Linux

From the AUR with an AUR helper (direct install):

```bash
yay -S carosello
```

or:

```bash
paru -S carosello
```

Without a helper:

```bash
git clone https://aur.archlinux.org/carosello.git
cd carosello
makepkg -si
```

The AUR checkout contains the Arch packaging recipe and builds the
upstream release source.

The `PKGBUILD` builds with Meson + Cargo (`arch-meson build && meson compile -C build`)
and installs the binary, desktop entry, AppStream metadata, and icons.
`depends`: `gtk4`, `libadwaita`, `glib2`, `graphene`, `glibc`, `libgcc`, `hicolor-icon-theme`.
Video codecs come via GStreamer (`gst-plugins-good/bad/ugly`, `gst-libav` as optdepends).

### Flatpak (recommended)

Build and install the flatpak:

```bash
flatpak-builder --user --install --force-clean build-dir io.github.grigio.carosello.yml
```

### From GitHub artifact

Download the latest `.flatpak` file from [GitHub Releases](https://github.com/grigio/carosello/releases) and install it:

```bash
flatpak install --user carosello.flatpak
```

Run directly:

```bash
flatpak run io.github.grigio.carosello
```

### From source (Meson)

```bash
meson setup builddir
meson compile -C builddir
meson install -C builddir
```

### From source (Cargo)

```bash
cargo build --release
./target/release/carosello
```

## Usage

```bash
# View media in current directory
carosello

# View media in a specific directory
carosello /path/to/directory

# View a specific file (shows siblings)
carosello /path/to/image.jpg
```

## File association

Carosello registers as a handler for its supported image and video types
(`data/io.github.grigio.carosello.desktop` `MimeType`, mirrored in the
AppStream `<provides>` block), so after install it appears in
"Open With" for JPEG, PNG, WebP, GIF, BMP, TIFF, MP4, WebM, and MKV.

To make it the default for everything it supports:

```bash
xdg-mime default io.github.grigio.carosello.desktop \
  image/jpeg image/png image/webp image/gif image/bmp \
  image/x-bmp image/x-ms-bmp image/tiff image/x-tiff \
  video/mp4 video/webm video/x-matroska
```

Or per type via GIO:

```bash
gio mime image/jpeg io.github.grigio.carosello.desktop
```

## Keyboard Shortcuts

### Navigation

| Key | Action |
|-----|--------|
| `Left` / `Right` | Previous / Next file |
| `3-finger swipe left/right` | Previous / Next file (trackpad) |

### Zoom

| Key | Action |
|-----|--------|
| `+` / `=` / `Ctrl++` | Zoom in |
| `-` / `_` / `Ctrl+-` | Zoom out |
| `0` / `Ctrl+0` | Reset zoom to fit |
| `Escape` | Exit fullscreen, else reset zoom (if zoomed) |
| `Double-click` | Toggle between fit and 2.5x zoom |

### View

| Key | Action |
|-----|--------|
| `F11` / `F` | Toggle fullscreen (also exits with `Escape`) |

### Application

| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit |
| `Ctrl+W` | Close window |
| `Delete` | Move to Trash (deletes directly when the fs has no Trash) |
| `F1` | About |

### Video Controls

| Key | Action |
|-----|--------|
| `Space` / `K` | Play / Pause |
| `[` | Seek backward 5 seconds |
| `]` | Seek forward 5 seconds |
| Click seek bar | Seek to position |

## Gestures

| Gesture | Action |
|---------|--------|
| Pinch (trackpad) | Zoom in/out |
| Scroll wheel | Pan when zoomed; navigate when fit |
| 3-finger swipe | Navigate between files |
| Drag (when zoomed) | Pan around the image/video |
| Double-click | Toggle zoom (fit / 2.5x) |
| Drag and drop | Open files or directories |

## Design Principles

- **Fast** — No visible lag when navigating between files
- **Minimal** — No file browser, thumbnails, or metadata viewer
- **Keyboard-first** — Arrow keys to navigate, scroll to zoom

## Tech Stack

- **Language:** Rust
- **UI Toolkit:** GTK4 + libadwaita
- **Image Loading:** gdk-pixbuf
- **EXIF Parsing:** kamadak-exif

## License

GPL-3.0-or-later

## Donations

If you find this project helpful, please consider making a donation to support its development.

- **Monero**: `88LyqYXn4LdCVDtPWKuton9hJwbo8ZduNEGuARHGdeSJ79BBYWGpMQR8VGWxGDKtTLLM6E9MJm8RvW9VMUgCcSXu19L9FSv`
- **Bitcoin**: `bc1q6mh77hfv8x8pa0clzskw6ndysujmr78j6se025`
