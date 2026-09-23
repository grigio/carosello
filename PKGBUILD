# Maintainer: grigio <https://github.com/grigio>
# Arch Linux package for Carosello — fast, minimalist image/video viewer (GTK4/libadwaita).
# AUR usage:
#   git clone https://aur.archlinux.org/carosello.git && cd carosello && makepkg -si
# Or with an AUR helper: yay -S carosello  /  paru -S carosello

pkgname=carosello
pkgver=1.2.2
pkgrel=1
pkgdesc="A fast, minimalist image and video viewer for Linux (GTK4/libadwaita)"
arch=('x86_64' 'aarch64')
url="https://github.com/grigio/carosello"
license=('GPL-3.0-or-later')
depends=('gtk4' 'libadwaita' 'glib2' 'graphene' 'glibc' 'libgcc' 'hicolor-icon-theme')
makedepends=('cargo' 'meson')
checkdepends=('appstream' 'desktop-file-utils' 'gdk-pixbuf2')
optdepends=(
  'gst-plugins-good: extra video codecs (mp4/webm)'
  'gst-plugins-bad: extra video codecs (mkv)'
  'gst-plugins-ugly: extra video codecs'
  'gst-libav: ffmpeg-based codecs'
)
source=(
  "$pkgname-$pkgver.tar.gz::https://github.com/grigio/carosello/archive/refs/tags/v$pkgver.tar.gz"
  'gpl-3.0.txt'
  'cargo-lock.patch'
)
# Upstream tarballs change per release; update with `updpkgsums` after tagging.
sha256sums=(
  '90974921ea365d97afc251ab3ca05e31d0bdf570bdc204b246b56b25b3113383'
  '3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986'
  '01ca5796700277d4e39d6a848bf55909f2a5a98e17817421f2102666dc416c45'
)

prepare() {
  cd "$pkgname-$pkgver"
  patch -Np1 -i "$srcdir/cargo-lock.patch"
  export RUSTUP_TOOLCHAIN=stable
  mkdir -p build/cargo-home
  CARGO_HOME="$PWD/build/cargo-home" cargo fetch --locked
}

build() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  arch-meson build
  meson compile -C build
}

check() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_HOME="$PWD/build/cargo-home"
  desktop-file-validate "data/io.github.grigio.carosello.desktop"
  appstreamcli validate --no-net "data/io.github.grigio.carosello.metainfo.xml"
  cargo test --frozen
}

package() {
  cd "$pkgname-$pkgver"
  meson install -C build --destdir "$pkgdir"
  install -Dm644 "$srcdir/gpl-3.0.txt" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
