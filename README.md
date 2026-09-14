# Carousel

A fast image and video viewer for Linux.

Built with **Rust** and **GTK4/libadwaita** for native Wayland/X11 support.

## Features

- Opens images and videos from a directory
- Navigates between files with keyboard or gestures
- Fit-to-window display with aspect ratio preservation
- Zoomable via pinch-to-zoom, scroll wheel, or keyboard
- Video playback with controls (play/pause, seek, volume, mute)
- Auto-hide controls for distraction-free viewing
- EXIF orientation handling for rotated images
- Toast notifications for load errors
- Fullscreen mode

## Supported Formats

| Type | Formats |
|------|---------|
| Images | JPEG, PNG, WebP, GIF, BMP, TIFF |
| Video | MP4, WebM, MKV |

## Building

### Dependencies

- Rust 1.75+
- GTK 4.14+
- libadwaita 1.6+
- gettext (for i18n)

### From source (Meson)

```bash
meson setup builddir
meson compile -C builddir
meson install -C builddir
```

### From source (Cargo)

```bash
cargo build --release
./target/release/carousel
```

### Flatpak

```bash
flatpak-builder --user --install --force-clean build-dir com.github.carousel.yml
```

## Usage

```bash
# View media in current directory
carousel

# View media in a specific directory
carousel /path/to/directory

# View a specific file (shows siblings)
carousel /path/to/image.jpg
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
| `Escape` | Reset zoom (if zoomed) |
| `Double-click` | Toggle between fit and 2.5x zoom |

### View

| Key | Action |
|-----|--------|
| `F11` / `F` | Toggle fullscreen |

### Application

| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit |
| `Ctrl+W` | Close window |
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
