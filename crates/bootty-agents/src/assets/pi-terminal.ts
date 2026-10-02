import { readFileSync } from "node:fs";
import { createConnection } from "node:net";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

type Activity = "idle" | "working" | "waiting" | "finished" | "stopped" | "error";
type Snapshot = {
  token: string;
  sessionId: string;
  sessionFile: string | null;
  status: Activity;
  detail: string | null;
};
type Connection = { socketPath: string; token: string };

// Complete snapshots can replace older snapshots without losing current activity.
const MAX_PENDING_EVENTS = 64;
class Publisher {
  readonly #queue: Snapshot[] = [];
  readonly #connection: Connection;
  #active = false;

  constructor(connection: Connection) {
    this.#connection = connection;
  }

  enqueue(snapshot: Snapshot): void {
    if (this.#queue.length === MAX_PENDING_EVENTS) this.#queue.shift();
    this.#queue.push(snapshot);
    void this.#drain();
  }

  async #drain(): Promise<void> {
    if (this.#active) return;
    this.#active = true;
    try {
      while (this.#queue.length > 0) {
        const snapshot = this.#queue.shift();
        if (snapshot) await this.#send(snapshot);
      }
    } finally {
      this.#active = false;
    }
  }

  #send(snapshot: Snapshot): Promise<void> {
    return new Promise((resolve) => {
      const socket = createConnection(this.#connection.socketPath);
      socket.unref();
      const finish = (): void => { socket.destroy(); resolve(); };
      socket.setTimeout(750, finish);
      socket.once("error", finish);
      socket.once("close", resolve);
      socket.once("connect", () => socket.end(`${JSON.stringify(snapshot)}\n`));
    });
  }
}

export default function observeTerminal(pi: ExtensionAPI): void {
  const parsed: unknown = JSON.parse(readFileSync(new URL("./connection.json", import.meta.url), "utf8"));
  if (typeof parsed !== "object" || parsed === null) return;
  const socketPath: unknown = Reflect.get(parsed, "socketPath");
  const token: unknown = Reflect.get(parsed, "token");
  if (typeof socketPath !== "string" || typeof token !== "string") return;
  const publisher = new Publisher({ socketPath, token });
  let activity: Activity = "idle";
  let settledActivity: Activity = "idle";
  let prompts = 0;
  const publish = (context: ExtensionContext): void => {
    publisher.enqueue({
      token,
      sessionId: context.sessionManager.getSessionId(),
      sessionFile: context.sessionManager.getSessionFile() ?? null,
      status: prompts > 0 ? "waiting" : activity,
      detail: prompts > 0 ? "Pi is waiting for input" : null,
    });
  };
  pi.on("session_start", (_event, context) => {
    prompts = 0;
    activity = context.isIdle() ? "idle" : "working";
    settledActivity = "idle";
    publish(context);
  });
  pi.on("session_info_changed", (_event, context) => publish(context));
  pi.on("agent_start", (_event, context) => {
    activity = "working";
    settledActivity = "idle";
    publish(context);
  });
  pi.on("agent_before_settle", (event) => {
    settledActivity = event.outcome === "error" ? "error" : event.outcome === "aborted" ? "stopped" : "finished";
  });
  // Aborted runs can skip the pre-settlement boundary, but still report a turn outcome.
  pi.on("turn_end", (event) => {
    settledActivity = event.outcome === "error" ? "error" : event.outcome === "aborted" ? "stopped" : "finished";
  });
  pi.on("agent_settled", (_event, context) => {
    activity = settledActivity;
    publish(context);
  });
  pi.on("ui_prompt_start", (_event, context) => {
    prompts += 1;
    publish(context);
  });
  pi.on("ui_prompt_end", (_event, context) => {
    prompts = Math.max(0, prompts - 1);
    publish(context);
  });
  pi.on("session_shutdown", (event, context) => {
    if (event.reason !== "quit") return;
    prompts = 0;
    activity = "stopped";
    publish(context);
  });
}
