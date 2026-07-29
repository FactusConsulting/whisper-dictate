---
name: pr-gate
description: Run the merge gate for a pull request in this repo — fetch every automated-review thread, fix or decline each one, resolve it, react, verify the base branch, and only then merge. Use whenever you are about to merge a PR, when asked to "check Codex comments", when a review posts new findings, or to sweep already-merged PRs for threads that landed after the merge.
---

# PR merge gate

Nothing merges until every automated-review thread on it is resolved and the
base branch is the one you think it is. `AGENTS.md` § "Pull request review"
states the policy; this skill is the executable form, including the failure
modes that have actually bitten this repo.

Set once per session:

```sh
REPO=FactusConsulting/whisper-dictate
```

## Why this exists

Every step below traces to a real incident, not a hypothetical:

| Step | What went wrong without it |
| --- | --- |
| Settle window | 9 of the last 12 merged PRs had review threads posted **50–181 s after** the merge. They were never seen again. |
| Check before merge | #663 merged with 5 unaddressed P2s; they landed on `main`. |
| Verify base | #681 merged into `port/diag-drop-accounting`, not `main`. The work never reached `main` and a follow-up nearly deleted it. |
| Prove the test bites | A guard test on #672 passed with the guard **inverted**. |
| Post-merge sweep | The orphaned threads above are invisible to any pre-merge check. |

## 1. Settle — wait for reviews before you look

Reviewers post asynchronously after a push. Merging into that window is the
single largest source of escaped findings in this repo.

**Do not evaluate a PR until ≥ 5 minutes have passed since its last push.**
Check the gap first:

```sh
PR=<number>
gh api repos/$REPO/pulls/$PR --jq '.head.sha[0:8] + "  pushed " + .updated_at'
gh api repos/$REPO/pulls/$PR/reviews \
  --jq '.[] | select(.user.login | test("claude|codex|copilot|sonar";"i"))
        | "\(.user.login) reviewed \(.commit_id[0:8]) at \(.submitted_at)"'
```

A reviewer has seen the current code only when its `commit_id` matches
`head.sha`. If the newest review is against an older SHA, more findings are
probably still coming — wait.

**Reviews are not guaranteed.** Some PRs (e.g. #684) never receive a Codex
pass at all, and Claude fires only on `pull_request: opened`. So "no review
yet" is not proof that one is coming: after ~10 minutes with no review
against the current SHA, proceed on the strength of the rest of this gate.
Never block indefinitely on a reviewer that may never post.

## 2. Fetch every unresolved thread

```sh
gh api graphql -f query='
  query($owner:String!,$name:String!,$pr:Int!,$cursor:String){
    repository(owner:$owner,name:$name){
      pullRequest(number:$pr){
        title state baseRefName headRefName
        reviewThreads(first:50, after:$cursor){
          pageInfo{ hasNextPage endCursor }
          nodes{
            id isResolved isOutdated
            comments(first:1){ nodes{ databaseId path line originalLine body } }
          }
        }
      }
    }
  }' -F owner=FactusConsulting -F name=whisper-dictate -F pr=$PR
```

Loop on `hasNextPage` — busy PRs here exceed 50 threads. Select both `line`
and `originalLine`; outdated threads have a null `line`.

## 3. Resolve each thread — all four actions

For every unresolved thread, in order:

**a. Fix it, or decline it with a reason.** Prefer the smallest change that
addresses the finding. Reviewers here are usually right, but not always —
check each claim against the actual code before acting. A finding that is
genuinely wrong gets a reasoned reply and 👎, not a fix.

**b. Add a regression test, and prove it bites.** A test that passes against
the broken code is worse than no test: it certifies a defect. Temporarily
revert your fix, watch the test **fail**, restore the fix, watch it pass.
Report that you did this, per fix. No inline `#[cfg(test)] mod tests` — a
discipline scanner rejects them; use a companion `_tests.rs`.

**c. Reply, then resolve.**

```sh
gh api repos/$REPO/pulls/$PR/comments -f body="Fixed in <sha>: <what changed>" \
  -F in_reply_to=<comment_id>
gh api graphql -f query='mutation{ resolveReviewThread(input:{threadId:"PRRT_..."}){ thread{ isResolved } } }'
```

**d. React** so the reviewer's signal quality can be scored:

```sh
gh api repos/$REPO/pulls/comments/<comment_id>/reactions -f content='+1'   # real finding, fixed
gh api repos/$REPO/pulls/comments/<comment_id>/reactions -f content='-1'   # false positive / declined
```

A fix without the resolve is not done. A resolve without the reply leaves no
audit trail.

## 4. Re-settle after your fixes

Pushing fixes starts the clock again — the reviewer will read the new commit.
Return to step 1 and wait out the window against the **new** head SHA. This
loop typically runs 2–4 rounds on a substantial PR. That is normal; merging
after round 1 is what leaves findings on `main`.

## 5. Pre-merge verification

Run all of these. Any failure blocks the merge.

```sh
# Base branch — stacked PRs default to their parent, not main
gh api repos/$REPO/pulls/$PR --jq '.base.ref'          # must be: main

# Zero unresolved
gh api graphql -f query='{repository(owner:"FactusConsulting",name:"whisper-dictate"){
  pullRequest(number:'$PR'){ reviewThreads(first:100){ nodes{ isResolved } } } } }' \
  --jq '[.data.repository.pullRequest.reviewThreads.nodes[]|select(.isResolved==false)]|length'

# CI green and mergeable
gh pr view $PR --json mergeable,mergeStateStatus,statusCheckRollup \
  --jq '{mergeable,mergeStateStatus}'
```

A `CONFLICTING` PR gets no CI runs at all — a green-looking check list on a
conflicted PR is stale, not passing.

Then merge, squash:

```sh
gh pr merge $PR --squash --delete-branch
```

Never `--admin` without explicit approval from the user.

## 6. After merging a stacked PR

If the PR's base was anything other than `main`, confirm the work actually
reached `main`:

```sh
git fetch origin main
git merge-base --is-ancestor <pr-head-sha> origin/main && echo "on main" || echo "NOT ON MAIN"
```

If it is not on `main`, the work is stranded on a dead branch. Say so
immediately — do not rebase or cherry-pick over it until you have established
what is actually missing, or you will silently drop the PR entirely.

## 7. Post-merge sweep

Threads that arrive after a merge are invisible to every check above and to
GitHub's own conversation-resolution rule. Sweep periodically — at minimum
before cutting a release:

```sh
gh api graphql -f query='{repository(owner:"FactusConsulting",name:"whisper-dictate"){
  pullRequests(states:MERGED, first:15, orderBy:{field:UPDATED_AT,direction:DESC}){
    nodes{ number title mergedAt
      reviewThreads(first:80){ nodes{ id isResolved
        comments(first:1){ nodes{ databaseId path line createdAt } } } } } } } }'
```

Anything unresolved is a live defect on `main`, regardless of when it was
posted. Fix it on a fresh branch off `origin/main` and resolve the original
thread there.

## Standing constraints

These apply to every commit this skill produces:

- **Signed commits always.** Never `--no-gpg-sign`. Key: on-disk Ed25519 at
  `~/.ssh/id_ed25519_signing`. Signing works from PowerShell, not git-bash.
- **Never `--amend` a pushed commit.** Force-push only with
  `--force-with-lease`.
- **Build and run the full suite locally before pushing** — devcontainer
  `wd-dev:latest`; native clippy on the Windows dev box is broken.
- **No file over 500 lines**, no oversized methods; split into modules.
- **User-facing feature PRs must update**
  `scripts/integration/wayland-user-smoke.sh`.
