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
| Author-filter the settle check | The first run of this skill's own wait loop passed instantly — matching the operator's reply records, not the reviewer's. |

## 1. Settle — wait for reviews before you look

Reviewers post asynchronously after a push. Merging into that window is the
single largest source of escaped findings in this repo.

**Do not evaluate a PR until ≥ 5 minutes have passed since its last push.**
Measure that from the head commit, **not** from the PR's `updated_at` — the
latter advances on any activity at all (a review, a comment, a label), so it
resets the window without the code having changed:

```sh
PR=<number>
HEAD=$(gh api repos/$REPO/pulls/$PR --jq '.head.sha')
gh api repos/$REPO/pulls/$PR/commits --jq '.[-1].commit.committer.date' # head pushed at
gh api repos/$REPO/pulls/$PR/reviews \
  --jq '.[] | select(.user.login | test("claude|codex|copilot|sonar";"i"))
        | "\(.user.login) reviewed \(.commit_id[0:8]) at \(.submitted_at)"'
```

A reviewer has seen the current code only when its `commit_id` matches
`head.sha`. If the newest review is against an older SHA, more findings are
probably still coming — wait.

**The author filter is part of the test, not decoration.** Replying to a
review comment creates a *review record authored by you*, stamped with the
current head SHA. So "does any review match head?" answers **yes** the instant
you finish replying to the previous round — on the strength of your own
replies, with the actual reviewer still one commit behind. Match on the
reviewer, never on the bare SHA:

```sh
# correct — waits for a bot review of this exact commit
until gh api repos/$REPO/pulls/$PR/reviews \
        --jq '.[] | select(.user.login | test("claude|codex|copilot|sonar";"i"))
              | .commit_id' | grep -qx "$HEAD"; do
  sleep 30
done

# WRONG — your own reply satisfies this immediately
until gh api repos/$REPO/pulls/$PR/reviews --jq '.[].commit_id' | grep -qx "$HEAD"; do ...
```

Give the loop a bounded number of iterations too, so a reviewer that never
posts cannot hang it (see the ceiling below).

**Reviews are not guaranteed, and the two reviewers behave differently.**

- **Codex** re-reviews on each push, unprompted. It is the reason the settle
  window exists.
- **Claude** fires once per PR, on `pull_request: opened` only —
  `.github/workflows/claude-review.yml` subscribes to that event and nothing
  else, deliberately. After you push fixes it will **not** read the new
  commit. Waiting for it is waiting for something that will never arrive. If
  a fresh Claude pass is genuinely worth it (a substantial rewrite, not a
  three-line fix), post an `@claude` comment to fire `claude.yml`, then wait.
- Some PRs (e.g. #684) receive no automated review at all.

So "no review yet" is never proof that one is coming. After ~10 minutes with
no review against the current SHA — and no `@claude` request outstanding —
proceed on the strength of the rest of this gate. Never block indefinitely on
a reviewer that may never post.

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
            comments(first:1){
              nodes{ databaseId path line originalLine body author{ login } }
            }
          }
        }
      }
    }
  }' -F owner=FactusConsulting -F name=whisper-dictate -F pr=$PR
```

Loop on `hasNextPage` — busy PRs here exceed 50 threads. Select both `line`
and `originalLine`; outdated threads have a null `line`.

**Split the results by author before doing anything else.** This gate governs
the four automated sources only:

```
claude | codex | copilot | sonar   (matched case-insensitively on author.login)
```

**Never resolve a human's review thread.** A human thread closes when that
person is satisfied, not when an agent judges the point addressed. Reply to
it, fix what it asks, and then leave it for them — or ask the user to close
it. Resolving someone's comment on their behalf destroys the signal that they
were still waiting.

## 3. Resolve each thread — all four actions

For every unresolved thread **from the four automated sources** — never a
human's, see step 2 — in order:

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

Pushing fixes starts the clock again **for Codex**, which reads each new
commit. Return to step 1 and wait out the window against the new head SHA.
This loop typically runs 2–4 rounds on a substantial PR — that is normal, and
merging after round 1 is what leaves findings on `main`.

Claude does not re-review on push (step 1). Do not wait on it; request it
explicitly with `@claude` if a fresh pass is warranted, otherwise proceed.

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

## 6. After merging, confirm the work reached `main`

**Do not test the PR head's ancestry.** A squash merge creates a new commit
with a different SHA, so the head is *never* an ancestor of `main` on the
documented `--squash` path — that check reports `NOT ON MAIN` on every
successful merge. Test the commit the merge actually produced:

```sh
git fetch origin main
MERGE_SHA=$(gh api repos/$REPO/pulls/$PR --jq '.merge_commit_sha')
git merge-base --is-ancestor $MERGE_SHA origin/main \
  && echo "landed on main" || echo "NOT ON MAIN"
```

For a squash merge `merge_commit_sha` is the squash commit itself, so this
holds for all three merge methods.

This matters most for **stacked** PRs, which default to their parent branch
rather than `main`. If the merge landed somewhere other than `main`, the work
is stranded on a branch that may itself never merge. Say so immediately — do
not rebase or cherry-pick over it until you have established what is actually
missing, or you will silently drop the PR entirely.

## 7. Post-merge sweep

Threads that arrive after a merge are invisible to every check above and to
GitHub's own conversation-resolution rule. Sweep periodically — at minimum
before cutting a release:

```sh
gh api graphql -f query='
  query($cursor:String){ repository(owner:"FactusConsulting",name:"whisper-dictate"){
    pullRequests(states:MERGED, first:25, after:$cursor,
                 orderBy:{field:UPDATED_AT,direction:DESC}){
      pageInfo{ hasNextPage endCursor }
      nodes{ number title mergedAt
        reviewThreads(first:100){
          pageInfo{ hasNextPage endCursor }
          nodes{ id isResolved
            comments(first:1){ nodes{ databaseId path line createdAt author{ login } } } } } } } } }'
```

**Both** connections paginate. Iterate the outer one back to your last sweep,
and follow `hasNextPage` on any PR whose threads hit the inner cap — a busy PR
here exceeds 100 threads. A fixed-size query silently truncates, and a sweep
that reports "nothing outstanding" because it stopped counting is worse than
no sweep.

An unresolved thread means **untriaged**, not "confirmed defect". Read each
one and check it against the code before writing anything: some late findings
are false positives, and the decline path in step 3a applies here exactly as
it does pre-merge. Only genuine findings get a fix branch off `origin/main`.
Either way the original thread gets a reply, a reaction, and a resolve.

## Standing constraints

These apply to every commit this skill produces:

- **Signed commits always.** Never `--no-gpg-sign`. Key: on-disk Ed25519 at
  `~/.ssh/id_ed25519_signing`. Signing works from PowerShell, not git-bash.
- **Never `--amend` a pushed commit.** Force-push only with
  `--force-with-lease`.
- **Build and run the full suite locally before pushing** — devcontainer
  `wd-dev:latest`; native clippy on the Windows dev box is broken.
- **No *new* file over ~500 lines**, no oversized methods; split into modules.
  The threshold applies to files you add, and to files your change pushes past
  the limit — it is not a demand to split a grandfathered file just because a
  review fix touched it. Widening a PR that way is its own defect.
- **User-facing feature PRs must update**
  `scripts/integration/wayland-user-smoke.sh`.
