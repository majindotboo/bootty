# Native workspace demonstrations

Captured October 1, 2026, from the packaged Development app built at
`d9501b8e584cdea2306203827ac3b4c2d5ddbefb`. Its executable SHA-256 is
`1707f3179df5873c7e9d1a1bce13c758db898a600389bc6a37d41c348457238c`.
Later media-only changes do not change that executable.

The videos use native window captures at their observed cadence. Idle spans
longer than two seconds are trimmed; interaction playback is not accelerated.

- [Browser](browser.mp4), 5 seconds: literal address-bar typing, remembered site
  data after relaunch, Cmd+K over the page, and Escape returning to the browser.
- [Agent terminal](agent-launch.mp4), 5 seconds: a colored Pi terminal, Cmd+D
  creating a split, Cmd+T creating a terminal tab, and returning to the split.
  This disposable session used `--no-session --no-extensions`, received no
  prompt, and was closed afterward. The prior 9 native / 2 rmux / 0 tmux
  session counts were restored.

![Rounded project sessions, compact pacing and reset countdown](sessions.png)
![Empty sidebar with direct choices for tools and browser pages](empty-sidebar.png)
![Files in a closable top-level sidebar tab](files.png)
![Browser input and persistent site data](browser.png)
![Native provider history with account status and search](history.png)
![Agent terminal, split and independent terminal tab](agent-terminals.png)

These captures demonstrate the listed interactions, not completion of desktop
acceptance. Computer permission setup, a terminal worker coordination run,
annotations, saved logins and project setup still need current demonstrations.
Mobile acceptance remains paused until the desktop work is ready.
