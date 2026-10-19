---
name: perseids
description: Run this repo's GitHub Actions CI on throwaway Boat cloud boxes, one box per job and matrix entry, all in parallel, for the exact local commit. Use before saying work is done, before pushing or opening a PR, when CI is slow, queued or costs Actions minutes, or when several agents need CI at once. It needs no push and spends no Actions minutes.
---

# perseids: parallel CI on boxes you own

`perseids run` reads `.github/workflows/ci.yml` (or the one workflow there) and starts one fresh Boat box per job and per
matrix entry, all at once. Each box tests the exact commit, then is deleted. A full run usually takes minutes and costs cents.

## Use it

```bash
perseids run                      # every job, waves in needs: order
perseids run test lint            # just these jobs (needs: ignored), each matrix entry on its own box
perseids run -w e2e.yml           # another workflow
perseids run --keep-going         # don't stop at a job's first failing step; see every failure in one run
perseids exec 'npm ci && npm test -- src/foo'   # any command on one box: the quick loop while you fix one thing
perseids run --dirty              # test uncommitted tracked changes (new untracked files aren't included)
PERSEIDS_EVENT=workflow_dispatch perseids run   # jobs see this event, so steps gated on it run too (default push)
```

When something that used to pass now fails and you don't know which commit broke it, run `perseids bisect '<command>'`
(add `--good <ref>` if you know a passing commit): it tests every commit at once and names the first bad one, exit 0.

If `perseids` isn't on PATH, run `bin/perseids` from this skill's plugin folder (two levels up from this file).

## Read the result

The exit code is the verdict:

| Exit | Means | What to do |
|---|---|---|
| 0 | every job passed | the commit is green |
| 1 | a job failed | the summary prints the failing step and the log's last lines; fix it and rerun **only that job** (`perseids run <job>`) |
| 2 | incomplete | a step or job it can't run on a box was skipped (macOS/Windows, a container job, an unsupported action, a secret). The summary marks it SKIPPED or unsupported. Say so and don't call it green. |
| 99 | a box failed (Boat, SSH, the image) | not your code: rerun once; if it repeats, report the error line |

Each job's full log is in the folder the summary names (`~/.local/state/perseids/logs/...`). To find the failure, grep it for `=== ` (step headers) and `FAIL`.

## Rules

- Commit first. perseids tests a commit, so the result belongs to that SHA. Quote the SHA when you report green.
- While iterating, rerun the one failing job or use `exec`. Use a full `perseids run` once at the end.
- Secrets are never sent to boxes, so `secrets.*` is empty there. Steps that need real credentials (deploys, paid
  APIs) fail or skip, which is expected.
- Never print the Boat key. It's read from `BOAT_API_KEY` or `~/.config/perseids/boat.env`. If perseids says there's no
  key, ask the user to run `perseids key` in their own terminal. Never ask them to paste a key into the chat.
- Boxes delete themselves. `perseids ls` shows what's running and `perseids reap` deletes boxes whose run died.
  `PERSEIDS_MAX` (default 10) caps boxes across all runs on the account, so many agents can share it safely.
- A repo that needs more on its boxes (a database, browsers, an exact toolchain) gets a `.perseids/setup.sh`, then
  `perseids image build` once; commit the `.perseids/image` it writes.
