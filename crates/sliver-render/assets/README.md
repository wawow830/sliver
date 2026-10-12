# sliver-render assets

Embedded into the binary so the built-in safe layer (Esc + F-keys) always renders, even with
no system fonts or icon themes (GOAL invariant 1). Also used for deterministic snapshot tests.

- `fonts/NotoSans-Bold-subset.ttf`: Noto Sans Bold 2.015 (from Fedora's `google-noto-sans-fonts`),
  subset with `pyftsubset` to Latin, Latin-1, Latin Extended-A, general punctuation, arrows,
  geometric shapes, € and key symbols (only what Noto Sans covers: it has no ⌘ ⌫ ⏎ arrows, which
  come from system fonts via fallback). SIL OFL 1.1, see `fonts/OFL.txt`.
  Regenerate:
  ```
  pyftsubset NotoSans-Bold.ttf --layout-features='*' --output-file=NotoSans-Bold-subset.ttf \
    --unicodes="U+0020-007E,U+00A0-017F,U+2000-206F,U+20AC,U+2190-21FF,U+2212,U+2318,U+2325,U+2326,U+232B,U+23CE,U+25A0-25FF"
  ```
- `icons/*.svg`: Material Symbols as shipped by tiny-dfr 0.3.7 (`share/tiny-dfr/`), Apache-2.0,
  see `icons/LICENSE`. White fill, 48x48 viewport.
