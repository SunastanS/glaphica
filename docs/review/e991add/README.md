# Review: glaphica @ e991add

- **Target**: Bottom-up reliability and architecture review
- **Commit**: e991add
- **Reason**: Treat current code as unreliable; recover intended design bottom-up
- **Status**: In progress

## Authority Order

When design evidence conflicts, use this order:

1. User replies in this review
2. Design documents
3. Existing code and tests

Existing code is evidence of current behavior, not proof of intended behavior.

## Working Rule

For each proposed change, first classify the scale:

- **Small confirmed fix**: implement immediately, add or update focused tests,
  and run the relevant verification.
- **Large or design-unclear change**: record it in this review folder first,
  continue the design grilling, then implement after the decision tree is clear.

Examples:

- Rejecting zero-size image creation is a small confirmed fix.
- Reworking tile ownership around non-`Copy` owning tile handles is a large
  design change and must be documented before implementation.

