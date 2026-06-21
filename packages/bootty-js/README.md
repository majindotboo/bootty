# bootty.js

Web terminal rendering for Bootty frames.

`bootty.js` is for browser apps that want to draw a Bootty terminal frame into a
`<canvas>`, plus Node tools that need to inspect or snapshot the same frame
schema. It does not start a PTY, spawn a shell, or provide the Bootty desktop app.

## Install

```sh
npm install bootty.js
```

```sh
pnpm add bootty.js
```

```sh
yarn add bootty.js
```

```sh
bun add bootty.js
```

## Entrypoints

| Entrypoint | Runtime | Use it for |
| --- | --- | --- |
| `bootty.js/browser` | Browser | WebGL canvas rendering, input forwarding, clipboard/selection handling, and the bundled Rust site backend. |
| `bootty.js/node` | Node | Frame construction, frame-to-text snapshots, and shared TypeScript types without DOM or WebGL dependencies. |

## Mount a browser terminal

```ts
import { createRustSiteBackend, mountCanvasTerminal } from "bootty.js/browser";

const canvas = document.querySelector<HTMLCanvasElement>("#terminal");
if (!canvas) throw new Error("Missing #terminal canvas");

const terminal = await mountCanvasTerminal({
  canvas,
  backend: () => createRustSiteBackend({ page: "docs" }),
  cols: 96,
  rows: 32,
  fps: 30,
  onFrame(frame) {
    console.info(`${frame.cols}x${frame.rows}`);
  },
  onError(error) {
    console.error("Bootty terminal failed", error);
  },
});

await terminal.write("j");
```

`mountCanvasTerminal` owns the browser glue around a canvas:

- creates a `WebGlTerminalRenderer`;
- starts the supplied `TerminalBackend`;
- forwards keyboard, mouse, resize, and copy events;
- exposes `refresh`, `resize`, `write`, and `dispose` on the mounted terminal.

## Provide your own backend

Any renderer backend implements the `TerminalBackend` contract.

```ts
import { createEmptyFrame, type TerminalBackend } from "bootty.js/browser";

export function createStaticBackend(): TerminalBackend {
  let frame = createEmptyFrame({ cols: 80, rows: 24 });

  return {
    label: "static",
    async start() {
      return frame;
    },
    async readFrame() {
      return frame;
    },
    async resize(request) {
      frame = createEmptyFrame({ cols: request.cols, rows: request.rows });
      return frame;
    },
    async write(input) {
      console.log(input);
    },
  };
}
```

The optional backend hooks are:

- `key(event)` for keyboard events;
- `mouse(event)` for pointer and wheel events;
- `fps(value)` for host frame-rate reporting;
- `selectedText()` for copy behavior.

## Use frame utilities in Node

```js
import { createBlankCell, createEmptyFrame, frameSize, frameToText } from "bootty.js/node";

const frame = createEmptyFrame({ cols: 24, rows: 4 });
frame.cells.push(...Array.from("Bootty", (text, x) => createBlankCell(x, 0, { text })));

console.log(frameSize(frame));
console.log(frameToText(frame));
```

`bootty.js/node` intentionally exports frame utilities and shared types only.
Importing browser rendering from Node throws a clear runtime error instead of
pretending DOM or WebGL are available.

## Browser exports

```ts
import {
  WebGlTerminalRenderer,
  createRustSiteBackend,
  mountCanvasTerminal,
  rustSiteNavigation,
  frameToText,
  frameRows,
  frameSize,
  cellAt,
  createBlankCell,
  createEmptyFrame,
  selectedFrameText,
  type TerminalBackend,
  type WebTerminalFrame,
} from "bootty.js/browser";
```

## Node exports

```ts
import {
  createBlankCell,
  createEmptyFrame,
  frameRows,
  frameSize,
  frameToText,
  cellAt,
  type TerminalBackend,
  type WebTerminalFrame,
} from "bootty.js/node";
```

## Examples

- `examples/browser` mounts the bundled Rust site backend into a full-page canvas.
- `examples/node/frame-summary.mjs` creates a frame and prints a text snapshot.
