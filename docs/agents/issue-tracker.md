# Issue tracker: GitHub

Issues and specs for this repo live as GitHub issues. Use the `gh` CLI for all operations and infer the repository from `git remote`.

## Conventions

- Create: `gh issue create`
- Read: `gh issue view <number> --comments`
- List: `gh issue list`
- Comment: `gh issue comment <number>`
- Label: `gh issue edit <number> --add-label <label>`
- Close: `gh issue close <number> --comment "..."`
- Publishing a spec or ticket means creating a GitHub issue.
- Pull requests are not a triage request surface.

## Wayfinding

Use one `wayfinder:map` issue with linked child issues. Represent blocking through GitHub's native issue dependencies when available; otherwise use explicit `Blocked by: #<number>` lines. Claim work by assigning the issue and resolve it by recording the answer before closing it.
