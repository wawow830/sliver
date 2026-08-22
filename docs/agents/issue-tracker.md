# Issue tracker

Sliver tracks work in GitHub Issues for `wawow830/sliver`. Use the `gh` CLI from the repository root; do not create local `.scratch/` tickets.

## Common operations

```sh
gh issue create --title "..." --body "..."
gh issue view <number> --comments
gh issue list --state open --json number,title,body,labels,comments
gh issue comment <number> --body "..."
gh issue edit <number> --add-label "..."
gh issue edit <number> --remove-label "..."
gh issue close <number> --comment "..."
```

When a skill asks to publish a ticket, create a GitHub issue. When it asks for the relevant ticket, fetch it with `gh issue view <number> --comments` and include its labels.

Pull requests are not part of the triage request queue. Triage issues only unless this policy is changed here.
