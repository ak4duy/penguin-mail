#!/usr/bin/env python3
"""Check what an AppImage does where the desktop stops bubblewrap, as a
stock Ubuntu 24.04 does: it shows the command that allows it, the command
works, and Check Again starts the app.

    blocked.py <AppImage>

Run it on such a system, not in a container, with a hidden display, a
session bus and the accessibility bus, and with sudo that asks for no
password. It installs an AppArmor profile for the AppImage.
"""

import os
import subprocess
import sys
import time
from pathlib import Path

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

IMAGE = os.path.realpath(sys.argv[1])
FLAGS = ["--appimage-extract-and-run"]
HOME = Path(os.environ["HOME"])
SVG = HOME / "image.svg"


def say(text):
    print(text, flush=True)


def fail(text, log=None):
    if log:
        print(Path(log).read_text(errors="replace")[-4000:], flush=True)
    raise SystemExit(f"FAIL: {text}")


def wait(test, what, timeout=60):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = test()
        if value:
            return value
        time.sleep(0.25)
    fail(f"timed out waiting for {what}", HOME / "app.log")


def application():
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        if app and "PenguinMail" in (app.get_name() or ""):
            return app
    return None


def find(node, test, depth=0):
    if node is None or depth > 80:
        return None
    if test(node):
        return node
    for index in range(node.get_child_count()):
        hit = find(node.get_child_at_index(index), test, depth + 1)
        if hit:
            return hit
    return None


def actions(node):
    iface = node.get_action_iface()
    return [iface.get_action_name(i) for i in range(iface.get_n_actions())] if iface else []


def probe():
    return subprocess.run([IMAGE, *FLAGS, "--runtime-probe", str(SVG)],
                          capture_output=True, text=True, timeout=120)


SVG.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">'
               '<rect width="32" height="32" fill="blue"/></svg>\n')
restricted = Path("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
if not restricted.exists() or restricted.read_text().strip() != "1":
    fail("this system lets every program make user namespaces, so it cannot show the block")

before = probe()
if before.returncode == 0:
    fail("the sandbox started before the profile was installed")
say("Sandbox blocked: PASS")

log = open(HOME / "app.log", "w")
subprocess.Popen([IMAGE, *FLAGS], stdout=log, stderr=subprocess.STDOUT)
text = wait(lambda: find(application(), lambda n: (n.get_name() or "") == "Command"),
            "the command in the sandbox window")
command = Atspi.Text.get_text(text, 0, -1)
if f'"{IMAGE}"' not in command or "apparmor_parser -r" not in command:
    fail(f"the window shows {command!r}")
say("Sandbox window: PASS")

ran = subprocess.run(["bash", "-euc", command], capture_output=True, text=True)
if ran.returncode != 0:
    fail(f"the command failed: {ran.stderr}")
check = find(application(), lambda n: (n.get_name() or "") == "Check Again"
             and "click" in actions(n))
check.get_action_iface().do_action(actions(check).index("click"))
wait(lambda: find(application(), lambda n: n.get_role_name() == "frame"
                  and "win.preferences" in actions(n)),
     "the main window after Check Again")
say("Check Again starts the app: PASS")

after = probe()
if after.returncode != 0:
    fail(f"the sandbox still failed after the profile: {after.stderr[-2000:]}")
say("Sandbox allowed by the profile: PASS")
subprocess.run(["gdbus", "call", "--session", "--dest", "io.github.c9dev.PenguinMail",
                "--object-path", "/io/github/c9dev/PenguinMail",
                "--method", "org.gtk.Actions.Activate", "quit", "[]", "{}"],
               capture_output=True, timeout=30)
