# Configuration and widgets

Sliver layouts are TOML documents. The daemon loads `sliver.toml` on startup;
the customizer can save that file and apply its current in-memory layout to a
running daemon.

## Root fields

```toml
background = "#111111"
```

- `background` — strip-wide `#rrggbb` background. Defaults to black.
- `[[widgets]]` — ordered widget entries. Their order is left-to-right on the
  physical strip.

## Shared text-widget fields

Label, button, clock, and battery widgets share these optional fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `color` | string | Text color as `#rrggbb`; battery auto-colors if omitted. |
| `width` | number | Fixed strip pixels. Omit for a share of flexible space. The GUI displays omitted width as `0 = flex`. |
| `font_size` | number | Pango font size; default `24`. |
| `bold` | boolean | Bold text; default `false`. |
| `bg` | string | Optional rounded background pill as `#rrggbb`. |
| `align` | string | `left`, `center`, or `right`; default `center`. |

The strip coordinate space is 2008x60. Layout includes 16 pixels of outer
padding and 8 pixels between widgets.

## Label

Static text. A label can optionally run a shell action when tapped.

```toml
[[widgets]]
type = "label"
text = "sliver"
color = "#ff88aa"
width = 180
font_size = 24
bold = true
align = "left"
action = "notify-send 'sliver label tapped'"
```

## Button

A visibly actionable text widget. Functionally it differs from a label only
in intent and its customizer defaults; give it a background pill and action.

```toml
[[widgets]]
type = "button"
text = "mute"
action = "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"
color = "#ffffff"
bg = "#3a2233"
width = 180
bold = true
```

Actions execute as the Sliver user through:

```text
sh -c <action>
```

Treat configs as executable code. Do not apply untrusted layouts.

Useful action examples:

```toml
action = "playerctl play-pause"
action = "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"
action = "hyprlock"
action = "notify-send 'hello from the touchbar'"
```

## Clock

Displays local time using chrono/strftime formatting. Both daemon and GUI
preview redraw once per second, so `%S` works.

```toml
[[widgets]]
type = "clock"
format = "%a %H:%M:%S"
color = "#aaddff"
width = 300
font_size = 26
bold = true
```

Common conversions:

| Token | Meaning |
| --- | --- |
| `%H` | 24-hour hour |
| `%I` | 12-hour hour |
| `%M` | minute |
| `%S` | second |
| `%a` | abbreviated weekday |
| `%d` | day of month |

## Battery

Reads the first `Battery` device under `/sys/class/power_supply` and replaces
`{capacity}` in the format.

```toml
[[widgets]]
type = "battery"
format = "{capacity}%"
width = 180
bg = "#1a2e22"
```

If `color` is omitted, text is:

- mint above 50%
- amber from 21–50%
- red at 20% or below

## Spacer

Consumes flexible width and renders nothing.

```toml
[[widgets]]
type = "spacer"
flex = 1
```

Multiple spacers divide remaining width in proportion to their `flex` values.

## Complete example

```toml
background = "#0d0c14"

[[widgets]]
type = "label"
text = "sliver"
color = "#ff88aa"
width = 180
bold = true

[[widgets]]
type = "spacer"
flex = 1

[[widgets]]
type = "clock"
format = "%H:%M:%S"
color = "#aaddff"
width = 260
font_size = 28

[[widgets]]
type = "battery"
format = "{capacity}%"
width = 180

[[widgets]]
type = "button"
text = "mute"
action = "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"
bg = "#3a2233"
width = 160
bold = true
```

Preview and apply it:

```bash
./target/release/sliverd example.toml example.png
./target/release/sliverd --apply example.toml
```

## Live-apply protocol

The daemon listens at:

```text
$XDG_RUNTIME_DIR/sliver.sock
```

A client writes one complete TOML document, closes its write side, and reads a
single response:

```text
ok
```

or:

```text
error: <parse or validation details>
```

`sliverd --apply` and `sliver-edit` both use this protocol.
