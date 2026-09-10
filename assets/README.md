# Application icon

`kokuban-icon.png` is the square 1254 × 1254 artwork supplied by the project
maintainer on 2026-09-10, copied without changing its pixels, transparency, or
proportions. It replaces the previous non-square icon.

The executable embeds this PNG. macOS uses it as the running application's Dock
icon. Linux renders a smaller version for X11 and uses the stable application ID
`io.github.guicybercode.kokuban` for desktop integration. Wayland compositors
resolve the icon through the matching installed `.desktop` entry.

See the [README installation instructions](../README.md#application-icon-development-builds)
to install the Linux launcher. A macOS executable's Finder icon is separate from
the running application's Dock icon; these builds remain standalone executables.
