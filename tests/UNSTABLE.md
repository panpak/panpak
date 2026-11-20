# Tests that failed once and then passed

A test that fails and then passes on its own is written here, with the date, the symptom and what was tried. **If it fails a second time, it is investigated before
anything else is merged**: it is not run again until it passes. A test leaves this list when its cause is found and fixed (the entry is then moved to the CHANGELOG), or
after a long time without a second failure, with a line that says so.

Worked example: `tests/pty.py` failed 3 times in the full suite and never alone. The failing step printed the answer of the terminal (`^C` and then the typed line, no prompt): the job did not die. Cause: the suite was started as a background command of a non-interactive shell, which makes SIGINT ignored for everything it starts, and an ignored signal is inherited through `exec`, so `sleep 30` in the shell of the test ignored Ctrl+C. Found by running the test with SIGINT ignored (it then fails every time); fixed by putting SIGINT back to its default in the child of the pty. The earlier guess (a wait of a fixed time after `fg`) was wrong, and the test says why it failed now.

| Test | First seen | Symptom | What is known | Count |
|---|---|---|---|---|
| (none now) | | | | |
