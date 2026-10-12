# ADR-0002: Rendering stack

Status: accepted (ash, T4). Supersede with a new ADR.

## Decision
Pure Rust: **tiny-skia** (rasterizer) + **resvg/usvg** (SVG icons) + **cosmic-text** (shaping,
font fallback, bidi; glyphs rasterized by swash). No system C libraries. Lives in
`crates/sliver-render`.

## Why not cairo + pango + librsvg (tiny-dfr's stack)
| | pure Rust | cairo/pango/librsvg |
|---|---|---|
| Build deps | none (crates only) | `cairo-devel`, `pango-devel`, `librsvg2-devel` (sudo; librsvg-devel isn't installed) |
| Snapshot tests | bit-stable across machines with the embedded font | output depends on distro freetype/harfbuzz/pango versions, so CI (Ubuntu) ≠ Fedora |
| Root daemon attack surface | memory-safe parsing of user SVG/PNG/fonts | C parsers in a root process |
| Safe layer guarantee | embedded font + icons, renders with zero system state | needs fontconfig and installed fonts |
| Settings app preview | same renderer → RGBA buffer → `GdkMemoryTexture`: pixel-identical WYSIWYG | possible via cairo too |
| Text quality | grayscale AA, no hinting; fine at 32 px on a 60 px bar (see snapshots) | hinting, subpixel |
| Font fallback / scripts | cosmic-text fallback + harfrust shaping + bidi; verified CJK, Greek, Cyrillic, Arabic | fontconfig + pango: best in class |
| Color emoji | **gap**: swash does CBDT/COLRv0, Fedora ships Noto emoji as **COLRv1** only | cairo 1.18 does COLRv1 |

The emoji gap is a contained follow-up (paint COLRv1 via `skrifa`'s `ColorPainter` onto tiny-skia,
or bundle a CBDT/SVG emoji set). Everything else favors pure Rust, especially invariants 1 and 4.

## Shape of `sliver-render`
- `Renderer::layout(scene, sheet, w, h) -> Layout` (zone + item rects; `render` returns it too) measures `Auto` content and calls the pure
  `solve()`: zones (each Auto/Fixed/Fill with an `align`) of items with `Width::Auto | Fixed(px) | Fill(n)`
  (H6). `Fill(n)` spans n slots plus the gaps between them, so all-`Fill` rows reproduce tiny-dfr's
  `Stretch` geometry. Leftover space goes to `Fill`, otherwise it is placed per `align`. On
  overflow (ux.md §4), `Fill` compresses to `min_width`, then the widest items are capped at a
  common width like a tab strip, and labels shrink and then ellipsize. Use the **same rects** for IPC
  key/agent regions so touch can't drift from what's drawn (`hit_test` splits gaps down the middle).
- `Scene { classes, zones: Vec<Zone{name, classes, width, align, items}> }`, `Item { content: Spacer|Blank|Text|Icon|IconText,
  id, classes, width, pressed, disabled, background, foreground }`. This is a *resolved frame*, not config. The agent
  builds it from sliver-config's resolved layout plus live widget state. The IPC's `pressed`
  image is the same scene with every item `pressed`.
- Styling: a `Stylesheet` (waybar-style CSS, **ADR-0004**, docs/styling.md) replaced the original
  `Theme` struct. The built-in `classic` sheet matches tiny-dfr 0.3.7's look.
- `Renderer` keeps font, glyph and shaped-label caches. Full 2008x60 frame: ~1.3 ms cold, ~0.4 ms
  cached (release, M2). Render on change only, then `panel::damage` diffs frames into column
  spans for DRM dirty clips. `panel::blit_xrgb8888` rotates into the panel's framebuffer
  (`Orientation::Rotate90` = tiny-dfr's `translate(h,0); rotate(90°)`; birch to confirm on hardware).
- Labels shrink to fit (down to `min_size`), then ellipsize. Labels are optically centered on cap
  height, so "esc" and "F1" share a baseline.
- Icons: SVG (vector, any size) and PNG. **Symbolic** icons (built-ins, XDG `*-symbolic`) are
  drawn in the item's foreground color. XDG icon theme lookup isn't in yet (follow-up).
- Fonts: `Fonts::embedded()` (only the embedded Noto Sans Bold subset: deterministic, for the safe
  layer and tests) or `Fonts::system()` (all installed fonts via fontconfig *files*, plus embedded).

## Testing
`crates/sliver-render/tests/snapshots.rs` renders scenes with the embedded font and compares against
committed PNGs (tolerance ±2 per channel, ≤20 pixels). Bless with `SLIVER_BLESS=1`; failures are
written to `target/snapshot-failures/`. **Look at the PNGs** when they change. For ad-hoc renders:
`cargo run -p sliver-render --example bar -- out.png esc F1 icon:play_pause x3:Hello F5!`.

## Open
- Who renders: sliverd renders everything from a scene description sent by the agent, or the agent
  sends pixels for rich widgets? Owned by the IPC design (T5, cedar). `sliver-render` works in
  either process.
- Color emoji (COLRv1), XDG icon themes, animations, pixel shift, sliders/progress content.
