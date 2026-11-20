# Tests that failed once and then passed

A test that fails and then passes on its own is written here, with the date, the symptom and what was tried. **If it fails a second time, it is investigated before
anything else is merged**: it is not run again until it passes. A test leaves this list when its cause is found and fixed (the entry is then moved to the CHANGELOG), or
after a long time without a second failure, with a line that says so.

| Test | First seen | Symptom | What is known | Count |
|---|---|---|---|---|
| `tests/pty.py` | 2026-11-20, the full Python suite of the branch of PR 6, twice (it passed in the suite of PR 5) | `FAIL: fg, then Ctrl+C ends it and the shell goes on: no (fg_ctrl_c)`; every other step of the test was `ok` | **Not reproduced**: alone 3 times, alone in a background shell with no terminal twice, with twice as many busy processes as cores 4 times, and right after `view.py` (the script before it in the suite) twice, all passed. The step waited a fixed 0.5 s after `fg` before sending Ctrl+C; if the shell has not resumed the job by then, the interrupt goes to the shell and the job (`sleep 30`) takes the next line, so `after-2` never prints. That is the only race in the step, and it is the one failure that matches the symptom; the step now waits for `fg` to print the job before it sends Ctrl+C. Whether that was the cause is not known: if it fails again with this change, it is not that | 2 |
