# Pin revisions for each Campaign

Every Campaign pins one Knowledge Revision and one Model Revision for its full lifetime. Training and Knowledge Consolidation may proceed concurrently and produce challengers, but promoted revisions are adopted only by newly constructed Campaigns; this makes outcomes reproducible and attributable while allowing the Runtime to learn continuously across bounded Campaigns.

## Considered Options

Hot-swapping revisions inside an active Campaign could exploit improvements sooner, but it would entangle multiple policies and knowledge states in one causal and recovery unit.
