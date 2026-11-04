# ADR-0004: Config language (waybar model)

Status: accepted. Spec: [customization.md](../customization.md). Code: `crates/sliver-config`.

## Context
GOAL.md's design principle is to be as customizable as possible, with waybar as the reference.
The project owner settled the taste questions; layouts, including the Fn layer, were later made
fully editable.
We need one config language that the agent, settings app, sliverctl, importers and docs all share.

## Decisions
1. **TOML, kebab-case keys, waybar names wherever the concept maps.** waybar module blocks port
   with few edits. waybar gesture and type names are accepted as aliases (`on-click` → `on-tap`,
   `pulseaudio` → `audio`, `hyprland/workspaces` → `workspaces`).
2. **Everything is a module instance** (`type`, `type#instance`, `ns/name#instance`) with one
   shared set of common keys. Built-ins get no special powers (customization.md §3). `key/<Key>`
   is a parametric built-in that needs no block. Key names are `input_linux::Key` names
   (tiny-dfr compatible), matched case-insensitively. The table is generated from the kernel
   header (`crates/sliver-config/tools/gen_keys.py`).
3. **Structure is data.** Zones are user-defined per layout (any number and names). Layers map
   names to default layouts, with `base` and `fn` reserved. Rules are **first match, top to
   bottom**. "Most specific wins" was rejected because it lets `app = firefox` beat `locked`.
4. **Inheritance is per zone**. A child zone with the same name replaces the parent's in
   place, new names append, and `zone-order` reorders or drops.
5. **Widths are `auto | px | fill | { fill = N }`**. tiny-dfr `Stretch = N` is `Fill(N)`.
   Zones take `module-width` as the default for their modules. Pixel layout belongs to
   sliver-render; sliver-config never emits rects.
6. **Merging:** built-in defaults (`builtin.toml`) are always underneath. Above them sit the
   includes, then the including file, with the including file and earlier includes winning
   (waybar). Tables deep-merge and **arrays are replaced**, so redefining `layouts.x.zones`
   replaces the zone list.
7. **Errors vs warnings:** errors block applying (the last good config stays). Anything a user may
   legitimately do is at most a warning, including unknown keys, small touch targets, and **no
   F-keys or Esc on the bar**.
8. **Invariant 1 moves out of config** (follows from layouts being fully editable). The Fn layer and all
   layouts are fully editable. Reachability comes from sliverd's hard-wired **safe hatch**
   (double-press Fn and hold → built-in F-keys, plus Esc on bars with no physical Esc;
   implemented in `sliver_ipc::hatch`), so sliverd must not reject frames that lack F-keys. The validator only warns.
9. **Context-only Esc:** the defaults place `key/Esc#auto` with `when = { not = { physical-esc =
   true } }`. Unknown hardware fails safe (Esc shown), and this reproduces tiny-dfr's "Esc only on
   bars ≥ 2170 px".
10. **Privacy while locked** is enforced in the resolver (`HIDDEN_WHILE_LOCKED`), not by config.
11. **Power floors:** `interval` ≥ 100 ms (it warns under 1 s), `restart-interval` ≥ 1 s, and
    macros are capped at 32 steps / 10 s.

## Consequences
- The agent calls `sliver_config::load()` → `Config::validate(Hardware)` →
  `Config::resolve(layer, &Context)` on every context change, then hands `ResolvedZone`s to its
  module runtime and renderer. This is cheap: there's no I/O, and parsing a
  module block is a few µs.
- sliverd and api.md drop the frame-level "escape floor" in favor of the safe hatch.
- The tiny-dfr importer (`sliver_config::tinydfr`) writes `config.toml` + `style.css`. The
  stylesheet imports `sliver:classic` (tiny-dfr's look, shipped in sliver-render).
- Comment-preserving writes from the settings app (toml_edit) are later work.
