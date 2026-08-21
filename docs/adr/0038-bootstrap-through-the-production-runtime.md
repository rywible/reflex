# Bootstrap through the production Runtime

A domain with no prior experience starts from a valid Bootstrap Revision that implements deterministic general-purpose search and allocation through the same Runtime interfaces used by learned Model Revisions. Learning produces challengers from observed experience, so cold start requires neither pretrained weights nor a disposable search engine.

## Considered Options

Requiring pretrained weights would prevent a new domain from being turned on locally. A separate bootstrapping engine could gather initial data but would create a second architecture whose behavior and evidence do not exercise the production loop.
