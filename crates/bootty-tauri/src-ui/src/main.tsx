import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { createTerminalBackend } from "./terminal-api";
import type { TerminalBackend } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";
import { WebGlTerminalRenderer } from "./webgl-terminal";
import "./style.css";

type GridSize = { cols: number; rows: number };

function App() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<WebGlTerminalRenderer | null>(null);
  const frameRef = useRef<WebTerminalFrame | null>(null);
  const gridRef = useRef<GridSize | null>(null);
  const backendRef = useRef<TerminalBackend | null>(null);
  const resizeInFlightRef = useRef(false);
  const [status, setStatus] = useState("starting terminal");
  const fpsRef = useRef({ frames: 0, startedAt: performance.now() });
  const [fps, setFps] = useState(0);

  const focusTerminal = useCallback(() => {
    window.requestAnimationFrame(() => canvasRef.current?.focus());
  }, []);

  const resizeToCanvas = useCallback(async (backend: TerminalBackend, frame: WebTerminalFrame) => {
    const canvas = canvasRef.current;
    if (!canvas || resizeInFlightRef.current) {
      return frame;
    }

    const nextGrid = gridForCanvas(canvas, frame);
    const currentGrid = gridRef.current;
    if (currentGrid?.cols === nextGrid.cols && currentGrid.rows === nextGrid.rows) {
      return frame;
    }

    resizeInFlightRef.current = true;
    try {
      const resized = await backend.resize({
        cols: nextGrid.cols,
        rows: nextGrid.rows,
        cellWidth: frame.cellWidth,
        cellHeight: frame.cellHeight,
      });
      gridRef.current = nextGrid;
      frameRef.current = resized;
      return resized;
    } finally {
      resizeInFlightRef.current = false;
    }
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }

    const renderer = new WebGlTerminalRenderer(canvas);
    rendererRef.current = renderer;
    let stop = false;
    let animationFrame = 0;

    async function drawNextFrame() {
      try {
        const backend = backendRef.current;
        if (!backend) {
          return;
        }
        let frame = await backend.readFrame();
        if (stop) {
          return;
        }
        frame = await resizeToCanvas(backend, frame);
        if (stop) {
          return;
        }
        frameRef.current = frame;
        renderer.render(frame);
        recordFrame(fpsRef, setFps);
      } catch (error) {
        if (!stop) {
          setStatus(String(error));
        }
      }

      if (!stop) {
        animationFrame = window.requestAnimationFrame(() => {
          void drawNextFrame();
        });
      }
    }

    async function start() {
      const backend = await createTerminalBackend();
      backendRef.current = backend;
      let frame = await backend.start();
      if (stop) {
        return;
      }
      frame = await resizeToCanvas(backend, frame);
      if (stop) {
        return;
      }
      frameRef.current = frame;
      renderer.render(frame);
      recordFrame(fpsRef, setFps);
      setStatus(backend.label);
      animationFrame = window.requestAnimationFrame(() => {
        void drawNextFrame();
      });
    }

    start().catch((error) => setStatus(String(error)));

    return () => {
      stop = true;
      window.cancelAnimationFrame(animationFrame);
      renderer.dispose();
      backendRef.current = null;
      rendererRef.current = null;
    };
  }, [resizeToCanvas]);

  useEffect(() => {
    const onResize = async () => {
      const frame = frameRef.current;
      const renderer = rendererRef.current;
      const backend = backendRef.current;
      if (!frame || !renderer || !backend) {
        return;
      }
      try {
        const resized = await resizeToCanvas(backend, frame);
        frameRef.current = resized;
        renderer.render(resized);
      } catch (error) {
        setStatus(String(error));
      }
    };

    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [resizeToCanvas]);

  const sendInput = useCallback((input: string) => {
    if (input.length === 0) {
      return;
    }
    const backend = backendRef.current;
    if (!backend) {
      return;
    }
    backend.write(input).catch((error) => setStatus(String(error)));
  }, []);

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLCanvasElement>) => {
      const encoded = encodeKey(event);
      if (encoded == null) {
        return;
      }
      event.preventDefault();
      sendInput(encoded);
    },
    [sendInput],
  );

  return (
    <main className="site-shell">
      <nav className="site-nav" aria-label="Primary">
        <a className="brand" href="#top" aria-label="Bootty home">
          <img className="brand-logo" src="./bootty-logo.png" alt="" />
          <span>Bootty</span>
        </a>
        <div className="nav-links">
          <a href="#demo">Demo</a>
          <a href="#stack">Stack</a>
          <a href="https://github.com/majinboos/bootty">GitHub</a>
        </div>
      </nav>

      <section className="hero" id="top">
        <div className="hero-copy">
          <div className="hero-lockup">
            <img src="./bootty-logo.png" alt="Bootty logo" />
            <div>
              <strong>Bootty</strong>
              <span>Terminal renderer</span>
            </div>
          </div>
          <h1>Bootty renders terminals.</h1>
          <p className="hero-text">A GPU renderer for terminal frames.</p>
        </div>

        <TerminalDemo
          canvasRef={canvasRef}
          status={status}
          fps={fps}
          focusTerminal={focusTerminal}
          onKeyDown={onKeyDown}
          sendInput={sendInput}
        />
      </section>

      <section className="feature-grid" aria-label="Bootty highlights">
        <article>
          <span className="feature-kicker">Frames</span>
          <h2>Cells in.</h2>
          <p>Text, colors, cursor state, and images.</p>
        </article>
        <article>
          <span className="feature-kicker">GPU</span>
          <h2>Pixels out.</h2>
          <p>Terminal frames drawn by the renderer.</p>
        </article>
        <article>
          <span className="feature-kicker">Demo</span>
          <h2>Try it.</h2>
          <p>Click the terminal and type.</p>
        </article>
      </section>

      <section className="stack-section" id="stack">
        <div>
          <p className="eyebrow">Pieces</p>
          <h2>Small parts.</h2>
        </div>
        <div className="stack-list">
          <div>
            <strong>bootty-terminal</strong>
            <span>frames</span>
          </div>
          <div>
            <strong>bootty-render</strong>
            <span>GPU renderer</span>
          </div>
          <div>
            <strong>bootty-tauri</strong>
            <span>app and web demo</span>
          </div>
        </div>
      </section>

      <section className="demo-notes" aria-label="Demo instructions">
        <div>
          <p className="eyebrow">Demo</p>
          <h2>Try it.</h2>
        </div>
        <p>
          Run <code>help</code>, <code>ls</code>, or <code>vim demo.txt</code>.
        </p>
      </section>
    </main>
  );
}

