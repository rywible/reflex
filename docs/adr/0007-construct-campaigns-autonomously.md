# Construct Campaigns autonomously

Given a Domain Definition, at least one caller-supplied Optimization Goal, and an overall resource budget, the Reflex Runtime constructs and schedules its own Campaigns from Seed Sources, known Artifacts, and Measurements. Autonomous execution is a first-class Runtime behavior rather than orchestration assembled outside Reflex, but Reflex does not choose what improvement means.

## Consequences

Campaign planning, portfolio formation, training and evaluation cadence, and cross-Campaign budget allocation remain behind the Runtime interface. The Domain Definition must provide enough representative Seeds for Reflex to plan useful work without domain-authored search code.
