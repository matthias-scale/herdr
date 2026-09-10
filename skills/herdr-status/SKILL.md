---
name: status
description: "Re-verify and recover work when Herdr sends /status to a stalled agent."
---

# Status

Re-open the current objective and verify its state from source artifacts,
running processes, and checks. Do not answer from memory.

If subagents exist, poll them now. Treat a subagent whose observable work has
not advanced as stalled. Restart stalled work when it is safe to restart.

If every active task is demonstrably progressing, reply only `Working`.

If the task is finished, report completion. If its state changed but work
remains, report the change and continue the work immediately.
