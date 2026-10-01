# Bootty Mobile

An experimental iOS companion for controlling a paired Bootty desktop. UIKit
hosts a GPUI Kit view of live Spaces, sessions, terminal captures, tabs, and
panes. The computer owns processes, topology, command targets, and mutations.

## Connect

On the computer, open the command palette and choose **Connect a phone…**.
Enable remote control and copy the masked pairing code. In the phone app,
choose **Connect** and paste it into the secure field. The code grants control
of that computer's sessions; keep it private. Reveal and Copy are explicit
desktop actions. Revoke disables the listener and invalidates its credentials;
enabling again creates a new certificate and code. Accepted commands retain
the desktop's existing bounded lifecycle.

The loopback listener supports simulator testing. A physical phone needs an
explicitly enabled LAN listener and a reachable computer address. Remote control
starts disabled and is not persisted across desktop restarts.

Choose a session to read its live terminal output and send text with Enter.
Esc, Tab, Ctrl+C, and arrow controls send terminal keys. Root-issued tabs and
pane controls let you choose a terminal, create a tab, or split a pane when the
backend supports it. While typing, **Done** dismisses the keyboard and restores
the topology controls. **New session** creates a shell or starts a Codex, Claude, or Pi terminal in
an absolute project directory. **Close…** requires explicit confirmation.
Unavailable backend operations remain disabled.

Reads refresh once per second while the app is active. Going offline disables
mutations; refresh reconnects automatically. Commands are never replayed on
reconnect. If delivery fails after sending, inspect the terminal before retrying.
The phone's selection does not move desktop focus.

## Connection and ownership

`bootty-control` owns an explicitly enabled TLS 1.3 gateway to its exact local
control server. The phone trusts only the certificate supplied in the pairing
code; public certificate roots and plaintext fallback are absent. An ephemeral
credential authenticates each bounded request. The gateway submits the existing
`CommandInvocation` through local IPC, preserving target generations, caller
checks, confirmations, and owner errors.

The phone stores its code in a device-only, unlocked Keychain item. A bounded,
protected temporary handoff is consumed and removed by Rust. **Disconnect**
removes the saved credential and stops refresh. No pairing code belongs in logs,
recordings, repository fixtures, or exported files.

The view consumes `spaces.list` and `terminal.capture`; it neither reconstructs
opaque targets nor owns a PTY or mux backend. Captures represent the desktop's
final terminal state. An unmodified ANSI parser projects colors and emphasis
into GPUI text and suppresses other control payloads. The default xterm palette
is used until the capture protocol supplies the computer's configured palette.

## Build and run

On an Apple Silicon Mac with Xcode 26 or newer:

```sh
rustup target add aarch64-apple-ios-sim
xcodebuild -downloadPlatform iOS -architectureVariant arm64
python3 mobile/build.py
xcrun simctl list devices available
```

Select one simulator UUID explicitly:

```sh
xcrun simctl boot UUID
xcrun simctl bootstatus UUID -b
xcrun simctl install UUID mobile/dist/iphonesimulator/BoottyMobile.app
xcrun simctl launch UUID dev.bootty.mobile.dev
```

The build script takes `/tmp/bootty-cargo-build.lock` with Python `fcntl.flock`
for Cargo. Other Bootty builds and commit hooks must use that same lock.
`mobile/` is a separate Cargo workspace, keeping desktop dependencies and
packaging independent of the experimental iOS backend. Use `mise run launch`
for an isolated Development desktop; release builds without `bootty-dev` use
Production state and are unsuitable for this test.

## Validation

Serialize these Cargo commands with the shared lock:

```sh
cargo fmt --manifest-path mobile/Cargo.toml -- --check
cargo nextest run --manifest-path mobile/Cargo.toml
cargo clippy --manifest-path mobile/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path mobile/Cargo.toml --target aarch64-apple-ios-sim --lib -- -D warnings
python3 mobile/build.py
```

The integration tests exercise TLS certificate trust, opaque target and Unicode
preservation, credential/protocol errors, size limits, incompatible topology,
and formatted terminal text. Exercise desktop pairing and revocation, live
session creation/input/closure, tabs/splits, background/foreground, keyboard
layout, rotation, large text, and themes in the actual simulator.

## Simulator evidence

The iPhone 17 / iOS 27 captures show Bootty’s dark mobile chrome paired over
loopback to an isolated Development desktop:

- [Spaces and live sessions](media/sessions.png)
- [Project directory and explicit agent provider picker](media/new-session.png)
- [Live terminal output, tabs, panes, and input strip](media/live-terminal.png)
- [Terminal chrome and navigation, 20 seconds](media/workspace-control.mp4)

The earlier control recordings cover Unicode input, native/tmux tabs and splits,
and an interactive Claude terminal. They precede the visual update:

- [Unicode input and colored live output, 14 seconds](media/live-input.mp4)
- [Native tab and pane creation, 19 seconds](media/native-topology.mp4)
- [tmux tab, split, and terminal input, 19 seconds](media/tmux-topology.mp4)
- [Claude terminal help interaction, 18 seconds](media/agent-control.mp4)
- [Revocation disables phone controls](media/revoked.png)

The visual pass verified readable portrait and landscape layouts, increased
Dynamic Type, provider icons, real terminal glyphs, and live tab/split creation.
The disposable session was closed, the listener revoked, and the phone credential
removed. The existing 9 native and 2 rmux sessions retained their exact targets
and order. The software keyboard did not appear in this Device Hub pass, so this
pass does not establish its layout. Earlier software keyboard checks remain
separate evidence. No model prompt was submitted.

## Platform limits

The mobile platform is an unmodified, pinned external dependency. Its GPUI core
and renderer use 0.3.7, matching GPUI Kit 0.7.0. Two unused backend features
(`camera`, `video_player`) are enabled because the pinned iOS platform references
them unconditionally. The app requests neither permission.

UIKit owns navigation, safe areas, keyboard geometry, pairing, and frame
delivery. The upstream embedding bridge hosts one GPUI view for the process
lifetime. Demand-driven frames pause when GPUI has no work and stop while hidden.
The mobile shell uses Bootty’s dark charcoal surfaces and purple accent. UIKit
forwards Dynamic Type into the theme; controls keep at least a 44-point touch
target. Provider outlines come from the desktop’s icon catalog; common icons
come from GPUI Kit. SVG data is embedded through the public `Icon.data` API
because the pinned embedding bridge cannot install an asset source. Licenses
are in `assets/LICENSE.md`. The host bridges
input focus to public keyboard APIs, disables smart punctuation, and commits
native paste after editing without committing marked IME text. The pointer-free
Rust exports have local symbol-attribute lint exceptions, without unsafe Rust
memory operations.

This is a polling terminal companion, with bounded captures of 80 history lines;
it does not stream terminal frames, resize the desktop terminal, render images,
or forward terminal mouse input. Herdr remains opaque and does not expose inner
tabs or panes. Device Hub exposes GPUI content as one accessibility group;
full VoiceOver navigation remains unverified. Android is not implemented.
iOS 16 is the deployment minimum; simulator execution establishes evidence only
for the runtime actually tested.

`rustup target add aarch64-apple-ios` and `python3 mobile/build.py --device` produce
an unsigned device app. Physical installation needs a development team,
provisioning, and signing. Physical-device performance and App Store readiness
remain unverified.
