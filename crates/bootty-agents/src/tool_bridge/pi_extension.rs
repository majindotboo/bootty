//! Pi's public extension APIs attach the same bounded private protocol on both API generations.
use super::{ToolBridge, ToolProtocol};
use serde::Serialize;

pub(super) fn tool_name(server: &str, tool: &str) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut name = format!("mcp__{server}__{tool}");
    if name.len() > 64 {
        let digest = Sha256::digest(format!("{server}\0{tool}"));
        name.truncate(55);
        name.push('_');
        for byte in digest.iter().take(4) {
            _ = write!(name, "{byte:02x}");
        }
    }
    name
}

#[derive(Serialize)]
struct PermissionTool<'a> {
    label: &'a str,
    read_only: bool,
}

impl ToolBridge {
    // Only this launch's host-issued catalog can classify a tool as a permitted read.
    pub(crate) fn pi_permission_tools(&self) -> Result<String, String> {
        let catalog = ToolProtocol::new(self.lease().clone()).catalog();
        let tools = catalog
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .ok_or("Invalid host tool catalog")?;
        let mut permissions = std::collections::BTreeMap::new();
        for tool in tools {
            let label = tool
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("Missing host tool name")?;
            permissions.insert(
                tool_name(self.server_name(), label),
                PermissionTool {
                    label,
                    read_only: tool
                        .get("annotations")
                        .and_then(|value| value.get("readOnlyHint"))
                        .and_then(serde_json::Value::as_bool)
                        == Some(true),
                },
            );
        }
        serde_json::to_string(&permissions).map_err(|error| error.to_string())
    }
}

pub(super) const TOOLS: &str = r"
  if (typeof pi.registerMcpServer === 'function') {
    pi.registerMcpServer(server, config);
  } else {
    const { spawn } = await import('node:child_process');
    const { createHash } = await import('node:crypto');
    const request = (method: string, params: unknown, signal?: AbortSignal): Promise<unknown> => new Promise((resolve, reject) => {
      const input = JSON.stringify({jsonrpc: '2.0', id: 1, method, params}) + '\n';
      if (Buffer.byteLength(input) >= 1024 * 1024) { reject(new Error('Bootty tool request exceeds 1 MiB')); return; }
      const child = spawn(config.command, config.args, {stdio: ['pipe', 'pipe', 'ignore'], signal, timeout: 10000, killSignal: 'SIGKILL'});
      const chunks: Buffer[] = []; let size = 0;
      child.once('error', reject);
      child.stdout.on('data', (chunk: Buffer) => {
        size += chunk.length;
        if (size >= 12 * 1024 * 1024) { child.kill('SIGKILL'); reject(new Error('Bootty tool response exceeds 12 MiB')); return; }
        chunks.push(chunk);
      });
      child.once('close', (code) => {
        if (code !== 0) { reject(new Error('Bootty tool endpoint closed')); return; }
        try {
          const response = JSON.parse(Buffer.concat(chunks).toString('utf8'));
          if (response.error) throw new Error(response.error.message);
          resolve(response.result);
        } catch (error) { reject(error); }
      });
      child.stdin.once('error', reject);
      child.stdin.end(input);
    });
    type ToolResult = Awaited<ReturnType<ToolDefinition['execute']>>;
    const catalog = await request('tools/list', {}) as {tools: {name: string, description: string, inputSchema: ToolDefinition['parameters']}[]};
    for (const tool of catalog.tools) {
      let name = 'mcp__' + server + '__' + tool.name;
      if (name.length > 64) name = name.slice(0, 55) + '_' + createHash('sha256').update(server + '\0' + tool.name).digest('hex').slice(0, 8);
      pi.registerTool({name, label: tool.name, description: tool.description, promptSnippet: tool.description, parameters: tool.inputSchema,
        async execute(_id, args, signal) {
          const result = await request('tools/call', {name: tool.name, arguments: args}, signal) as {content: ToolResult['content'], structuredContent?: unknown, isError?: boolean};
          if (result.isError) throw new Error(result.content.filter(item => item.type === 'text').map(item => item.text).join('\n'));
          return {content: result.content, details: result.structuredContent ?? {}};
        }
      });
    }
  }
";
