# Git and Commits

## Commit Messages

Follow the existing project style. First line is an imperative summary (under 72 characters). If a body is needed, leave a blank line and explain *why*, not *what*.

```
Add frame allocator for 4 KiB page granularity

The PMM allocates 2 MiB regions but shard page tables need 4 KiB
frames. This adds a sub-region bitmap allocator that carves frames
from PMM-allocated regions.
```

Use verbs that match the change: "Add" for new code, "Fix" for bugs, "Remove" for deletions, "Update" for modifications to existing behavior. Don't use "Refactor" unless the behavior is truly unchanged.

## Commit Granularity

One logical change per commit. A commit should be reviewable in isolation:

- Adding a new module: one commit
- Fixing a bug: one commit (include the test or verification)
- Updating documentation: one commit

Don't mix unrelated changes. Don't split a single feature across commits that break the build individually.

## Branch Workflow

Work on `main` for now (single developer). When collaborators join, switch to feature branches with PR review.

## What NOT to Commit

- Build artifacts (`target/`)
- Editor config (`.vscode/`, `.idea/`)
- Credentials or secrets
- Temporary debug `serial_println!()` calls that aren't part of the final output
- `#[allow(dead_code)]` that papers over cleanup you should do

## Tags

Tag milestone completions: `v0.1.0`, `v0.2.0`, etc. Tags are on the commit that completes the milestone, after docs are updated.
