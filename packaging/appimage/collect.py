#!/usr/bin/env python3
"""Collect Ubuntu's runtime files without changing the application's build."""
import glob
from pathlib import Path
import re
import shutil
import subprocess
import sys

root = Path(sys.argv[1]).resolve()
seen = set()


def copy(path):
    path = Path(path)
    if str(path) in seen or not path.exists():
        return
    seen.add(str(path))
    if path.is_dir():
        for child in sorted(path.iterdir()):
            copy(child)
        return
    dest = root / str(path).lstrip('/')
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, dest, follow_symlinks=True)
    with path.open('rb') as stream:
        is_elf = stream.read(4) == b'\x7fELF'
    if is_elf:
        deps = subprocess.run(['ldd', str(path)], text=True, capture_output=True)
        if 'not found' in deps.stdout:
            raise RuntimeError(f'Missing dependency of {path}: {deps.stdout}')
        for lib in re.findall(r'(/[^\s()]+)', deps.stdout):
            copy(lib)


# Libraries loaded at runtime do not appear in the app's ELF dependencies.
for pattern in [
    '/usr/lib/x86_64-linux-gnu/webkitgtk-6.0',
    '/usr/lib/x86_64-linux-gnu/gio/modules/libgiognutls.so',
    '/usr/lib/x86_64-linux-gnu/gio/modules/libdconfsettings.so',
    '/usr/lib/x86_64-linux-gnu/gdk-pixbuf-2.0',
    '/usr/lib/x86_64-linux-gnu/gstreamer-1.0',
    '/usr/lib/x86_64-linux-gnu/dri',
    '/usr/lib/x86_64-linux-gnu/libgallium-*.so',
    '/usr/lib/x86_64-linux-gnu/libEGL_mesa.so.0',
    '/usr/lib/x86_64-linux-gnu/libGLESv2.so.2',
    '/usr/libexec/glycin-loaders',
    '/usr/share/glycin-loaders', '/usr/share/glvnd/egl_vendor.d/50_mesa.json',
    '/usr/share/glib-2.0/schemas', '/usr/share/mime',
    '/usr/share/icons/Adwaita', '/usr/share/icons/hicolor/index.theme',
    '/usr/share/fonts/truetype/dejavu', '/etc/fonts', '/etc/ssl/certs',
    '/usr/lib/ssl/cert.pem', '/usr/lib/ssl/openssl.cnf', '/usr/share/zoneinfo',
]:
    for path in glob.glob(pattern):
        copy(path)
for path in [root / 'usr/bin/penguin-mail', root / 'usr/bin/penguin-mail-cli',
             root / 'usr/lib/penguin-mail/probe', root / 'usr/lib/penguin-mail/appimage-runtime.so']:
    result = subprocess.run(['ldd', str(path)], capture_output=True, text=True, check=True)
    if 'not found' in result.stdout:
        raise RuntimeError(result.stdout)
    for lib in re.findall(r'(/[^\s()]+)', result.stdout):
        copy(lib)
# Keep attribution and source-package versions with the redistributable files.
# dpkg's complete manifest also records packages supplying indirect resources.
manifest = root / 'usr/share/doc/penguin-mail/runtime-packages.tsv'
manifest.parent.mkdir(parents=True, exist_ok=True)
manifest.write_text(subprocess.check_output(
    ['dpkg-query', '-W', '-f=${binary:Package}\t${Version}\t${source:Package}\t${source:Version}\n'], text=True))
for path in Path('/usr/share/doc').glob('*/copyright'):
    copy(path)
copy('/usr/share/common-licenses')
print(f'Collected {len(seen)} runtime paths')

subprocess.run(['gio-querymodules', str(root / 'usr/lib/x86_64-linux-gnu/gio/modules')], check=True)

fonts = root / 'etc/fonts/fonts.conf'
fonts.write_text(fonts.read_text().replace('</fontconfig>', '<dir prefix="relative">../../usr/share/fonts</dir>\n</fontconfig>'))
