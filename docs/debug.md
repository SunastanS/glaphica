---
name: systematic-debug
description: Use this skill when investigating, reproducing, isolating, fixing, and documenting bugs. Applies to runtime errors, failing tests, incorrect behavior, regressions, flaky behavior, performance anomalies, and user-reported “it should do X but does Y” issues. Prioritize reproducibility, observability, root-cause analysis, targeted fixes, regression tests, pattern-wide audits, and debug records.
---

# Systematic Debug Skill

This skill defines a disciplined workflow for debugging software.

Use it whenever the task is to diagnose or fix a bug, regression, unexpected behavior, failing test, crash, data corruption, incorrect output, flaky behavior, or suspicious performance issue.

The goal is not merely to make the symptom disappear. The goal is to understand the root cause, fix it at the right layer, prevent recurrence, and leave behind a useful debug record.

---

## Core Principles

### 1. Reproduce before repairing

Do not start by editing production code.

First establish what is currently happening, what should happen, and under what conditions the difference appears.

Prefer one of the following forms of reproducibility:

- a failing automated test;
- a minimal command that reliably fails;
- a small script or fixture;
- a logged trace with stable input and output;
- a dev assertion that fails at the earliest known incorrect state.

If no stable reproduction is available, improve observability before changing behavior.

### 2. Visibility before speculation

When the cause is unclear, add visibility.

Use:

- logs;
- trace output;
- focused assertions;
- intermediate value inspection;
- snapshot-style test fixtures;
- reduced test cases;
- instrumentation around component boundaries.

Prefer observing actual data flow over guessing from code shape.

### 3. Narrow the fault boundary

Debugging should progressively shrink the unknown region.

At each step, try to answer:

- Which input enters this component?
- Which output leaves this component?
- Is the output already wrong here?
- If not, where does it first become wrong?
- Which invariant was expected but violated?

Add assertions to locate the earliest point where the program state diverges from the expected state.

### 4. Distinguish bug from trade-off

Before fixing, decide whether the behavior is truly a bug.

A behavior may be:

- a real implementation defect;
- an undocumented edge case;
- an intentional architecture trade-off;
- a mismatch between user expectation and current design;
- a test expectation that no longer matches the model.

If the behavior is a trade-off rather than a bug, explain the trade-off and propose a design-level change instead of patching around it.

### 5. Root cause before patch

A fix is valid only when it explains why the observed behavior happened.

Avoid symptom patches unless explicitly marked as temporary and justified.

Bad fixes include:

- catching errors without understanding why they occur;
- adding fallback branches until the test passes;
- retrying operations without identifying the failure mode;
- type-casting away errors;
- special-casing in an unrelated middle layer;
- silently ignoring invalid states;
- weakening tests to match broken behavior.

### 6. Fix at the right layer

Fix the layer where the incorrect assumption, missing invariant, or wrong state transition originates.

A correct fix usually changes one of:

- the data model;
- the invariant;
- the parser/normalizer;
- the state transition;
- the boundary contract;
- the caller/callee responsibility split;
- the test fixture if the fixture encoded the wrong expectation.

### 7. Preserve context

Before modifying the suspected code, inspect its local history when possible.

Use commit history to understand:

- why the code was written this way;
- whether the behavior was introduced by a regression;
- which assumptions existed at the time;
- whether related code changed together;
- whether a previous fix already tried to address this issue.

Prefer:

