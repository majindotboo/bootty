// Pi's public blocking hook implements the supervised/edit permission modes.
export default function boottyPermissions(pi) {
  const mode = __BOOTTY_PERMISSION_MODE__;
  const capturedTools = __BOOTTY_PERMISSION_TOOLS__;
  const reads = new Set(["read", "grep", "find", "ls"]);
  const edits = new Set(["edit", "write"]);
  pi.on("tool_call", async (event, context) => {
    const captured = Object.hasOwn(capturedTools, event.toolName) ? capturedTools[event.toolName] : undefined;
    if (mode === "full-access" || reads.has(event.toolName) || captured?.read_only === true) return;
    if (mode === "auto-accept-edits" && edits.has(event.toolName)) return;
    const label = captured?.label ?? event.toolName;
    const approved = await context.ui.confirm(
      `Allow ${label}?`,
      JSON.stringify(event.input ?? {}).slice(0, 4000),
    );
    if (!approved) return { block: true, reason: `${label} was declined in Bootty.` };
  });
}
