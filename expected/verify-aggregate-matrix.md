# Test matrix

| Case | native | sim |
| --- | --- | --- |
| fixture (8 cases) | 1 N/A, 6 pass, 1 xfail | 1 N/A, 6 pass, 1 xfail |

## Failures

None.

## Expected failures

- `native` `fixture/trap/boom`: deliberate trap: the fixture pins the runner's poisoning containment (https://github.com/lann/component-test/issues/45)
- `sim` `fixture/trap/boom`: deliberate trap: the fixture pins the runner's poisoning containment (https://github.com/lann/component-test/issues/45)

## Summary

- `native`: 1 N/A, 6 pass, 1 xfail (8 total)
- `sim`: 1 N/A, 6 pass, 1 xfail (8 total)
