# Require explicit semantic migration

A change to domain semantics, Artifact encoding, or Verification Kernel contract creates a new Semantic Identity. Prior Domain Bundles remain incompatible until an explicit migration replays Verification and rebuilds affected indexes and learned state; Reflex never silently interprets old Artifacts under new meaning.

## Considered Options

Treating package versions as compatible by default would make upgrades frictionless but could preserve invalid claims under changed semantics. Rejecting all prior knowledge permanently would be safe but unnecessarily discard Artifacts that an explicit migration can re-establish.
