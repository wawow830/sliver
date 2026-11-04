# sliver

**Sliver turns the Touch Bar on Linux Macs into a first-class part of the Linux desktop.** It is
a complete Touch Bar platform and customizer that replaces tiny-dfr. Users get full control over
what the bar shows and does: layouts, per-app contexts, widgets, gestures and appearance. Every
app, script and desktop environment gets a standard way to put things on the bar. And the user
is never left without Escape or the F-keys.

- [docs/user-guide.md](docs/user-guide.md): install, configure, style, script; man pages in [man/](man)
- [GOAL.md](GOAL.md): the goal, what "complete" means, and the invariants
- [docs/decisions](docs/decisions): architecture decisions

Status: early. It drives the Touch Bar day to day on an M2 MacBook Pro 13" (`Mac14,7`) under
Hyprland. Other compositors, desktops and Macs are experimental: see the user guide's "What's been
tested where".

## License

Sliver is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Bundled third-party assets keep their own licenses (fonts: OFL-1.1, icons: Apache-2.0,
see `crates/sliver-render/assets/`), and code derived from tiny-dfr keeps its MIT notice.
