# Private broker source and role-bound raw storage

This continues [approved P1](native-performance-policy-approval.md) and the
[lockstep coordinator](native-performance-coordinator.md). It is **software
collection, not an installed native collector or release acceptance**. Ordinary
service constructors still do not acquire declarations, export sources or arm
capture. No broker payload, public command, Lua surface, service policy, driver
or historical acceptance ledger changes in this slice.

## Direct adapter source

`diagnostic_broker.rs::BrokerCapture` owns one explicitly provisioned broker raw
source and an exporting minimal sampler. `ObservedHardware::for_broker` installs
it immediately around the broker's concrete hardware adapter. The lifecycle
owner retains a handle and calls `finish()` only after the producer stops/joins.
A `StartupCapture` from [trusted provisioning](native-performance-provisioning.md)
can construct it only with a broker role binding. The lower-level explicit FD
constructor supports private software tests; it does not authenticate a process.
Neither constructor is called from installed startup yet.

Every mode records 32 explicit calibration probes and the aggregate cost before
work. A/B do not decode or retain markers. C independently decodes the immutable
complete `LogicalFrame` at broker entry, checks run/generation against the plan,
and records entry/call/return identity. The bytes decoded are exactly those
passed to the hardware adapter. Correlation metadata does **not** cross the
ordinary broker wire; no returned-call numbering establishes unique frames.
Replays retain their raw identity, and require later complete cohort assembly.

Minimal Present spans cover the supplied adapter's complete `present` operation,
not just an ioctl. The endpoint clock read occurs immediately on adapter return,
before detailed retention and before the IPC reply. C's `PresentReturned.at_ns`
and the minimal sample's `end_ns` share that exact reading. Decode and retention
are outside the bare hardware-call duration but inside the coordinator's full
supervisor-drive span. The old generic `ObservedHardware::new` smoke seam retains
its documented decorated-call timing; it is not used by this new source.

This timestamp is an authoritative *location* when placed around the real M2
adapter. The type system cannot prove that the supplied adapter is real hardware;
the launcher, installed-role discovery and build/ownership provenance must
establish that independently. A source wrapped around a supervisor's
`BrokerHardware` client is **not** a native broker source. Tests deliberately use
fake hardware and make no native or optical claim.

## Inputs, failures and closure

All modes retain every normalized input event and poll error. All events in one
adapter poll batch share the reading taken immediately after that batch returns;
raw sequence preserves original batch order. Receipt is not the event's supplied
`time`, a supervisor receipt, physical actuation or an optical timestamp. Empty
polls append nothing and do not take an unrecorded receipt clock reading.
Nonempty-batch clock errors remain raw source failures without changing the
hardware result. Source-complete forwarding and joining to actual child
callback/token mutations remain an assembler/coordinator obligation; this slice
does not manufacture cross-source order from equal clock readings.

Claim, reacquisition, owner confirmation, synthetic output, backlight operations
and release retain success/failure records. Any unexpected lifecycle work must be
classified against the predeclared interval by the eventual assembler; these
records alone do not implement recovery/suspend exclusion. Initial contact,
local-session and sole-device ownership checks likewise remain required.

Hardware results and cleanup behavior are preserved even after observation
failure: a decoder/storage failure neither invents adapter success nor prevents
release or fencing. Instead source health fails. C's foreign run/generation,
malformed markers, actual adapter/poll errors, abandoned minimal spans, clock
errors, overflow and post-closure activity cannot produce clean source closure.
An adapter panic poisons the source and is explicitly failed even if a caller
catches the panic and later requests closure.

`finish()` emits the minimal summary, checks it and the raw source, appends the
terminal record, checks that append too, then closes storage without copying its
record buffer. Healthy repeated finish is idempotent. Last-handle drop without
explicit finish records abandonment, not successful completion. Closing too early
does not silently disarm the wrapper: later work updates loss/after-close/failure
counters, including an empty poll. A collector snapshot taken before producers
join is never final evidence. Source closure does not prove fixture T/S/E closure,
complete dispositions, service/device cleanup or release success.

## Storage version 3

The same-host bounded raw layout now routes exactly one `SourceRole` per mapping:
Worker (1), Broker (2), or Supervisor (3). `Collector::for_role`,
`take_storage(role)` and `MappedWriter::receive_for_role` reject wrong-role tickets
and writers. Existing worker helpers remain worker-only. Collector snapshots
check magic/version/role/source-count/capacity/record-size against the collector's
own retained declaration both before and after reading. Mutating a writable
header cannot silently relabel a collector source.

Version 3 retains the version 2 allocation fields and adds fixed tags 33
(`BrokerConfigured`: mode, epoch, rate, causal flag) and 34 (`BrokerOperation`).
Run/generation and process authority live in the independently acquired immutable
provisioning declaration, not those writable configuration records. Matched
private binaries are required. Records remain 512 bytes with bounded explicit
encoding, strict tags/booleans and zero padding. CRC, header checks and size seals
are integrity/routing checks, **not authentication**.

## Software evidence and next integration

Tests cover role/ticket mismatches, repeated claims, header mutation, new tag
round trips and malformed encodings; all-mode input/error retention; exact
endpoint/minimal timestamp equality; clean repeated closure, post-close activity,
calibration/terminal overflow, abandonment and panic closure; empty-poll clock
avoidance and retained nonempty receipt clock failures. A real child renders
the unchanged private P1 fixture, the coordinator sends its complete frame through
the ordinary Unix broker protocol, and the broker source records a fake adapter
return strictly before the coordinator's IPC-return observation. A/B/C pass that
identity/timing check; a valid marker from a foreign declared run fails source
health without changing hardware-call success. This is bounded warmup software
work, **not a complete native case or the A/B/C/C/B/A overhead battery**.

Remaining: installed startup/control-channel discovery and lifecycle integration,
exact executable/RPM/fixture/observer provenance, physical input source and device
ownership checks, broker-to-supervisor receipt forwarding, complete bounded raw
assembly and native overhead comparison, coherent release schema/verifier gates,
then exact-source RPM checks and the full authorized #17/#24 hardware/rollback
transaction. P1 numbers and old failed evidence remain unchanged.
