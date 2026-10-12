# sliver-settings UX

Owner: elm (T6). Status: **draft**. Answers to the taste questions in §11 may change it. The
feature tiers are in [features.md](features.md). The app is GTK4 + libadwaita (ADR-0001).

## 1. Principles

1. **The bar is the preview.** The on-screen preview is pixels from the same renderer as sliverd.
   It's never a GTK approximation. "Mirror to Touch Bar" puts the edit on the real bar live.
2. **Never strand the user.** On a machine with no physical Esc, no edit can remove Esc from the Base or Fn layer. On any machine, no edit can remove F1–F12 from the
   Fn layer (§7). Every destructive action can be undone.
3. **The 90% path is one click.** For example, "Customize for Firefox…" lives in the editor.
   Rules, inheritance and priorities exist but stay out of the way.
4. **Config is the truth.** The settings app writes the same TOML a human would and keeps comments.
   Everything it does is also possible through `sliverctl` or a text editor.
5. **Native everywhere.** libadwaita patterns: AdwNavigationSplitView, AdwPreferencesPage,
   toasts with Undo, instant apply (pending Q3). It works on GNOME, KDE and Hyprland, with no CSD
   assumptions beyond libadwaita's.

## 2. Model (the words users see)

| Term | Meaning |
|---|---|
| **Bar** | The physical strip. |
| **Item** | Anything on the bar: a button, widget, group or spacer. |
| **Widget** | An item with live content (clock, volume, now playing). |
| **Zone** | One of three fixed regions of a layout: **Esc** (left) · **Main** (flexible) · **Controls** (right, the collapsible control strip). The Esc zone is pinned and locked only on bars with no physical Esc (2170 px models). On 2008 px bars with a physical Esc (this Mac14,7, pending H2) it's empty by default and the user can put anything there, including an Esc. |
| **Layout** | Items arranged into zones. Has a name. |
| **Layer** | The slot a layout is shown in. **Base** is the default. **Fn** shows while Fn is held. Custom layers (Media, Emoji…) open from an action and close with their auto ✕ button, the bar's Esc if there is one, or an idle timeout. |
| **Rule** | "When ‹conditions›, use ‹layout› for ‹layer›". |
| **App controls** | A placeholder item. Items that the focused app publishes appear here. |
| **Profile** | A complete config: layouts, rules, appearance. One is active. |
| **Preset** | A shareable export of a layout or profile. |

Resolution (what the bar shows): the agent takes the current layer and finds the matching rules.
The **most specific** match wins (an app+title rule beats an app rule, which beats a state rule).
Ties go to the rule higher in the user's list. The Default rule always matches. The agent then
resolves inheritance and fills App controls. The daemon's safe layer bypasses all of this
(pending cedar: confirm the resolver lives in the agent).

### Inheritance
A layout can **extend** a parent. Inheritance is per **zone**: each zone is either *inherited*
(shown ghosted, with a "linked" chip) or *overridden*. To customize Firefox, you override only
Main and keep Esc and Controls from Default. Editing a ghosted zone asks once: "Override Main for
Firefox? [Override] [Edit Default instead]". Each zone menu has "Reset to parent".
Per-item diffs were rejected for now because they're hard to show and hard to reason about (Q5).

## 3. Information architecture

```
┌ Sliver ───────────────────────────────────────────────────────────┐
│ Sidebar             │ Page                                         │
│  ▣ Bar (editor)     │                                              │
│  ⇄ Layouts & Rules  │                                              │
│  ✋ Gestures         │                                              │
│  ◐ Appearance       │                                              │
│  ☀ Display & Power  │                                              │
│  ⧉ Apps             │                                              │
│  ⇪ Profiles & Presets│                                             │
│  ⓘ Status           │                                              │
└────────────────────────────────────────────────────────────────────┘
```

- **Bar**: the WYSIWYG editor (§4). This is the landing page.
- **Layouts & Rules**: all layouts, their conditions, priority and parents (§5).
- **Gestures**: global gestures (multi-finger swipes, long-press timing, double-tap on/off,
  tap-on-press vs tap-on-release).
