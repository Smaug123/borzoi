@AGENTS.md

After completing a change and committing it to a branch and doing all the usual pre-push flow, please also get a review from GPT-5.6 with `~/.local/bin/codex review --base main` (this will take several minutes and may output a vast amount of text before it finally summarises).
If you think its findings should be addressed, address them and repeat, but be aware of whether you're entering a doom loop: after each review which has substantive comments, ask yourself whether the review or the sequence of reviews is indicating something wrong with the overall approach, and whether you need to step back.
Additionally ask yourself whether you should have some *systematic* (e.g. exhaustive, property-based, or fuzzing) testing or structural change in place which would have avoided needing to apply intelligence to find the problem.

`codex review --base main` compares the current branch against `main`. It only produces a useful diff when you are on a non-`main` branch — if you accidentally commit to `main` directly, create a branch from the current HEAD, reset `main` to its parent (`git reset --hard HEAD~1`), switch back to the branch, and then rerun the review.

