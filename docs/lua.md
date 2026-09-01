# Lua v1 interface

Sliver runs standard Lua 5.4 in a disposable worker. A source file is loaded
with `sliver FILE`; the worker's first successful render is the apply commit.
The entry file's directory is added to Lua and C-module search paths and is the
worker's working directory.

## Application table

The source must return a table with:

| Field | Type | Required |
| --- | --- | --- |
| `api_version` | integer `1` | yes |
| `render` | function | yes |
| `start` | function | no |
| `stop` | function | no |
| `visibility` | function | no |
| `touch` | function | no |
| `key` | function | no |

Unknown fields, unsupported versions, and wrong callback types are errors.
Callback references are captured during load. Callbacks run serially, cannot
yield, and their return values are ignored.

`render(canvas, time, delta)` receives intended monotonic presentation time and
the elapsed time since the previous presented frame. The first delta is zero.
Each render gets a fresh black-cleared canvas. A canvas is invalid after its
render callback returns.

`start` runs while staging. Timers and synthetic keys become active only after
commit. `stop(reason)` receives `replaced`, `logout`, or `shutdown` and has a
500 ms cleanup deadline. A callback has a two-second deadline.

The embedded default uses the same contract. It has no source metadata; an
explicit file can read `sliver.source.path` and `sliver.source.directory`.

## Module

```lua
local sliver = require("sliver.v1")
```

The module exposes `api_version = 1`, plus:

- `sliver.redraw()` requests a frame. Requests coalesce, and a request during
  rendering schedules at most one subsequent frame.
- `sliver.timer.after(delay, callback)` creates a one-shot timer.
- `sliver.timer.every(interval, callback)` creates a repeating timer. Delays
  are fractional monotonic seconds; missed repetitions are skipped. Both
  return a handle with `cancel()`.
- `sliver.backlight.get()` and `sliver.backlight.set(level)` read and set a
  normalized level from `0.0` through `1.0`.
- `sliver.input.state()` returns `{ fn = boolean, modifiers = { ... } }` for
  the current Fn and left/right Ctrl, Alt, Shift, and Super state.

## Input

`touch(event)` receives:

```lua
{
    phase = "down" | "move" | "up" | "cancel",
    id = integer,
    time = number,
    x = number,
    y = number,
    modifiers = { ... },
    pressure = number?,
    width = number?,
    height = number?,
}
```

Coordinates use the top-left logical 2008 by 60 canvas space. Move events may
be coalesced per contact; transitions are ordered and retained. On worker
replacement, existing contacts are canceled and already-down contacts are not
sent to the new worker until they lift.

`key(event)` receives Fn or a named left/right modifier transition:

```lua
{ key = "fn", name = "fn", phase = "down" | "up", active = boolean, state = ... }
```

Sliver does not expose the general physical keyboard stream.

## Generic keys

The versioned key constants are under:

```lua
sliver.input.keys.keyboard
sliver.input.keys.consumer
```

The keyboard table includes `escape`, `f1` through `f12`, and left/right
`ctrl`, `alt`, `shift`, and `super`. The consumer table includes
`brightness_down`, `brightness_up`, `previous`, `play_pause`, `next`, `mute`,
`volume_down`, and `volume_up`.

Use `sliver.input.key.down(key)`, `.up(key)`, or `.tap(key)`. A tap inherits
physical modifiers by default. Pass `{ modifiers = false }` to suppress them,
or `{ modifiers = { sliver.input.keys.keyboard.left_ctrl } }` to provide an
explicit list. One tap is emitted as one ordered virtual-device transaction.
Held synthetic keys are released on worker replacement, failure, and ownership
change.

Synthetic key output is unavailable while staging.

## Canvas

Canvas coordinates are logical floating-point pixels. Every color is either a
six-digit sRGB string such as `"#336699"` or four normalized components
`red, green, blue, alpha`.

`text` positions the Pango layout from its top-left logical origin. The `y`
argument is not a baseline. `measure_text` returns the layout's logical width
and height, so keep `y + height` below the 60-pixel canvas edge and leave a
small margin for font ink. For example:

```lua
local _, height = canvas:measure_text(label, 24)
canvas:text(20, 60 - height - 1, label, 24, "#ffffff")
```

Methods are:

- `rectangle(x, y, width, height, color)`
- `fill(path, color)` and `stroke(path, width, color)`; create reusable paths
  with `sliver.path({ { "move_to", x, y }, { "line_to", x, y },
  { "curve_to", x1, y1, x2, y2, x3, y3 }, { "close" } })`
- `text(x, y, text, font_size, color)` and
  `measure_text(text, font_size)`
- `image(image, source_rect, destination_rect, filter?)`
- `raw_pixels(data, format, width, height, stride, source_rect,
  destination_rect, filter?)`
- `translate(x, y)`, `scale(x, y)`, `rotate(angle)`
- `clip(path)`, `alpha(value)`, `operator("source-over" | "source")`
- `save()` and `restore()`

Images are immutable worker-owned objects created with
`sliver.image.new(data, format, width, height, stride)`. Formats are straight
alpha `rgba8` and `bgra8`; filters are `"nearest"` and `"linear"` (linear is
the default). Encoded image and video formats are not decoded by Sliver.

Text uses UTF-8 shaping, bidirectional layout, system fonts, and fallback.
No font or asset bundle is required.
