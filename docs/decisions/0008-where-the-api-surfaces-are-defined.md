# ADR-0008: Where the two API surfaces are defined

Status: accepted · Amends: ADR-0003 (Ownership)

## Context
ADR-0003 put both API surfaces, the public D-Bus API and the private sliverd⇄agent IPC, in
`sliver-ipc`. The D-Bus interfaces were implemented later in their own crate, `sliver-api`,
because only the agent serves them and they need zbus, which sliverd doesn't. `sliver-ipc` kept
the D-Bus names, error names and limits, which sliverctl, the settings app and `sliver-client`
use too.

## Decision
- The private IPC is defined in `sliver-ipc` (`proto`, `validate`, `transport`, `shm`): its wire
  format, and the checks sliverd applies to it.
- The public D-Bus API is defined by `docs/dbus/io.github.wawow830.Sliver.xml` together with
  `sliver_ipc::dbus` (bus name, paths, interface and error names, limits). `sliver-api` implements
  it, and its introspection test compares what it serves with the XML, so the implementation
  can't add or drop a method, property or signal without the XML changing in the same commit.
- `docs/api.md` is the spec for app authors over both: what each call means and why.
- Changing either surface still takes amending ADR-0003, as it says.

## Consequences
- A client of the D-Bus API depends on `sliver-ipc` for names, or on the XML, never on
  `sliver-api`.
- sliverd builds without zbus.
