# Fork completed Domain Bundles for new Seeds

A completed Domain Bundle can initialize a new Improvement Session through `BundlePlan::Fork`. A Fork imports its replay-verified Artifacts, retained Experience, Knowledge Revision, and Model Revision, but starts search from the new request's Seed Scope, goals, and Resource Envelope. The source Search Frontier, deferred Candidates, pending-parent tail, and spent resources do not carry into the Fork.

`BundlePlan::Resume` remains exact continuation. It retains unfinished search and therefore remains bound to the compatible Goal Set, Seed Scope, Resource Envelope, and recovery tail that produced it. A Fork requires a completed source; attempting to Fork an interrupted Bundle fails as incompatible instead of silently abandoning restart-complete work.

This distinction makes portable learned state explicit. Training and evaluation corpora can share verified knowledge without relying on training to happen to exhaust every pending parent before a deadline. It also prevents an evaluation harness from accidentally continuing training-specific Candidates under held-out Seeds. Imported positive Experience still replays through the installed Verification Kernel, so a Fork changes search ownership but does not weaken Verification.

The public interface gains one semantic operation rather than a set of knobs for selecting Bundle segments. Runtime revision 14 pins Fork behavior, and internal Lean Development schemas advance. Existing Fresh and Resume behavior remains unchanged.
