# ADR-0004: Stylesheets (waybar-style CSS) for the bar

Status: accepted (ash, T22). Supersede with a new ADR. User reference: `docs/styling.md`.

## Context
GOAL.md's design principle says to style the bar like waybar's `style.css`: target modules by id,
class and state, and make themes just stylesheets. The renderer is pure Rust (ADR-0002), so GTK's
CSS engine isn't available.

## Decision
- **Own small CSS engine in `sliver-render::style`, no new dependencies** (`svgtypes`, already in
  the tree via resvg, parses CSS colors). Rejected options:
  - `simplecss`: fixed pseudo-classes, no `@define-color`, and no error positions.
  - `lightningcss`: heavy, and built for browser CSS we'd mostly reject.
  - `cssparser`: a fine tokenizer, but the subset is small enough that owning it gives
    better diagnostics for less glue.
- **Style tree** `bar > zone > module`.
  - Modules: tag `module`, plus class `.module` (waybar compat). `#id` = module type
    (`#battery`, `#custom-vpn`). Classes = instance name, states, and script/static classes.
  - Zones: class `.zone-<name>`. **`#id` only ever means a module.**
  - Bar: classes from the agent (`layer-fn`, `layout-x`, `locked`…).

  This follows docs/customization.md §7.
- **Selectors:** type, `*`, `#id`, `.class`, `:active` (pressed), `:checked` (toggle on), `:disabled`,
  `:first-child`, `:last-child`, `:not(compound)`, descendant and `>` combinators, and lists.
  `:hover`/`:focus`/… parse but never match (with a note). `+`/`~` are rejected for that selector
  only. Cascade is standard: `!important`, then specificity, then source order.
- **Properties:** `color`, `background`/`background-color` (colors only), `border`,
  `border-width/-color/-style` (solid/none), `border-radius`, `padding*`, `margin*`, `font-family`
  (first installed wins, generic → embedded), `font-size`, `font-weight`, `opacity`, `min-width`,
  `gap` (between zones on `bar`, modules in a `zone`, icon and label in a `module`), `min-height`
  (slider parts), and
  `-sliver-icon-size` (alias `-gtk-icon-size`). Units are `px`, `pt`, `em`, and bare numbers.
  `inherit`/`initial`/`unset` work. Inherited: color, font-*, icon size. `transition` is accepted
  and not yet animated.
- **GTK-isms** waybar users paste: `@define-color`, `@name`, `alpha()`, `shade()`, `mix()`,
  `lighter()`, `darker()`. **Imports:** `@import "file.css"` (relative to the importing file) and
  `@import "sliver:NAME"` for built-in themes. Other at-rules are reported and skipped.
- **Never fails on content.** Every problem is a `Diagnostic` (`file:line:col: message`), and only
  the bad declaration, selector or rule is dropped. `Stylesheet::load` errors only if the main
  file is unreadable. A typo can't blank the bar, and the safe layer doesn't use user CSS at all
  (sliverd renders it with the built-in `classic`).
- **Themes are stylesheets.** `assets/themes/classic.css` reproduces tiny-dfr pixel-for-pixel
  (identical snapshots to the old hard-coded theme). `sliver.css` is the default (T18, H10: macOS
  look + desktop accent on `.on` toggles and slider fills). `@accent` is always defined
  (`DEFAULT_ACCENT`); the agent overrides it via `Stylesheet::new()` + `define_color()` before
  `add_file()`.
- **Sliders** are styled through GTK/waybar child nodes `module > trough > {highlight, slider}`, so
  waybar slider CSS carries over. `Layout::tracks` gives the trough rect for touch → value.
  Toggles that are on match `:checked` (api.md §3); built-in themes also style `.on`.
- The tiny-dfr importer turns `ShowButtonOutlines=false` into
  `module { background: transparent }`.
- **Hot reload:** the agent's watcher (inotify) watches `Stylesheet::files()`, imports included,
  re-loads, and logs diagnostics. `Renderer` caches computed styles by `Stylesheet::generation()`,
  so a new sheet invalidates them automatically. Unchanged frames still cost nothing.
- **Zones** gained `width` (Auto/Fixed/Fill) and `align` (start/center/end), laid out by the same
  solver as modules (`geometry::solve`). waybar's left/center/right is Fill+start, Auto, Fill+end.

## Consequences
- `Theme` is gone. `Renderer::render(scene, &Stylesheet, pixmap)` returns the `Layout` (zone and
  item rects) it drew, so touch regions come from the same pass.
- No layout-affecting CSS beyond padding, margin, gap, min-width and border. `width` modes stay in
  config (H6), so layout can't be broken from the stylesheet.
- Later, as needed: `transition`/animations, `box-shadow`, gradients, per-part pseudo-elements
  (`::icon`, `::label`), `@media (layer)`-like queries.
