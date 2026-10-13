# Desktop integration and ROM images

## Desktop integration

On Wayland, taskbars take the application icon from the desktop
entry database, not from the window (there is no client-side window
icon); on X11 the same entry makes taskbars group and identify the
window. One-time, user-level (no root):

```
cargo build --release
dist/install-user.sh
```

This installs `pmd85.desktop` (pointing at the built binary) and
the icon into `~/.local/share`, and refreshes the desktop
databases. If the taskbar does not pick it up immediately, relog or
restart the panel. Re-run the script after moving the repository or
rebuilding at a different path.

## ROM images

Monitor ROMs live in `Rom/` (see `Rom/rom-list.txt`). They are not
part of this repository's source; the boot tests require them to be
present.
