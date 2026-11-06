# AGENTS.md

- README.md's four invariants are never traded away. `scripts/check` is the gate (CI runs it).
- Never stop tiny-dfr or the installed sliverd, take DRM master on the bar or open its input
  devices: real-hardware runs go only through `harness/safeswap/sliver-safeswap`, which gives the
  bar back. Tests and GUIs run headless. Root, the live desktop and anything public: ask first.
- One path per idea: spellings are normalised once, at load. A crate needs a boundary
  (privilege, a wire contract, test isolation, or two users).
- Docs: this file and README, a few lines each; the man pages and `docs/dbus/`, which tests hold
  to the code. Everything else is a `//!` comment beside the code, saying why, as facts.