- **Appearance**: theme, accent, font, icon theme, outlines, corner radius, animations and
  reduce motion, and a press-feedback style. Changes preview live on the bar strip at the top of
  the page.
- **Display & Power**: adaptive vs fixed brightness, curve editor (main-display % → bar level),
  idle dim after N s, off after N s, wake on touch/keypress, on-battery overrides, pixel shift.
- **Apps**: apps that have published items. Per app: Allow / Deny, a "may take the whole bar"
  toggle, and their item IDs, which can be pinned or hidden (§6).
- **Profiles & Presets**: switch, duplicate or rename profiles. Import tiny-dfr. Export or
  import a `.sliver` file.
- **Status**: daemon and agent health, the active layout and *why* it's active, the current
  layer, the compositor backend, recent errors (failed scripts), "Restart agent", and "Open
  config folder".

## 4. The editor (Bar page)

```
┌───────────────────────────────────────────────────────────────────────────┐
│ Editing [Default ▾]   Layer [Base|Fn|Media|+]   Preview as [Live ▾]  ⟲ ⟳ │
│ ┌───────────────────────────────────────────────────────────────────────┐ │
│ │ esc ┊ ◀ ▶ ⟳ │      App controls      │ ♪ Song – Artist ┊ ☀ 🔊 🔇 ‹ │ │  ← real render, scaled
│ └───────────────────────────────────────────────────────────────────────┘ │
│   Esc zone      Main zone                                Controls zone    │
│ [● Mirror to Touch Bar]                          [Customize for app… ▾]  │
├──────────────────────────────────────┬────────────────────────────────────┤
│ Library     [search…]                │ Inspector: Now playing             │
│ Keys  Media  System  Desktop  Info   │  Look   label/icon/colors/width    │
│ Toggles  Custom  Apps                │  Tap         Play/Pause         ▾  │
│ ┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐┌──┐     │  Long press  Choose player      ▾  │
│ │F1││☀ ││🔊││⏯ ││🕐││🔋││⌘ ││＋│ …   │  Swipe ←/→   Previous / Next    ▾  │
│ └──┘└──┘└──┘└──┘└──┘└──┘└──┘└──┘     │  Show only when  media playing  ▾  │
│                                      │  Advanced: id, refresh, script…    │
└──────────────────────────────────────┴────────────────────────────────────┘
```

### Interactions
- **Add**: drag from the Library onto the bar preview (as in macOS Customize Touch Bar). An insert
  caret shows the drop slot. Double-clicking a library tile appends it to the end of Main.
- **Reorder**: drag within or across zones. On bars with no physical Esc, items can't be dropped
  into the Esc zone on Base or Fn, where Esc stays first.
- **Size limits**: the editor warns when an item would be narrower than 80 px (~9 mm, the
  comfortable touch target) and won't go below 60 px. Those limits come from birch,
  features.md §9.
- **Remove**: drag off the bar (poof), press Delete, or use the context menu. A toast says
  "Removed Volume · Undo".
- **Resize**: a handle on the right edge of the selected item. Width is **Auto** (fits content),
  **Fixed** (px, snaps to 10 px, hold Alt for free), or **Fill** (shares leftover space by weight,
  like dfr `Stretch`). Double-click the handle to go back to Auto. The inspector shows the mode
  (Q4).
- **Group**: select several items (Shift/Ctrl-click), then "Group". This makes a folder item that
  opens a sub-bar. The editor enters the group with a breadcrumb `Default › Tools`.
- **Select**: click the bar preview. If Mirror is on, touching the real bar also selects in the
  editor and briefly outlines the item there.
- **Keyboard**: Tab/arrow moves between items. Alt+←/→ reorders. Delete removes. Ctrl+Z / Ctrl+Shift+Z
  undo and redo. Every item has an accessible name ("Volume slider, Controls zone, 3 of 4").
- **Overflow**: if items exceed 2008 px, the preview shows a red overflow region and the
  inspector suggests "Fill → Auto" or moving items into a group. Saving still works, and the
  renderer compresses Fill items first, then truncates labels with an ellipsis.
- **Preview as**: Live (the real current state), or simulate a context (app = Firefox, media
  playing, locked, on battery). This lets users check their rules without switching apps. It
  uses the headless render path.
