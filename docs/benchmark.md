# Benchmark: pretix's real CI on Perseids

Measured on 19–20 October 2026. Every number on this page comes from a run listed here. Nothing was picked out of a
larger set: every full run is shown, including the slow one and the one with a network error.

## The short version

Same commit, same workflow file, unchanged:

- **GitHub:** pretix's `tests.yml` took **25m18s**.
- **Perseids on `large` boxes (8 vCPU):** the full workflow took **15m26s, 22m11s and 17m50s** in three runs, for about
  **$0.08** a run.
- **Perseids on `xlarge` boxes (16 vCPU)** took **15m08s and 22m46s**: no better, because pretix runs pytest with 3
  workers.
- **The test steps** ran 1.9 to 3.9 times faster on large boxes than on GitHub.
- **The biggest cost was the upload.** Each box gets pretix's full git history from the laptop (906 MB), and that took
  about 2 to 8 minutes per job.
- **A warm image** cut the e2e job's on-box setup from 128–140 s to 56 s.

What the numbers do and don't show: on this one repo, Perseids returned pretix's whole CI verdict faster than GitHub's
free runners did. The gap comes from bigger machines running the tests, minus a large per-box history upload. This
says nothing about other repos, other networks or GitHub's larger paid runners.

## The repo

[pretix/pretix](https://github.com/pretix/pretix), an open-source ticketing system. Its
[`.github/workflows/tests.yml`](https://github.com/pretix/pretix/blob/e4b29f56a47788a52a606bdb5c71380b20da36f8/.github/workflows/tests.yml)
is Linux-only and has 6 jobs:

- a 5-entry matrix (Python 3.11, 3.13 and 3.14 × sqlite and postgres, with two entries excluded);
- an end-to-end job using Playwright.

Every job starts a `postgres:15` service. The jobs use `setup-python`, apt, uv, npm and pytest-xdist (`-n 3`).

**Commit:** `e4b29f56a47788a52a606bdb5c71380b20da36f8`. GitHub tested this exact commit on master in run
[38918325034](https://github.com/pretix/pretix/actions/runs/38918325034) (16 Oct 2026, success).

**How it was picked.** I looked for Linux-only CI files that run 10+ minutes and use services, a matrix and setup
actions. Candidates I ruled out:

| Repo | Why not |
|---|---|
| chatwoot | Fits feature-wise, but it has 20 jobs (we capped ourselves at 8 boxes) and finishes in 5 min on GitHub. |
| wagtail | Runs on ARM runners and uses `ankane/setup-opensearch`. |
| diaspora | Starts the MySQL that GitHub's runner image ships with; Boat boxes don't have it. |
| huginn | Its main jobs use coveralls and docker build/push actions. |
| netbox | `dorny/paths-filter` decides which jobs run. |

## Reproduce it

```bash
git clone https://github.com/pretix/pretix && cd pretix
git checkout e4b29f56a47788a52a606bdb5c71380b20da36f8
PERSEIDS_MAX=8 perseids run -w tests.yml --keep-going                        # large, the default
PERSEIDS_MAX=8 PERSEIDS_TYPE=xlarge perseids run -w tests.yml --keep-going
```

`--keep-going` runs a job's later steps after a failed one, so each run does the same steps as GitHub's green run. It
matters because of the Peppol test described under "Caveats".

Timings came from the board's state lines, each stamped with the time it was printed:

- booting → running is boot time;
- running → the creation time of the job's log file is the upload;
- booting → pass/fail is the job's total.

Step times are from the runner's own summary in each job's log. Wall time runs from launching `perseids run` to its
exit.

## GitHub against Perseids

GitHub's numbers are from run 38918325034: each job's `started_at` to `completed_at`, and the wall time from the
first job's start to the last job's finish. GitHub documents its standard runner for public repositories as 4 vCPU
and 16 GB; `large` Boat boxes have 8 vCPU and 16 GB (checked with `nproc` and `free`). `xlarge` has 16 vCPU.

| | GitHub (ubuntu-22.04) | large a | large b | large c | xlarge a | xlarge b |
|---|---|---|---|---|---|---|
| **Wall time** | **25m18s** | **15m26s** | **22m11s** | **17m50s** | **15m08s** | **22m46s** |
| test (3.11, postgres) | 25m18s | 12m04s | 22m01s | 17m38s | 13m50s | 22m37s |
| test (3.13, sqlite) | 23m32s | 14m57s | 13m09s | 9m41s | 8m32s | 18m08s |
| test (3.13, postgres) | 23m42s | 12m04s | 11m53s | 15m19s | 14m59s | 14m46s |
| test (3.14, sqlite) | 9m13s | 6m58s | 9m32s | 8m13s | 7m35s | 7m50s |
| test (3.14, postgres) | 17m53s | 11m07s | 8m22s | 11m34s | 13m00s | 10m47s |
| e2e | 3m02s | 8m23s | 4m11s | 6m03s | 4m06s | 7m10s |
| Job time, summed | 102m40s | 65m33s | 69m08s | 68m28s | 62m02s | 81m18s |
| Exit code | (success) | 2 | 2 | 2 | 1 | 2 |
| Box-minutes, cost estimate | (free on a public repo) | 65, $0.08 | 69, $0.08 | 68, $0.08 | 61, $0.21 | 81, $0.27 |

The test step alone (`Run tests` or `Run E2E tests`), on the same commit:

| Job | GitHub | large a | large b | large c | xlarge a | xlarge b |
|---|---|---|---|---|---|---|
| test (3.11, postgres) | 1294s | 450s | 674s | 684s | 505s | 1104s |
| test (3.13, sqlite) | 1304s | 426s | 340s | 335s | 314s | 435s |
| test (3.13, postgres) | 1171s | 473s | 460s | 555s | 563s | 617s |
| test (3.14, sqlite) | 468s | 169s | 187s | 175s | 167s | 254s |
| test (3.14, postgres) | 858s | 409s | 257s | 351s | 439s | 364s |
| e2e | 80s | 42s | 30s | 40s | 30s | 42s |

Upload of the git bundle, from box ready to upload done, per job:

- large-a: 108 to 335 s (143, 335, 120, 171, 108, 211)
- large-b: 108 to 458 s (458, 360, 108, 301, 119, 110)
- large-c: 157 to 242 s (190, 157, 201, 242, 201, 189)
- xlarge-a: 109 to 215 s (156, 127, 165, 215, 182, 109)
- xlarge-b: 111 to 503 s (118, 503, 111, 118, 135, 205)

Two other recent green GitHub runs of the same workflow, on neighbouring commits, show how much GitHub varies:

- [38874639131](https://github.com/pretix/pretix/actions/runs/38874639131) (`531758d4`, 16 Oct): 33m40s wall, 111
  job-min.
- [39259353056](https://github.com/pretix/pretix/actions/runs/39259353056) (`f1964170`, 19 Oct): 28m31s wall, 101
  job-min.

**xlarge did not reliably beat large** (15m08s and 22m46s). pretix runs pytest with `-n 3`, so 16 vCPU do the same work as 8,
at about 2.8 times the price.

## Where a Perseids job's time goes

Take test (3.13, sqlite) in large c, which ran 9m41s:

- 4 s booting the box;
- 157 s uploading the bundle;
- 390 s in the postgres service and the workflow's own steps, of which 335 s were `Run tests`;
- the remaining 30 s fetching the commit from the bundle, installing mise and Python, and sending the log back.

The upload is the part to fix. `perseids run` ships a git bundle with the commit's **full history**, which for pretix
is 906 MB. Every box gets its own copy, and all of them go up the laptop's link at the same time (6 × 906 MB):

- in full runs, a box's upload finished 108 to 503 s after it booted;
- a single box on its own took 132 to 564 s.

A bundle from a shallow clone doesn't work: the box rejects it because the parent commits are missing. Repos with a
small history don't pay this cost.

## Warm start: the e2e job, stock box against `perseids image build`

The e2e job has the heaviest setup: Python, pip and npm dependencies, gettext, and Playwright with its browsers and
system packages. The image comes from `perseids image build` with the
[`.perseids/setup.sh`](#the-setupsh-used) below. The build took 6m08s, once.

Each e2e run below ran on its own, as `perseids run -w tests.yml e2e`, alternating stock and warm. No other run from
this benchmark was uploading at the time. Other sessions on the same machine and account were running.

| Run | Boot | Upload | On-box setup until the test step | Time to first test step | `Run E2E tests` | Total |
|---|---|---|---|---|---|---|
| stock 1 | 6 s | 185 s | 128 s | 319 s | 41 s | 360 s |
| warm 1 | 17 s | 132 s | 56 s | 205 s | 28 s | 233 s |
| stock 2 | 4 s | 564 s | 140 s | 708 s | 27 s | 735 s |
| warm 2 | 14 s | 187 s | 56 s | 257 s | 24 s | 281 s |

"On-box setup" is the time to first test step minus boot and upload. It covers fetching the commit, starting postgres
and every workflow step before `Run E2E tests`. The image doesn't change the upload, so this column is the fair
comparison.

The steps that the image makes nearly free:

| Step | Stock 1 / 2 | Warm 1 / 2 |
|---|---|---|
| Install system dependencies | 10 / 13 s | 2 / 2 s |
| Install Python dependencies | 10 / 10 s | 2 / 2 s |
| Install JS dependencies | 4 / 3 s | 1 / 1 s |
| Install Playwright browsers | 37 / 38 s | 2 / 2 s |

A box started from an image took 14–17 s to boot, against 4–6 s for a stock box.

On GitHub the same job spent 96 s before its test step, helped by GitHub's preinstalled image and its pip cache.

### The setup.sh used

```bash
#!/bin/bash
# Warm image for pretix's tests.yml: what its jobs spend their first minutes on. Runs as root after images/base.sh.
set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get install -y gettext
sudo -u user -H bash -c '
set -euxo pipefail
for v in 3.11 3.13 3.14; do mise install -y python@$v; done
git clone -q https://github.com/pretix/pretix /tmp/pretix
cd /tmp/pretix && git checkout -q e4b29f56a47788a52a606bdb5c71380b20da36f8
for v in 3.11 3.13 3.14; do
  py=$(mise where python@$v)/bin
  $py/pip3 install -q uv && $py/uv pip install --python $py/python3 -e ".[dev]" psycopg2-binary
done
cd src && npm ci
$(mise where python@3.13)/bin/playwright install --with-deps
rm -rf /tmp/pretix
'
```

## What was skipped, and why

- **Skipped as no-ops on a fresh box:**
  - `actions/checkout`, because the commit arrives in the bundle;
  - `actions/cache`, because there is nothing to restore;
  - `actions/upload-artifact`.
- **`codecov/codecov-action` in test (3.13, postgres)** is reported as SKIPPED, because Perseids doesn't run other
  marketplace actions or send secrets. That job is `incomplete`, so every large run and xlarge b exit **2**, never 0.
- **Comparison:** every job ran on every run, so none is left out of it.

## Caveats

- **One pretix test checks the live internet.** `test_order_update_transmission_validation` does a DNS NAPTR lookup of a
  real Peppol participant on `edelivery.tech.ec.europa.eu`.
  - The production record now answers NXDOMAIN, from a laptop too. The test passes only when the second registry
    errors out instead of answering.
  - It failed in our first run (08:58 UTC, 19 Oct). GitHub's own run
    [39211047023](https://github.com/pretix/pretix/actions/runs/39211047023) failed the same test the same morning.
  - It passed in every run in the table. This is upstream flakiness, not a difference between Boat and GitHub.
- **xlarge a had one network error.** `npm ci` hit an npm registry `ECONNRESET` in test (3.11, postgres), so that run
  exits 1. Its tests still ran, with 6080 passed, and are counted.
- **Upload time depends on your uplink.** These runs went over this machine's connection, measured at 210 Mbps up,
  which other Perseids sessions on the same machine were also using. The upload speed it actually got varied a lot.
  On a faster link, or on a repo with less history, Perseids' times would be lower.
- **GitHub's run had caches and a preinstalled image. Perseids' stock boxes start empty.** Even so, GitHub's e2e job
  was faster (3m02s, against 4m06s–8m23s).
- **Costs are Perseids' own estimate:** box-seconds at the per-hour prices in `bin/perseids` ($0.072 for large, $0.20
  for xlarge), as printed on each run's summary line.
  - On a public repo, GitHub's standard runners cost nothing.
  - What the whole benchmark spent on Boat is under [Spend](#spend).
- **The run that hit the rate limit is not counted.** The first attempt lost 5 of 6 boxes to Boat's limit of 210 box
  starts an hour, which every session on the account shares. Waiting for the hourly window to pass avoided it after
  that.

## Spend

Boat use for everything on this page:

- **38 box starts.** The first rate-limited attempt also had 5 refused.
- **380 box-minutes, about $0.76**, adding up the summary lines of every run.
- **Two `image build` boxes, about 7½ large box-minutes in all.** The first build failed in our setup.sh: it used
  `uv --system` and hit the distro Python.
- **Storage:** the image `perseids-pretix-20261020-0103` is still on the account.

Raw result files (the board stamped with times, plus the per-job logs) are kept outside this repository.
