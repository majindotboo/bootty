# Native workspace demonstrations

Captured October 1, 2026, between 20:15 and 20:52 PDT, from the packaged
Development app built at `ade61d879fee0428acd05507deef2d8217008cde`.
Its executable SHA-256 was
`75a8dfffefece34004322d8d2fe1436cb5586c7894cdd3197a0ddde3ee7bd6ae`.
Project captures followed a repackage of the same source; their executable hash
was `76e7e69b8a125bb7444d5d0920d5cbb0c1e4bd203eb0c85bc98076023d0d5cf5`.
[Capture manifest](captures.json) records each artifact hash and capture time.

The videos use native window captures at their observed cadence. Interaction
playback is not accelerated.

- [Browser](browser.mp4), 3.4 seconds: literal address-bar typing, remembered
  site data, Cmd+K over the page, and Escape returning to the browser.
- [Project setup](project-setup.mp4), 4.6 seconds: select the detected project,
  choose the existing checkout and launch its terminal. The disposable session
  was closed, restoring the prior session counts.
- [Agent terminal](agent-launch.mp4), 4.2 seconds: a colored Pi terminal, Cmd+D
  creating a split, Cmd+T creating a terminal tab, and returning to the split.
  This disposable session used `--no-session --no-extensions`, received no
  prompt, and was closed afterward. The prior 9 native / 2 rmux / 0 tmux
  session counts were restored.

![Rounded project sessions, compact pacing and reset countdown](sessions.png)
![Empty sidebar with direct choices for tools and browser pages](empty-sidebar.png)
![Files in a closable top-level sidebar tab](files.png)
![Browser input and remembered site data](browser.png)
![Native provider history with account status and search](history.png)
![Agent terminal, split and independent terminal tab](agent-terminals.png)
![Page element annotation ready for review](annotations.png)
![Annotation feedback pasted without submitting the terminal](annotation-paste.png)
![Configured browser search and site data settings](browser-settings.png)
![Search terms loaded through the configured engine](browser-search.png)
![Ordinary terminal worker and recorded completion report](coordination.png)

![Detected project artwork](project-selection.png)
![Existing checkout choices](checkout-selection.png)
![Terminal and native provider launch choices](session-launch.png)

The coordination capture shows a persisted completion from a real task dispatch
in the preceding build. A worker message corrected its initially mistyped
executable path; the worker then invoked the exact completion command. Bootty
recorded the report against that terminal target and dispatch generation. Both
disposable worker sessions were closed.

These captures demonstrate the listed interactions, not completion of desktop
acceptance. OS computer permission setup
and saving/filling a dummy login remain unverified; confirmation is pending for
those actions. Mobile acceptance remains paused until the desktop work is ready.