```bash
git log --follow -- path/to/file
git blame path/to/file
git show <commit>
git diff <known-good>..<bad>
````

### 8. One bug may indicate a pattern

After fixing the immediate bug, search for the same error pattern across the repository.

Look for:

* duplicated logic;
* similar edge-case handling;
* repeated unsafe assumptions;
* similar conversions;
* similar boundary contracts;
* similar missing assertions;
* similar tests with incomplete coverage.

If the same pattern appears elsewhere, either fix it too or document why it is safe there.

### 9. Debugging should improve the system

Each debug session should consider whether the issue could be prevented by:

* a regression test;
* a property test;
* a dev assertion;
* a linter rule;
* a type-level constraint;
* CI coverage;
* better logging;
* better error messages;
* a clearer interface contract;
* documentation of an invariant.

### 10. Leave a debug record

For non-trivial bugs, write a debug record under `./docs/debug/`

The record should explain the observed behavior, reproduction, investigation path, root cause, fix, verification, and future prevention.

---

## Workflow

Follow this workflow unless the user explicitly requests a different process.

---

### Phase 1: Intake and behavior summary

Start by restating the bug in operational terms.

Capture:

* trigger condition;
* actual behavior;
* expected behavior;
* affected command, API, screen, file, test, or component;
* whether this is a regression;
* known good version, if available;
* environment details, if relevant.

---

### Phase 2: Reproduction

Create or identify a stable reproduction.

Preferred order:

1. Existing failing test.
2. New minimal failing test.
3. Minimal command or script.
4. Instrumented manual reproduction.
5. User-provided reproduction steps.

When writing a test, make it fail for the observed bug before changing production code.

The test should assert the externally meaningful behavior first. If the failure location remains unclear, add narrower assertions around internal boundaries.

---

### Phase 3: Visibility and trace

If the reproduction fails but the fault location is unclear, add temporary visibility.

Useful techniques:

* log inputs and outputs at component boundaries;
* add dev assertions for invariants;
* inspect normalized/intermediate data structures;
* compare actual vs expected state transitions;
* bisect the data flow;
* reduce the input case;
* isolate nondeterminism.

Do not keep noisy logs unless they become useful diagnostics. Remove temporary logs before finalizing, or convert them into proper debug-level logs/assertions.

---

### Phase 4: Fault localization

Narrow the bug to the smallest responsible unit.

A good localization identifies:

* the exact component/function/module;
* the incorrect assumption;
* the input shape that violates it;
* the first wrong state or wrong branch;
* why existing tests did not catch it.

---

### Phase 5: History check

Before editing the localized code, inspect commit history when available.

Look for:

* when the code was introduced;
* why it was introduced;
* whether later changes invalidated its assumptions;
* whether related tests or docs changed;
* whether a regression commit can be identified.

---

### Phase 6: Fix selection

Choose the least surprising fix that addresses the root cause.

Classify the fix:

#### Best fix

Use when the root cause is clear and the model can be corrected directly.

Examples:

* correct the state transition;
* enforce the invariant at construction time;
* repair the parser/normalizer;
* move responsibility to the right layer;
* make invalid states unrepresentable;
* update the boundary contract and callers together.

#### Acceptable edge-case fix

Use when the root cause is clear and the current model intentionally has a corner case.

Requirements:

* the corner case must be explicitly named;
* the special handling must live at the correct abstraction layer;
* tests must cover the corner case;
* the behavior must be documented if non-obvious.

#### Temporary workaround

Use only when:

* the root cause is known but cannot be fixed safely now; or
* an external dependency is broken; or
* the user explicitly needs a short-term mitigation.

Requirements:

* mark it as temporary;
* explain the risk;
* add a TODO with removal condition;
* prefer failing loudly over silently corrupting state.

#### Forbidden patch

Do not use:

* blind catch-and-ignore;
* retry loops without failure classification;
* fallback chains that hide invalid state;
* type casts to bypass checks;
* patching an unrelated middle layer;
* weakening assertions to pass tests;
* broad changes without a failing reproduction.

---

### Phase 7: Implementation

Implement the smallest coherent change that fixes the root cause.

During implementation:

* keep the failing reproduction intact;
* avoid unrelated refactors;
* preserve public behavior unless intentionally changed;
* update types/contracts/docs if the invariant changed;
* remove temporary diagnostic noise;
* keep useful assertions or tests.

If a broader refactor is tempting, separate it from the bug fix unless the refactor is necessary to fix the bug safely.

---

### Phase 8: Verification

Run the narrowest relevant checks first, then broader checks.

Preferred order:

1. The failing reproduction now passes.
2. Nearby unit tests pass.
3. Related integration tests pass.
4. Full test suite, if practical.
5. Lint/typecheck/format, if relevant.

If a check cannot be run, say why.

---

### Phase 9: Pattern search

Search the repository for similar mistakes.

Strategies:

* grep for similar function calls;
* search for repeated conversions;
* inspect similar modules;
* search for the same boundary assumption;
* search for duplicated conditionals;
* search for similar TODOs or comments;
* check tests that cover neighboring behavior.

---

### Phase 10: Prevention

After fixing, decide whether future recurrence can be prevented.

Consider:

* regression test;
* property test;
* invariant assertion;
* stricter type;
* linter rule;
* CI check;
* codegen/schema check;
* better logging;
* debug tooling;
* documentation.

### Phase 11: Debug record

For any non-trivial bug, create a Markdown file under:

```text
./docs/debug/
```

Suggested filename:

```text
YYYY-MM-DD-short-bug-name.md
```

---

## Collaboration Protocol

If multiple attempts fail to identify the root cause, ask the user for help with concrete artifacts.

Do not say only “I need more information.”

Provide:

1. A stable reproduction request.
2. The exact command/test to run.
3. The logs/assertions needed.
4. The current data-flow understanding.
5. The first point where the state becomes unknown.

For complex bugs, include enough detail that another engineer can understand the investigation without rereading the entire conversation.

---

## Anti-Patterns

Avoid these behaviors:

* editing production code before reproducing the bug;
* treating the first plausible cause as the root cause;
* fixing the symptom where it appears instead of where it originates;
* adding broad fallback behavior;
* suppressing errors;
* changing tests to match broken behavior;
* skipping history when history is available;
* ignoring similar code elsewhere;
* claiming verification without running or explaining checks;
* leaving temporary logs or debug code behind;
* failing to document a non-trivial root cause.

---

## Minimal Debug Checklist

Before finalizing a bug fix, confirm:

* [ ] Actual vs expected behavior is stated.
* [ ] Reproduction exists.
* [ ] Root cause is identified.
* [ ] Fix is at the right layer.
* [ ] Regression test or equivalent check exists.
* [ ] Similar patterns were searched.
* [ ] Relevant tests/checks were run or explicitly not run.
* [ ] Temporary debug artifacts were removed or converted into proper diagnostics.
* [ ] `./docs/debug/` record was created for non-trivial bugs.
