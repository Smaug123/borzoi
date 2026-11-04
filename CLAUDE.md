@AGENTS.md

After completing a change and committing it to a branch and doing all the usual pre-push flow, please also get a review from a GPT model with `~/.local/bin/codex review --base origin/main` (this will take several minutes and may output a vast amount of text before it finally summarises).
If you think its findings should be addressed, address them and repeat, but be aware of whether you're entering a doom loop: after each review which has substantive comments, ask yourself whether the review or the sequence of reviews is indicating something wrong with the overall approach, and whether you need to step back.
Additionally ask yourself whether you should have some *systematic* (e.g. exhaustive, property-based, or fuzzing) testing or structural change in place which would have avoided needing to apply intelligence to find the problem.

`codex review --base main` compares the current branch against `main`. It only produces a useful diff when you are on a non-`main` branch — if you accidentally commit to `main` directly, create a branch from the current HEAD, reset `main` to its parent (`git reset --hard HEAD~1`), switch back to the branch, and then rerun the review.

# Subagents

Don't hand back a final result or call `SubagentHandback` while required check results are still pending.

# Long-running work

The tests take a long time, especially on a loaded machine.
Expect at least 15mins for a test run, and use the background-task mechanism for external conditions which don't have a native notification.
Keep background tasks silent until meaningful state changes, so e.g. do not emit periodic "still waiting" messages.

When a tool promises a completion notification, rely on the notification: do independent work if available, otherwise yield to the harness until the notification arrives.
Don't issue `sleep`, foreground `until`/`while` loops, log reads, repeated status checks etc just to keep waiting, and don't create another task that waits for an already-tracked background task.
If a command times out and the tool says it moved into the background, retain its task ID and await the notification; don't relaunch the command or a waiter.

Inspect intermediate output only when it enables a concrete next action, the user requests progress, or you have evidence of a problem; don't do so simply because time has passed.
