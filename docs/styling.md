# Styling the bar

Sliver styles the Touch Bar with a stylesheet, like waybar's `style.css`. Decision record:
ADR-0004. Engine: `crates/sliver-render/src/style/`.

## Quick start
```css
@import "sliver:classic";          /* start from a built-in theme, then override */

@define-color accent #3584e4;

module            { border-radius: 10px; padding: 0 18px; }
module:active     { background: @accent; }               /* while pressed */
#battery.critical { background: #c01c28; }
#custom-vpn.connected { color: #33d17a; }
.zone-main        { background: alpha(white, 0.06); border-radius: 12px; }
bar.layer-fn module { background: #1e1e24; }              /* only on the Fn layer */
```
Try it headless: `cargo run -p sliver-render --example bar -- out.png --style my.css esc '|' icon:play_pause 'Song#title' '|' 12:34#clock`

## The tree
```
bar   classes: layer-<name>, layout-<name>, <bar name>, locked, dimmed (set by the agent)
└─ zone   class .zone-<name> (.zone-esc, .zone-main, .zone-controls by default) + config `class`
   └─ module   #<type> (#clock, #battery, #custom-vpn, #key-f1), class .module,
               .<instance> (battery#bat2 → .bat2), state classes (.charging .warning
               .critical .muted .playing, .on for toggles that are on …), script JSON
               `class`, config `class`
      └─ trough          sliders only (GTK/waybar node names)
         ├─ highlight    the filled part, up to the value
         └─ slider       the knob (drawn only if it has a min-width)
```
`#id` only ever matches modules.

## Selectors
`*`, `bar`/`zone`/`module`, `#id`, `.class`, `:active` (pressed), `:checked` (a toggle that is on; the `.on` class is equivalent in the built-in themes),
`:disabled`, `:first-child`, `:last-child`, `:not(simple)`, descendant (`a b`) and child
(`a > b`) combinators, and comma lists. `:hover`/`:focus` are accepted but never match on a touch
strip. Sibling combinators (`+`, `~`) aren't supported.

Precedence is standard CSS: `!important`, then specificity, then later rules win.

## Properties
| Property | Values | Inherited | Notes |
|---|---|---|---|
| `color` | color | yes | Labels and symbolic icons |
| `background`, `background-color` | color | no | Gradients and images aren't supported |
| `border` | `<width> [solid] <color>` / `none` | no | |
| `border-width`, `border-color`, `border-style` | length / color / `solid`,`none` | no | `border-color` defaults to `color` |
| `border-radius` | length | no | |
| `padding`, `padding-{top,right,bottom,left}` | 1–4 lengths | no | Inside the box. Auto widths include it |
| `margin`, `margin-{…}` | 1–4 lengths | no | Outside the box, eats into its slot |
| `font-family` | list | yes | First installed wins. `sans-serif` and unknown → the embedded Noto Sans |
| `font-size` | length | yes | Labels shrink (down to half) to fit, then ellipsize |
| `font-weight` | `normal`, `bold`, 100–900 | yes | |
| `-sliver-icon-size` (`-gtk-icon-size`) | length | yes | |
| `opacity` | 0–1 or % | multiplies down the tree | |
| `min-width` | length | no | Floor for `auto`-width modules. On `trough`: its natural length. On `slider`: knob width |
| `min-height` | length | no | On `trough`: thickness (default 6 px). On `slider`: knob height |
| `gap` | length | no | On `bar`: between zones. On `zone`: between modules. On `module`: icon to label |
| `transition` | `none` or a comma list of `[property] <time> [timing] [delay]` | no | Animates `color`, `background-color` (`background`), `border-color` and `opacity` when a module's style changes (pressed, state classes, `:checked`...). Timing: `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`, `cubic-bezier(a,b,c,d)`. `all` means those four. Other properties (`min-width`, `transform`...) parse and are ignored, since they would need a re-layout. Frames are drawn at ~30 fps only while something moves; an idle bar costs nothing. The pressed overlay sliverd shows never animates. |

Lengths: `px`, `pt`, `em` (relative to the element's font size), or bare numbers (px).
`inherit`, `initial` and `unset` work everywhere.

Colors: anything CSS accepts (`#rgb[a]`, `#rrggbb[aa]`, `rgb()`, `rgba()`, `hsl()`, names,
`transparent`, `currentColor`), plus GTK's `@name`, `alpha(c, a)`, `shade(c, k)`,
`mix(a, b, t)`, `lighter(c)`, `darker(c)`.

## At-rules
- `@define-color name value;` (must come before use).
- `@import "path.css";` (relative to the importing file) and `@import "sliver:classic";`.
  Imports nest up to 8 deep.
- Anything else (`@media`, `@keyframes`, …) is reported and skipped.

## Errors
Nothing in a stylesheet can blank the bar. Each problem is reported as `file:line:col: message`
and only the offending declaration, selector or rule is dropped. The built-in safe layer never
uses your stylesheet.

## Built-in themes
- `classic`: tiny-dfr's look (gray 0.2/0.4 buttons, radius 8, 16 px gaps, 32 px bold).
- `sliver` (the default): the macOS Touch Bar look. Toggles that are `.on` and slider fills use
  `@accent`, which the agent sets from your desktop's accent color (falls back to `#3584e4`).
