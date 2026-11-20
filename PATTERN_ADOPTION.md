# PATTERN_ADOPTION

How `breaker` relates to the WyattAu pattern canon
(`WyattAu/pattern-standards`). Validated by
`pattern-standards/scripts/adoption-check.py`.

| Pattern | Status | Evidence |
|---|---|---|
| T13 | partial | src/state.rs:104 (StateMachine and Window are the decision core; the async shell is src/lib.rs and the Tower middleware shell is src/tower.rs; the clock read at src/state.rs:247 sits inside the core, so the split is not fully pure) |
| P51 | adopted | src/lib.rs:110 (Closed/Open/HalfOpen FSM with sliding-window and consecutive-failure tripping; bounded half-open probe permits, released by a drop guard at src/lib.rs:171) |
| P64 | adopted | tests/proptest.rs:42 (proptest invariants over generated configs and outcome sequences) |
| P66 | adopted | .github/workflows/mutation.yml:19 (scheduled cargo-mutants audit with a score gate; equivalent-mutant triage record in mutants-equivalent.txt) |
| P24 | n/a | canon baseline line names this repo; code shows no implementation of P24: the crate has no retry path (src/lib.rs:65 BackoffStrategy is the circuit-open wait of P51, not a retry budget) |

## Why some near-miss patterns are omitted

P24 (retry budgets) and P52 (bulkheads) have no implementation in this
crate: there is no retry path, and a library owns no shared resource
pools. The half-open permit cap is part of the canon P51 mechanism
("half-open probes with bounded concurrency"), not a separate bulkhead
claim.
