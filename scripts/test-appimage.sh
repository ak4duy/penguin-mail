#!/usr/bin/env bash
# Run a real artifact with the target distro's stock libraries. The outer
# container allows namespace creation; WebKit and glycin keep their sandbox.
set -euo pipefail
cd "$(dirname "$0")/.."
artifact=$(realpath "${1:?usage: scripts/test-appimage.sh <AppImage> <base-image>}")
base=${2:?usage: scripts/test-appimage.sh <AppImage> <base-image>}
work=$(mktemp -d)
name="penguin-appimage-test-$$"
fuse="penguin-appimage-fuse-$$"
image="penguin-appimage-test:$$"
cleanup() {
    docker rm -f "$name" "$fuse" >/dev/null 2>&1 || true
    docker image rm "$image" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
docker build --build-arg "BASE=$base" -t "$image" - < packaging/appimage/test.Dockerfile
docker run -d --name "$name" --label io.github.c9dev.penguin-mail.appimage-test=1 \
    --security-opt seccomp=unconfined --security-opt apparmor=unconfined \
    --security-opt systempaths=unconfined "$image" sleep infinity
docker cp "$artifact" "$name:/tmp/Penguin Mail.AppImage"
docker cp packaging/appimage/relaunch.py "$name:/tmp/relaunch.py"
cat > "$work/check.sh" <<'CHECK'
#!/bin/sh
set -eu
export HOME=/tmp/penguin-test-home GNUPGHOME=/tmp/penguin-test-gnupg
export GDK_BACKEND=x11 GSK_RENDERER=cairo LANG=C.UTF-8
unset DISPLAY WAYLAND_DISPLAY LD_LIBRARY_PATH LD_PRELOAD
mkdir -p "$HOME" "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
image='/tmp/Penguin Mail.AppImage'
"$image" --appimage-extract-and-run --version
"$image" --appimage-extract-and-run --runtime-cli --version
cat > "$HOME/image.svg" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="blue"/></svg>
SVG
xvfb-run -a dbus-run-session -- "$image" --appimage-extract-and-run \
    --runtime-probe "$HOME/image.svg" https://github.com --host-gpg
cd "$HOME"
"$image" --appimage-extract >/dev/null
mv squashfs-root 'Extracted Mail'
'Extracted Mail/AppRun' --version
xvfb-run -a dbus-run-session -- 'Extracted Mail/AppRun' --runtime-probe "$HOME/image.svg"
# A demo uses a throwaway store, with no personal accounts or keys.
export PENGUIN_TEST_IMAGE="$image"
xvfb-run -a dbus-run-session -- sh -eu -c '
    openbox >/tmp/penguin-openbox.log 2>&1 &
    "$PENGUIN_TEST_IMAGE" --appimage-extract-and-run --demo >/tmp/penguin-demo.log 2>&1 &
    app=$!
    trap "kill $app 2>/dev/null || true" EXIT
    for i in $(seq 40); do
        kill -0 "$app" || { cat /tmp/penguin-demo.log; exit 1; }
        if xwininfo -root -tree | grep -q "Penguin Mail"; then
            echo "Demo window: PASS"
            exit 0
        fi
        sleep 1
    done
    cat /tmp/penguin-demo.log
    exit 1
'
# Restarts, the login item and the bridge keep running without FUSE. The
# check adds an account row to its own throwaway store.
mkdir -p /tmp/relaunch-extracted
HOME=/tmp/relaunch-extracted xvfb-run -a dbus-run-session -- \
    python3 /tmp/relaunch.py "$image" extracted
CHECK
docker cp "$work/check.sh" "$name:/tmp/check.sh"
docker exec -u 1000 "$name" sh /tmp/check.sh

# The same with FUSE, where the runtime mounts the image instead. This
# container gets the FUSE device and the right to mount; the first has
# neither, so it proves extraction mode needs no FUSE.
docker run -d --name "$fuse" --label io.github.c9dev.penguin-mail.appimage-test=1 \
    --device /dev/fuse --cap-add SYS_ADMIN \
    --security-opt seccomp=unconfined --security-opt apparmor=unconfined \
    --security-opt systempaths=unconfined "$image" sleep infinity
docker cp "$artifact" "$fuse:/tmp/Penguin Mail.AppImage"
docker cp packaging/appimage/relaunch.py "$fuse:/tmp/relaunch.py"
cat > "$work/mounted.sh" <<'CHECK'
#!/bin/sh
set -eu
export HOME=/tmp/relaunch-mounted GDK_BACKEND=x11 GSK_RENDERER=cairo LANG=C.UTF-8
unset DISPLAY WAYLAND_DISPLAY LD_LIBRARY_PATH LD_PRELOAD
mkdir -p "$HOME"
image='/tmp/Penguin Mail.AppImage'
"$image" --version
xvfb-run -a dbus-run-session -- python3 /tmp/relaunch.py "$image" mounted
CHECK
docker cp "$work/mounted.sh" "$fuse:/tmp/mounted.sh"
docker exec -u 1000 "$fuse" sh /tmp/mounted.sh
