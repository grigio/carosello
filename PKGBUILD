# Maintainer: grigio <https://github.com/grigio>
# Arch Linux package for Carosello — fast, minimalist image/video viewer (GTK4/libadwaita).
# AUR usage:
#   git clone https://aur.archlinux.org/carosello.git && cd carosello && makepkg -si
# Or with an AUR helper: yay -S carosello  /  paru -S carosello

pkgname=carosello
pkgver=1.2.1
pkgrel=1
pkgdesc="A fast, minimalist image and video viewer for Linux (GTK4/libadwaita)"
arch=('x86_64' 'aarch64')
url="https://github.com/grigio/carosello"
license=('GPL-3.0-or-later')
depends=('gtk4' 'libadwaita' 'glib2' 'gdk-pixbuf2' 'hicolor-icon-theme')
makedepends=('cargo' 'meson' 'git' 'desktop-file-utils' 'appstream-glib')
optdepends=(
  'gst-plugins-good: extra video codecs (mp4/webm)'
  'gst-plugins-bad: extra video codecs (mkv)'
  'gst-plugins-ugly: extra video codecs'
  'gst-libav: ffmpeg-based codecs'
)
source=("$pkgname-$pkgver.tar.gz::https://github.com/grigio/carosello/archive/refs/tags/v$pkgver.tar.gz")
# Upstream tarballs change per release; update with `updpkgsums` after tagging.
sha256sums=('3fcb07056fadd2b0ae4af325516fb3254b4767fbaaf53c63b24feb1f46284476')

build() {
  cd "$pkgname-$pkgver"
  arch-meson build
  meson compile -C build
}

check() {
  cd "$pkgname-$pkgver"
  desktop-file-validate "data/io.github.grigio.carosello.desktop"
  appstreamcli validate --no-net "data/io.github.grigio.carosello.metainfo.xml"
  cargo test --locked
}

package() {
  cd "$pkgname-$pkgver"
  meson install -C build --destdir "$pkgdir"
}