type TerminalDemoProps = {
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  status: string;
  fps: number;
  focusTerminal: () => void;
  onKeyDown: (event: React.KeyboardEvent<HTMLCanvasElement>) => void;
  sendInput: (input: string) => void;
};

function TerminalDemo({ canvasRef, status, fps, focusTerminal, onKeyDown, sendInput }: TerminalDemoProps) {
  return (
    <section className="terminal-card" id="demo" aria-labelledby="terminal-demo-title">
      <div className="terminal-toolbar">
        <div>
          <p className="terminal-label" id="terminal-demo-title">
            Live terminal
          </p>
          <span>{status}</span>
        </div>
        <span>fps {fps.toFixed(1)}</span>
      </div>
      <div className="terminal-pad">
        <canvas
          ref={canvasRef}
          className="terminal"
          tabIndex={0}
          aria-label="Interactive Bootty terminal demo"
          onMouseDown={focusTerminal}
          onKeyDown={onKeyDown}
          onPaste={(event) => {
            event.preventDefault();
            sendInput(event.clipboardData.getData("text"));
          }}
        />
      </div>
    </section>
  );
}

function recordFrame(
  fpsRef: React.MutableRefObject<{ frames: number; startedAt: number }>,
  setFps: React.Dispatch<React.SetStateAction<number>>,
): void {
  const now = performance.now();
  const sample = fpsRef.current;
  sample.frames += 1;
  const elapsed = now - sample.startedAt;
  if (elapsed >= 1000) {
    setFps((sample.frames * 1000) / elapsed);
    fpsRef.current = { frames: 0, startedAt: now };
  }
}
function gridForCanvas(canvas: HTMLCanvasElement, frame: WebTerminalFrame): GridSize {
  return {
    cols: Math.max(1, Math.floor(canvas.clientWidth / frame.cellWidth)),
    rows: Math.max(1, Math.floor(canvas.clientHeight / frame.cellHeight)),
  };
}

function encodeKey(event: React.KeyboardEvent): string | null {
  if (event.metaKey) {
    return null;
  }
  if (event.ctrlKey && event.key.length === 1) {
    return String.fromCharCode(event.key.toUpperCase().charCodeAt(0) - 64);
  }
  switch (event.key) {
    case "Enter":
      return "\r";
    case "Backspace":
      return "\x7f";
    case "Tab":
      return "\t";
    case "Escape":
      return "\x1b";
    case "ArrowUp":
      return "\x1b[A";
    case "ArrowDown":
      return "\x1b[B";
    case "ArrowRight":
      return "\x1b[C";
    case "ArrowLeft":
      return "\x1b[D";
    default:
      return event.key.length === 1 ? event.key : null;
  }
}

createRoot(required(document.getElementById("root"), "find root element")).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

function required<T>(value: T | null, action: string): T {
  if (value == null) {
    throw new Error(`Failed to ${action}`);
  }
  return value;
}