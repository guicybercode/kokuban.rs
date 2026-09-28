# dmgbuild settings for the Kokuban macOS disk image.
# Invoked by scripts/package-macos-dmg.sh as:
#   dmgbuild -s scripts/dmg-settings.py -D app=PATH/Kokuban.app -D root=REPO "Kokuban" OUT.dmg
# Window size and icon positions come from assets/dmg/layout.json, shared with
# scripts/render-dmg-background.py so the artwork lines up with the icons.
# `defines` is provided by dmgbuild when it executes this file.

import json
import os.path

_app = defines["app"]  # noqa: F821
_root = defines["root"]  # noqa: F821
_layout = json.loads(open(os.path.join(_root, "assets", "dmg", "layout.json"), encoding="utf-8").read())
_app_name = os.path.basename(_app)

format = "UDZO"
filesystem = "HFS+"
files = [_app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(_app, "Contents", "Resources", "Kokuban.icns")

# dmgbuild combines background.png with background@2x.png into a HiDPI TIFF.
background = os.path.join(_root, "assets", "dmg", "background.png")
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
sidebar_width = 0
default_view = "icon-view"
window_rect = ((200, 160), (_layout["window"]["width"], _layout["window"]["height"]))
arrange_by = None
show_icon_preview = False
label_pos = "bottom"
icon_size = _layout["icon_size"]
text_size = _layout["text_size"]
icon_locations = {
    _app_name: tuple(_layout["icons"]["Kokuban.app"]),
    "Applications": tuple(_layout["icons"]["Applications"]),
}