- **Customize for app…**: pick a running app or a .desktop app. This creates layout "Firefox"
  extending the current one, adds a rule `app = firefox`, and switches the editor to it.
  It's the main path into per-app layouts.

### Mirror to Touch Bar
- On by default while the window is focused (Q10). It turns off after 60 s unfocused or when the
  window closes, and the bar returns to the resolved layout.
- The bar Esc (if present) and Fn keep working during mirroring. Pressing an item on the real bar while mirroring
  selects it **and** fires it, unless "Select only" is toggled.
- It's implemented as an agent "preview override". It never touches the daemon's safe layer.

### Inspector sections
1. **Look**: label, icon (an icon picker with search over the XDG theme, plus emoji tab and
   file), foreground/background colors (theme default unless set), width mode, outline override.
2. **Actions**: one row per gesture the widget supports. Each row is an action picker: Key/combo
   (a "press the keys" recorder), Command, Open app, Open URL, D-Bus call, Switch layer/layout,
   Toggle, Macro, None. The keyboard recorder has to work with no physical F row, so it also
   offers a list.
3. **Visibility**: always, or only when ‹condition› (the same condition builder as rules). This
   suits small things like "show the battery only when below 20%" without needing a new layout.
4. **Advanced**: stable item ID, refresh interval and timeout for scripts, the raw TOML for the
   item (editable with validation).

## 5. Layouts & Rules page

```
 Rules (first match wins among equally specific)        [+ Rule]
 ┌──────────────────────────────────────────────────────────────┐
 │ ≡ Locked screen     when screen locked       → Locked    Base │
 │ ≡ Terminals         app ∈ {foot, kitty}      → F-keys    Base │
 │ ≡ Firefox           app = firefox            → Firefox   Base │  ● active now
 │ ≡ Presenting        app = libreoffice-impress & fullscreen    │
 │ ─ Default           always                   → Default   Base │  (pinned last)
 └──────────────────────────────────────────────────────────────┘
 Layouts                                                [+ Layout]
   Default · Firefox (extends Default: Main) · Locked · F-keys (built-in)
```

- The condition builder has the pickers **App** (from running windows and .desktop files),
  **Window title** (contains/regex), **Workspace**, **Monitor**, and **State** (media playing,
  locked, on battery, battery < N%, fullscreen, DND, external display connected, time range).
  Conditions combine with *all of* / *any of*. One level only, with no nested boolean trees in
  the UI. Hand-written TOML can do more.
- "Active now" marks the winning rule. Clicking it explains why: "Firefox matched; Terminals
  didn't (app = firefox)".
- There's a built-in **F-keys** layout (F1–F12, with Esc first on bars that have no physical Esc). A rule pointing at it implements macOS's
  per-app "show F-keys", and the settings app offers this as a one-click option per app.
- **Locked screen** layout: the default contents are brightness, volume and media controls, plus Esc on bars with no physical Esc.
  App controls, script widgets and notification text are **never** shown while locked, whatever
  the layout says (privacy).

## 6. Apps (published items)

- When a focused app publishes items (API, pending cedar), they render inside the user's **App
  controls** placeholder. If a layout has no placeholder, app items don't show. That's how a
  user opts out per layout.
- An app may never cover the Esc zone or remove Fn → F-keys. Taking Main + Controls ("modal", e.g. a color picker)
  needs the per-app "may take the whole bar" toggle, which defaults on for the focused app and
  ends when focus leaves or on Esc.
- Default policy for new apps is **Allow** (Q6). The Apps page lists every app seen, with
  Allow/Deny and its item IDs. Users can hide individual app items, or *pin* one into a
  layout like a normal item (this needs stable item IDs, pending cedar).

## 7. Invariants enforced by the editor

