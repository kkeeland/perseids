---
description: Run this repo's CI on parallel Boat boxes for the current commit (perseids)
argument-hint: "[job...] [-w workflow] [--keep-going] [--dirty]"
---
Use the perseids skill. Run `perseids run $ARGUMENTS` from the repo root (the plugin's `bin/perseids` if `perseids` isn't on PATH), in the background if it may take more than two minutes. Then report the verdict from the exit code and summary: the SHA, each job's result, and for any failure, the step and the cause from its log. Don't call an incomplete run green.
