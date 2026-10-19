#!/usr/bin/env bash
# Packs an AppImage-only build staged by stage.sh. Build mailrs with
# packaging-appimage in its own job; never reuse this tree for native packages.
set -euo pipefail
umask 022
cd "$(dirname "$0")/.."
tree=$(realpath "${1:?usage: scripts/appimage.sh <tree> <version> <out>}")
version=${2:?usage: scripts/appimage.sh <tree> <version> <out>}
mkdir -p "${3:?usage: scripts/appimage.sh <tree> <version> <out>}"
out=$(realpath "$3")
[ "$(uname -m)" = x86_64 ] || { echo 'AppImage currently supports x86_64.' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
appdir="$work/PenguinMail.AppDir"
mkdir -p "$appdir/usr"
cp -a "$tree/." "$appdir/usr/"
for file in bin/penguin-mail bin/penguin-mail-cli share/applications/io.github.c9dev.PenguinMail.desktop \
    share/icons/hicolor/scalable/apps/io.github.c9dev.PenguinMail.svg \
    share/metainfo/io.github.c9dev.PenguinMail.metainfo.xml; do
    test -s "$appdir/usr/$file" || { echo "Missing staged file: $file" >&2; exit 1; }
done
# appimagetool reads and checks the store listing only under its older
# appdata name.
ln -s io.github.c9dev.PenguinMail.metainfo.xml \
    "$appdir/usr/share/metainfo/io.github.c9dev.PenguinMail.appdata.xml"
mkdir -p "$appdir/usr/lib/penguin-mail"
cc -O2 -Wall -Wextra -Werror -shared -fPIC packaging/appimage/runtime.c \
    -o "$appdir/usr/lib/penguin-mail/appimage-runtime.so" $(pkg-config --cflags --libs gio-2.0) -ldl
cc -O2 -Wall -Wextra -Werror -static packaging/appimage/bwrap.c -o "$appdir/usr/bin/bwrap"
cc -O2 packaging/appimage/probe.c -o "$appdir/usr/lib/penguin-mail/probe" \
    $(pkg-config --cflags --libs gtk4 webkitgtk-6.0 libsoup-3.0)
python3 packaging/appimage/collect.py "$appdir"
install -m755 packaging/appimage/AppRun "$appdir/AppRun"
cp "$appdir/usr/share/applications/io.github.c9dev.PenguinMail.desktop" "$appdir/"
cp "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.c9dev.PenguinMail.svg" "$appdir/"
install -Dm644 LICENSE "$appdir/usr/share/doc/penguin-mail/LICENSE"
# Tagged releases, checked against their sums before they run. The type-2
# runtime is static and supports extraction when FUSE is unavailable. Its
# "continuous" release is replaced on every upstream build, so a pin on it
# breaks; release downloads also need no API token.
curl -fL --retry 3 https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage -o "$work/appimagetool"
curl -fL --retry 3 https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 -o "$work/runtime"
printf '%s  %s\n' \
    ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 "$work/appimagetool" \
    2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d "$work/runtime" | sha256sum -c -
chmod 755 "$work/appimagetool"
# Unpacked here rather than with --appimage-extract-and-run, whose runtime
# leaves its copy in the temporary folder.
(cd "$work" && ./appimagetool --appimage-extract >/dev/null)
ARCH=x86_64 "$work/squashfs-root/AppRun" --runtime-file "$work/runtime" \
    "$appdir" "$out/penguin-mail-$version-x86_64.AppImage"
