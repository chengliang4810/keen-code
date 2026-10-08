# Modified for RCode. See NOTICE.
{ lib, stdenv, fetchurl
, dpkg, autoPatchelfHook, makeWrapper, wrapGAppsHook3
, gtk3, gdk-pixbuf, cairo, glib, webkitgtk_4_1, libsoup_3, libgcc, gst_all_1
}:

let
  sources = builtins.fromJSON (builtins.readFile ./sources.json);
  version = sources.version;

  # 发布地址与哈希必须来自 RCode 自己的产物，禁止猜测或复用旧应用安装包。
  srcMap = lib.mapAttrs (_: artifact: fetchurl {
    inherit (artifact) url hash;
  }) sources.artifacts;

  sys = stdenv.hostPlatform.system;
in

assert lib.assertMsg (builtins.hasAttr sys srcMap)
  "RCode: no verified release artifact configured for ${sys}; build from local source";

stdenv.mkDerivation {
  pname = "rcode";
  inherit version;

  src = srcMap.${sys};

  nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    dpkg autoPatchelfHook makeWrapper wrapGAppsHook3
  ];

  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    gtk3 gdk-pixbuf cairo glib webkitgtk_4_1 libsoup_3 libgcc
    gst_all_1.gstreamer gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good gst_all_1.gst-plugins-bad
  ];

  GST_PLUGIN_SYSTEM_PATH = lib.optionalString stdenv.hostPlatform.isLinux
    "${gst_all_1.gst-plugins-base}/lib/gstreamer-1.0:${gst_all_1.gst-plugins-good}/lib/gstreamer-1.0:${gst_all_1.gst-plugins-bad}/lib/gstreamer-1.0";

  # wrapGAppsHook3 would auto-wrap the binary in postFixup; we wrap it manually
  # to add the GStreamer path, so disable the auto-wrap and splice the hook's
  # GApps args into our single wrapper instead of nesting two wrappers.
  dontWrapGApps = true;

  unpackPhase = if stdenv.hostPlatform.isLinux then "dpkg -x $src ." else "tar xzf $src";

  installPhase = if stdenv.hostPlatform.isLinux then ''
    mkdir -p $out/bin $out/share
    cp -r usr/share/* $out/share/
    install -Dm755 usr/bin/rcode $out/bin/rcode

    wrapProgram $out/bin/rcode \
      "''${gappsWrapperArgs[@]}" \
      --prefix GST_PLUGIN_SYSTEM_PATH : "$GST_PLUGIN_SYSTEM_PATH"
  '' else ''
    mkdir -p $out/Applications
    cp -r *.app $out/Applications/
  '';

  meta = with lib; {
    description = "Rust and Result agent development workbench";
    license = licenses.asl20;
    platforms = [ "x86_64-linux" "x86_64-darwin" "aarch64-darwin" ];
  };
}
