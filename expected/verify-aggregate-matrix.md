# Test matrix

| Case | native | sim |
| --- | --- | --- |
| fixture (8 cases) | 1 FAIL, 1 N/A, 6 pass | 1 FAIL, 1 N/A, 6 pass |

## Failures

- `native` `fixture/trap/boom` FAIL: wasm trap: wasm `unreachable` instruction executed
  - diag: about to trap
  - (diagnostics truncated)
- `sim` `fixture/trap/boom` FAIL: wasm trap: wasm `unreachable` instruction executed
  - diag: about to trap
  - (diagnostics truncated)

## Summary

- `native`: 1 FAIL, 1 N/A, 6 pass (8 total)
- `sim`: 1 FAIL, 1 N/A, 6 pass (8 total)
