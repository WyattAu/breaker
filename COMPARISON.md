# breaker vs failsafe: head-to-head comparison
- State checks are a wash (~18 vs ~19 ns); both are a lock + enum compare.
| Error classification              | Yes (`failure_predicate`; non-failures pass through typed) | Yes (`FailurePredicate`)              |
| Half-open probe budget            | Yes; bounded concurrent probes with lock-free permits (`half_open_max_calls`, `success_threshold`) | Policy-internal        |
`OrElse` policy-combinator engine; it remains the more configurable policy
path, closure-based call), want batteries included; presets, metrics
strategies, typed error passthrough; or care about