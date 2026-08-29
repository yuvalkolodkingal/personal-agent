# Security-critical crate coverage

This report records the FIX-27 coverage gate for the native policy and tool gateway. It was
generated on 2026-08-30 with Rust 1.98.0 (`88d9e12ae`), LLVM 22.1.8, and exactly
`cargo-llvm-cov` 0.9.0.

## Reproduction

```sh
cargo llvm-cov --summary-only \
  -p personal-agent-policy \
  -p personal-agent-tools \
  --locked
```

The machine used for this report has distribution-packaged Rust, so `cargo-llvm-cov` and the
matching `llvm-tools-preview` binaries were supplied from temporary directories through `PATH`,
`LLVM_COV`, and `LLVM_PROFDATA`. Those environment variables affect only tool discovery; the
command, workspace sources, lockfile, and coverage instrumentation are unchanged.

## Report

```text
Filename                      Regions    Missed Regions     Cover   Functions  Missed Functions  Executed       Lines      Missed Lines     Cover    Branches   Missed Branches     Cover
-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
policy/src/lib.rs                 519                 2    99.61%          17                 0   100.00%         344                 0   100.00%           0                 0         -
tools/src/lib.rs                  981                63    93.58%          81                10    87.65%         690                45    93.48%           0                 0         -
-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
TOTAL                            1500                65    95.67%          98                10    89.80%        1034                45    95.65%           0                 0         -
```

`crates/policy` therefore exceeds the required 90% line threshold at **100.00%**. Rust's LLVM
coverage mapping reports no independent branch counters in this build, so the decision branches
are also inventoried explicitly below rather than inferring branch coverage from that `-` column.

| `PolicyEngine::decide` path | Exercising test |
|---|---|
| Missing capability scope → deny | `policy_gate_precedence_and_consent_cover_every_decide_branch` |
| Required user absent → deny | `policy_gate_precedence_and_consent_cover_every_decide_branch` |
| Reversible mutation lacks checkpoint → deny | `policy_gate_precedence_and_consent_cover_every_decide_branch` |
| Untrusted input controls cross-zone effect → ask | `every_effect_risk_and_zone_combination_matches_the_documented_decision` |
| Matching scoped consent → allow with grant ID | `policy_gate_precedence_and_consent_cover_every_decide_branch` |
| Always-confirm effect/risk without consent → ask | `every_effect_risk_and_zone_combination_matches_the_documented_decision` |
| Bounded read/reversible local work → allow without grant | `every_effect_risk_and_zone_combination_matches_the_documented_decision` |

The exhaustive decision test evaluates all 7 effects × 4 risks × all 128 subsets of the 7 data
zones: **3,584 combinations**. Separate grant tests cover expired grants, the call-count boundary,
inclusive and exceeded cost ceilings, non-finite estimated cost, unlimited cost, background use,
revocation, and every scoped dimension. Tool-gateway tests verify that policy-declared checkpoint
coverage is required and that missing or failed real checkpoints prevent execution. The redaction
suite adds 4,096 deterministic fuzz cases over nested JSON and the A-4 secret-shape corpus.