| Rule | Enforcement |
|---|---|
| Esc is reachable | **No physical Esc** (2170 px bars, auto-detected from bar length, overridable in config): the Esc zone is locked and the Esc item can be restyled but not deleted or moved. **Physical Esc present** (2008 px bars): the Esc zone is optional. |
| Every item is touchable | Width below 80 px gets a warning; below 60 px is refused. |
| A dark bar doesn't fire | The first touch on a dimmed-off bar only wakes it. |
| Fn layer has F1–F12 | Validation: you can restyle, reorder, add F13–F24 or a group, but not delete F1–F12 (Q2). |
| A broken config never reaches the bar | The settings app validates before writing. The agent re-validates and keeps the last good config. The daemon's safe layer is the final backstop. |
| Scripts can't hang the bar | Each script has a timeout (default 2 s). On failure the item shows "!" and the error goes to Status. |
| Undo | Every editor change goes on an undo stack for the session. "Revert to how it was at launch" is in the menu. |

## 8. Onboarding (first launch)

1. "Sliver is running. If anything goes wrong, your Touch Bar falls back to F-keys (and Esc, if
   your Mac has no physical Esc key)."
2. If a tiny-dfr config is found: "Import your tiny-dfr setup?" Show a side-by-side render of
   the imported config. [Import] (default) / [Start fresh].
3. Choose a starting preset, each shown as a real render: **Classic** (dfr), **macOS-like**,
   **Desktop** (workspaces + now playing + clock) (Q1).
4. A 3-card tip strip: hold Fn for F-keys · press-and-slide volume · "Customize for app…".

## 9. Presets and profiles

- **Export** writes a `.sliver` file (zip of TOML + referenced icons). Absolute paths are
  rewritten and secrets or env aren't included.
- **Import** previews the render, then lists **every command, script and D-Bus call** in the
  preset with an "I've reviewed these" checkbox before enabling. Imported scripts start disabled
  if unchecked.
- **Profiles** let you switch the whole config from Status, or from a "Profile" action on the
  bar. Rules don't switch profiles (that would be two rule systems). State rules switch layouts
  instead.

## 10. Errors and degraded states

| State | Bar shows | Settings shows |
|---|---|---|
| Agent not running | Daemon safe layer | Banner: "Bar is in safe mode, the session agent isn't running [Start]" |
| Config invalid (hand edit) | Last good config | Banner with file:line and [Open file] |
| Compositor backend unavailable | Default layout only (no per-app rules) | Status: "Per-app layouts unavailable: ‹reason›" |
| Script fails / times out | "!" on that item | Status log entry, highlight in the editor |
| App item misbehaves (flooding updates) | The agent rate-limits it and then freezes it | Apps page shows "throttled" |

## 11. Taste questions (asked to the human, H#)

| # | Question | Options | Pick |
|---|---|---|---|
| Q1 | Fresh-install default bar | A dfr classic (F-keys, Fn → media) · B macOS-like (Esc zone · App controls · control strip, Fn → F-keys; the Esc zone is empty when a physical Esc exists) · C Desktop (Esc zone · workspaces · app controls · now playing · clock/battery · control strip) | **B**. tiny-dfr users keep their imported config. |
| Q2 | How locked is the Fn layer | A locked to F1–F12 (style only) · B editable but must keep Esc + F1–F12 · C fully editable | **B** |
| Q3 | Apply model | A instant apply + undo (GNOME HIG) · B explicit Save/Revert | **A** |
| Q4 | Item width model | A grid units only (dfr Stretch) · B free px · C Auto/Fixed/Fill per item | **C** |
| Q5 | Inheritance granularity | A per zone · B per item diff | **A** |
| Q6 | App-published items, default policy | A Allow · B Ask first time · C Deny | **A**. Apps can't cover Esc. Modal ends on Esc or focus loss. |
| Q7 | App name in menus | A "Sliver" (GenericName "Touch Bar Settings", Keywords touch bar) · B "Touch Bar" · C "Sliver Settings" | **A**. It avoids leaning on Apple's trademark and stays findable. |
| Q8 | Default visual style | A dfr look (outlined, white on black, bold) · B macOS-like (filled dark-gray rounded keys, no outlines) · C B + desktop accent for active toggles and sliders | **C** |
| Q9 | Collapsible control strip zone | A yes, macOS-style (collapsed 4 items ↔ expanded) · B no, just items | **A** |
| Q10 | Mirror to real bar while editing | A on by default · B off by default | **A**. The bar Esc (if any) and Fn still work. |

Answers get recorded here, and in an ADR if they constrain the config schema (Q2, Q4, Q5).
