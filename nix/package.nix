{ lib
, stdenv
, src
, version
, rustPlatform
, rustc
, cargo
, meson
, ninja
, pkg-config
, python3
, glib
, gtk4
, libxml2
, desktop-file-utils
, libadwaita
, wrapGAppsHook4
, hicolor-icon-theme
, gst_all_1
}:

stdenv.mkDerivation {
  pname = "carosello";
  inherit version src;

  # Cargo.lock is committed and has no git dependencies, so vendoring is
  # fully automatic — no outputHashes to update per release.
  cargoDeps = rustPlatform.importCargoLock {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    cargo
    rustc
    rustPlatform.cargoSetupHook # unpacks cargoDeps + writes the vendor config
    meson
    ninja
    pkg-config
    python3 # build-aux/cargo-version.py runs at meson configure time
    glib # glib-compile-resources
    gtk4 # gtk4-update-icon-cache, run by build-aux/meson/postinstall.py
    desktop-file-utils # update-desktop-database (the .desktop has MimeType=)
    libxml2 # xmllint, so the gresource xml-stripblanks step actually runs
    wrapGAppsHook4
  ];

  # Video playback: gtk::MediaFile decodes through GStreamer. The plugins
  # are picked up automatically — gstreamer's setup hook exports
  # GST_PLUGIN_SYSTEM_PATH_1_0 and wrapGAppsHook4 bakes it into the wrapper.
  buildInputs = [
    gtk4
    libadwaita
    hicolor-icon-theme
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-ugly
    gst_all_1.gst-libav
  ];

  # build-aux/cargo.sh (#!/bin/bash) and build-aux/meson/postinstall.py
  # (#!/usr/bin/env python3) are exec'd directly by meson; neither interpreter
  # path exists inside the sandbox, so rewrite them to store paths first.
  # The postinstall helper also calls the GTK3-named icon-cache binary; GTK4
  # ships it under gtk4-update-icon-cache (same trick nixpkgs uses, e.g. for
  # apostrophe and blackbox-terminal).
  postPatch = ''
    patchShebangs build-aux
    substituteInPlace build-aux/meson/postinstall.py \
      --replace-fail 'gtk-update-icon-cache' 'gtk4-update-icon-cache'
  '';

  meta = {
    description = "A fast, minimalist image and video viewer for Linux";
    homepage = "https://github.com/grigio/carosello";
    license = lib.licenses.gpl3Plus;
    mainProgram = "carosello";
    platforms = lib.platforms.linux;
  };
}
