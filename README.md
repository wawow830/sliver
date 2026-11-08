# sliver

sliver makes the Touch Bar on an Apple Silicon MacBook Pro running Asahi Linux
into a small Wayland screen. Any Wayland app can draw on it. The bar it ships
with is a Quickshell config, so you can change it or write your own.

It was written on a 13-inch M2 MacBook Pro. The 13-inch M1 has the same Touch
Bar and should work too.

## The default bar

On the left are your Hyprland workspaces, each with the icons of the apps open
on it. Tap one to go there, or swipe sideways with two fingers.

The middle changes with the focused window. Terminals get F1 to F12. Browsers
get back, forward, reload and new tab, with the music player next to them.
Everything else gets the music player, if one is open. The cover is the play
button, and you can drag along the timeline to seek. If cava is installed, its
spectrum sits on the timeline.

On the right are mic mute, screen brightness and volume. Drag anywhere on a
slider to move it. Tapping the volume icon mutes.

Holding Fn shows Esc and F1 to F12 no matter what else is up.

The Touch Bar turns off after a minute without typing or touches. The next key
or touch wakes it.

## How it works

`sliverd` is a Wayland compositor, built on Smithay, that only drives the Touch
Bar. It runs as a system service, takes the display and touchscreen that udev
puts on `seat-touchbar`, and listens on `/run/sliver`.

It shows one window at a time: the newest one, fullscreen at 2008x60. Touches
go to that window. Keys pressed through the virtual keyboard protocol come out
of a uinput device called "Sliver Touch Bar", so they reach whatever has focus
on your normal desktop.

While Fn is held it shows the newest window titled `fkeys` instead. sliverd has
a plain one built in, which is also what you see when nothing else is
connected.

tiny-dfr stays installed. sliverd stops it on start. If sliverd keeps
crashing, systemd gives up on it and starts tiny-dfr again, so you always have
an Esc key.

## Install

This is packaged for Fedora Asahi Remix. Build the RPM from a clone:

```sh
git clone https://github.com/wawow830/sliver
cd sliver
sudo dnf install rpm-build
sudo dnf builddep packaging/sliver.spec
git archive --prefix=sliver-0.1.0/ -o sliver-0.1.0.tar.gz HEAD
rpmbuild -bb --define "_sourcedir $PWD" packaging/sliver.spec
sudo dnf install ~/rpmbuild/RPMS/aarch64/sliver-0.1.0-*.rpm
```

The bar uses Google Sans Flex and Material Symbols Rounded, which Fedora
doesn't package. Without them the icons show up as their names. Both are on
GitHub:

```sh
curl -fL --create-dirs -o ~/.local/share/fonts/GoogleSansFlex.ttf \
  'https://raw.githubusercontent.com/google/fonts/main/ofl/googlesansflex/GoogleSansFlex%5BROND%2Copsz%2Cslnt%2Cwdth%2Cwght%5D.ttf'
curl -fL --create-dirs -o ~/.local/share/fonts/MaterialSymbolsRounded.ttf \
  'https://raw.githubusercontent.com/google/material-design-icons/master/variablefont/MaterialSymbolsRounded%5BFILL%2CGRAD%2Copsz%2Cwght%5D.ttf'
```

The socket belongs to the `input` group, so add yourself to it, then reboot:

```sh
sudo usermod -aG input $USER
```

sliverd starts by itself when the Touch Bar appears. The bar is a user service
tied to `graphical-session.target`:

```sh
systemctl --user enable --now sliver-bar.service
```

If your session never reaches `graphical-session.target` (it does under uwsm),
start the bar from your Hyprland config instead:

```
exec-once = WAYLAND_DISPLAY=/run/sliver QT_QPA_PLATFORM=wayland QT_QUICK_BACKEND=software qs -c sliver
```

## Your own bar

`~/.config/quickshell/sliver/shell.qml` replaces the default one in
`/etc/xdg/quickshell/sliver/`. The default is only this:

```qml
import Sliver

Bar {}
```

`Bar` takes the lists it uses to tell terminals and browsers apart, if yours
aren't in them:

```qml
Bar { terminals: ["kitty", "foot"] }
```

Or build something else from the same parts:

```qml
import QtQuick
import QtQuick.Layouts
import Quickshell
import Sliver

FloatingWindow {
    color: "black"

    RowLayout {
        anchors.fill: parent
        anchors.margins: 1
        spacing: Theme.gap

        Key { Layout.fillHeight: true; icon: "content_copy"; keys: [29, 46] }
        Key { Layout.fillHeight: true; icon: "content_paste"; keys: [29, 47] }
        Media { Layout.fillWidth: true; Layout.fillHeight: true }
        Volume { Layout.fillHeight: true }
    }
}
```

`Key` presses Linux key codes in order and lets go in reverse, so `[29, 46]` is
Ctrl+C. Icons are Material Symbols names. The other parts are `Pill`, `Level`,
`Brightness`, `Volume`, `Mic`, `Workspaces`, `Media`, `Cover`, `Seek` and
`FKeys`, with `Theme`, `Desktop` (Hyprland), `Spectrum` (cava) and `Keyboard`
behind them. The sources are in `bar/Sliver`.

Since the newest window is the one shown, you can try a config without
touching the service. Close it and the old bar comes back:

```sh
WAYLAND_DISPLAY=/run/sliver QT_QPA_PLATFORM=wayland QT_QUICK_BACKEND=software qs -p my-bar.qml
```

sliverd only takes shared memory buffers, which is why Qt Quick needs the
software backend. Any other app you point at `/run/sliver` has to do the same.
To give Fn your own keys, title a window `fkeys`.

## Colours

The bar reads Material colour roles from `~/.config/sliver/colors.json` and
picks up changes as soon as the file is written. Roles you leave out keep
their defaults. The names are the ones in `bar/Sliver/Theme.qml`, and they
match matugen's, so a matugen template can write the file:

```json
{
  "primary": "{{colors.primary.default.hex}}",
  "on_primary": "{{colors.on_primary.default.hex}}",
  "surface_container_high": "{{colors.surface_container_high.default.hex}}"
}
```

## Tests

`cargo test` runs sliverd against a fake panel. `cargo test -- --ignored` also
builds the QML module and checks that Quickshell can draw and type through it.

## License

MIT
