# Approved native M2 performance policy P1

## Decision and immutable reference

The user explicitly replied **“approve. don't stop”** to the request to approve
P1 as written at commit **`cbe2b5da809eed7cc4b3406b222baf43e1e34a2d`**.
This approves the complete [P1 document at that revision](https://github.com/wawow830/sliver/blob/cbe2b5da809eed7cc4b3406b222baf43e1e34a2d/docs/research/native-performance-policy-candidate.md),
not only nominal 30 FPS. The original candidate is retained verbatim as the
approved snapshot, including its historical not-yet-approved status text.
This record supersedes that status; it does not silently revise its rules.

Approved claims and budgets include:

- Successful **broker updates**, not optical FPS or DMA retirement.
- Nominal 30 Hz, ≥1,785 unique scheduled in-window returns per 60 seconds
  (29.75/s), ≤15/1,800 software deadline misses for the two 30 Hz cases.
- ≤100 ms update gaps (including interval edges), opportunity completion,
  original allocation-to-disposition age and published residence.
- Every eligible isolated physical down: broker receipt → causally marked
  successful update ≤150 ms; ≥60 samples, ≥8 per 10 s window, ≤2 s receipt gaps,
  and ≤33,333,334 ns worsening between any earlier/later window maxima.
- The three native cases, separate requested-60-Hz load accounting, existing
  fake-adapter software headroom/drop checks, and production-seam slot tests.
- Predeclaration, warmup/measurement/drain closure, all-input causal chains,
  loss/error handling, exact build provenance and complete cleanup.
- A/B/C/C/B/A overhead comparisons with ≤0.5% call-rate loss and separate
  ≤1 ms p95 render-callback and complete-drive increases.

The full snapshot governs all details and equality/boundary rules. These are
approved product budgets, **not newly measured panel tolerances**. The archived
~29.67/s evidence still falls below the approved floor and is not regraded.

## Implementation and release boundary

#23 is now ready for agent implementation; #1 and #17 adopt P1 in place of
their native 60 FPS clauses while preserving the old requirement provenance.
The approved implementation/test seams are the existing private observation
and complete-frame marker seams, offline evidence-validator interface, and
real CLI/worker/canvas with fake broker hardware; no public diagnostic command,
Lua interface addition or frame-identity field in the broker payload is implied.

Implementation proceeds in tested slices. Until a versioned capture, trusted
local collection/provenance path, evaluator and release-verifier integration
are complete together, **no new native performance acceptance can be claimed**.
In particular, neither the experimental v0 reader nor legacy v1 TSV evidence
can become a P1 release pass by changing one FPS constant or supplying a native
source label. Existing legacy verifier code is not the approved P1 evaluator.

Approval permits implementation and software/RPM verification. It does not
itself run or certify hardware, authorize an unannounced service takeover,
waive #24 rollback revalidation, or turn a failed/incomplete ledger into a pass.
A fresh authorized complete #17 hardware transaction remains necessary.
