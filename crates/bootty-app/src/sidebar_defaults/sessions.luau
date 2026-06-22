--!strict
-- Built-in sidebar `sessions` module. Override with ~/.config/bootty/sidebar/sessions.luau.
-- Bootty exposes only owned session identity/state; all enrichment is rendered here as primitives.
local ui = bootty.ui
local quote = ui.shell_quote

local MUTED_COLOR = "#6c7086"
local SUCCESS_COLOR = "#a6e3a1"
local WARNING_COLOR = "#f9e2af"
local DESTRUCTIVE_COLOR = "#f38ba8"
local TRACK_COLOR = "#313244"
local CODEX_COLOR = "#74c7ec"
local CLAUDE_COLOR = "#fab387"
local OPENCODE_COLOR = "#9a8fbf"

local PROCESS_STYLE = {
	git = { icon = "git-branch", color = DESTRUCTIVE_COLOR },
	nvim = { icon = "square-pen", color = CODEX_COLOR },
	vim = { icon = "square-pen", color = CODEX_COLOR },
	cargo = { icon = "package", color = WARNING_COLOR },
	rustc = { icon = "package", color = WARNING_COLOR },
	["rust-analyzer"] = { icon = "package", color = WARNING_COLOR },
	node = { icon = "terminal", color = SUCCESS_COLOR },
	bun = { icon = "terminal", color = SUCCESS_COLOR },
	deno = { icon = "terminal", color = SUCCESS_COLOR },
}

local AGENT_STYLE = {
	codex = { icon = "openai", color = CODEX_COLOR },
	claude = { icon = "claude", color = CLAUDE_COLOR },
	opencode = { icon = "bot", color = OPENCODE_COLOR },
}

local FACTS_CACHE = {}
local first_render = true
local refresh_index = 1

local function tmux_session(session)
	return string.sub(session.id, 1, 1) == "$"
end

local function tmux_active_pane(session)
	if not tmux_session(session) then
		return nil, nil, nil
	end
	local out = bootty.run(
		"tmux list-panes -t "
			.. quote(session.id)
			.. " -F '#{pane_active}\t#{pane_id}\t#{pane_current_command}\t#{pane_pid}' 2>/dev/null"
	)
	for line in string.gmatch(out, "[^\n]+") do
		local active, pane_id, command, pane_pid = string.match(line, "^([^\t]*)\t([^\t]*)\t([^\t]*)\t(.+)$")
		if active == "1" and command ~= "" then
			return command, pane_id, pane_pid
		end
	end
	return nil, nil, nil
end

local function tmux_option(session, option)
	if not tmux_session(session) then
		return nil
	end
	local value = bootty.run("tmux show-options -t " .. quote(session.id) .. " -qv " .. quote(option) .. " 2>/dev/null")
	return if value == "" then nil else value
end

local function captured_pane(pane_id)
	return if pane_id and pane_id ~= ""
		then bootty.run("tmux capture-pane -t " .. quote(pane_id) .. " -p -S -30 2>/dev/null")
		else ""
end

local function git_branch(cwd)
	if not cwd or cwd == "" then
		return nil
	end
	local branch = bootty.run("git -C " .. quote(cwd) .. " rev-parse --abbrev-ref HEAD 2>/dev/null")
	return if branch == "" or branch == "HEAD" then nil else branch
end

local function git_diff(cwd)
	if not cwd or cwd == "" then
		return nil, nil
	end
	local added = 0
	local removed = 0
	for line in string.gmatch(bootty.run("git -C " .. quote(cwd) .. " diff HEAD --numstat 2>/dev/null"), "[^\n]+") do
		local add, remove = string.match(line, "^(%d+)\t(%d+)\t")
		added += tonumber(add) or 0
		removed += tonumber(remove) or 0
	end
	if added == 0 and removed == 0 then
		return nil, nil
	end
	return added, removed
end

local function command_basename(command)
	if not command or command == "" then
		return nil
	end
	local name = string.match(command, "([^/]+)$") or command
	name = string.gsub(name, "^%-+", "")
	return string.lower(name)
end

local function command_agent_name(command)
	local lower = command_basename(command)
	if lower == "codex.app" then
		return "codex"
	elseif lower == "claude.app" then
		return "claude"
	end
	return if lower == "claude" or lower == "codex" or lower == "opencode" then lower else nil
end

local INTERPRETER_WRAPPERS = {
	bun = true,
	deno = true,
	node = true,
	python = true,
	python3 = true,
	ruby = true,
}

local function args_tokens(args)
	local tokens = {}
	for token in string.gmatch(args or "", "%S+") do
		tokens[#tokens + 1] = token
	end
	return tokens
end

local function wrapper_script_agent(command, args)
	local wrapper = command_basename(command)
	if not INTERPRETER_WRAPPERS[wrapper] then
		return nil
	end
	local tokens = args_tokens(args)
	for index = 2, #tokens do
		local token = tokens[index]
		if not ui.starts_with(token, "-") then
			return command_agent_name(token)
		end
	end
	return nil
end

local function process_snapshot()
	local children = {}
	for line in string.gmatch(bootty.run("ps -axo pid=,ppid=,comm=,args= 2>/dev/null"), "[^\n]+") do
		local pid, ppid, command, args = string.match(line, "^%s*(%d+)%s+(%d+)%s+(%S+)%s*(.*)$")
		if pid and ppid and command then
			if not children[ppid] then
				children[ppid] = {}
			end
			local siblings = children[ppid]
			siblings[#siblings + 1] = {
				pid = pid,
				command = command_basename(command),
				agent = command_agent_name(command) or wrapper_script_agent(command, args),
			}
		end
	end
	return children
end

local function agent_command(command)
	return command_agent_name(command)
end

local function descendant_agent(children, root_pid)
	if not root_pid or root_pid == "" then
		return nil
	end
	local queue = { root_pid }
	local seen = {}
	local head = 1
	while head <= #queue and head < 256 do
		local pid = queue[head]
		head += 1
		if not seen[pid] then
			seen[pid] = true
			for _, child in ipairs(children[pid] or {}) do
				local agent = child.agent or agent_command(child.command)
				if agent then
					return agent
				end
				queue[#queue + 1] = child.pid
			end
		end
	end
	return nil
end

local function parse_star_activity(text)
	local marks = { "·", "•", "✻", "⋆", "✦", "✧", "✶", "✢", "✽", "✳" }
	local lines = {}
	for line in string.gmatch(text, "[^\n]+") do
		lines[#lines + 1] = ui.trim(line)
	end
	for index = #lines, 1, -1 do
		for _, mark in ipairs(marks) do
			local prefix = mark .. " "
			if ui.starts_with(lines[index], prefix) then
				local rest = string.sub(lines[index], #prefix + 1)
				if string.find(rest, "…", 1, true) then
					return string.gsub(string.match(rest, "^(%S+)") or "working", "…$", "") .. "…"
				end
			end
		end
	end
	return nil
end

local function parse_codex_activity(text)
	for line in string.gmatch(text, "[^\n]+") do
		local trimmed = ui.trim(line)
		if ui.starts_with(trimmed, "• ") then
			local verb = string.match(trimmed, "^•%s*(%S+)")
			if verb == "Working" or verb == "Thinking" or verb == "Running" then
				return verb .. "…"
			end
		end
	end
	return nil
end

local function parse_opencode_activity(text)
	local lines = {}
	for line in string.gmatch(text, "[^\n]+") do
		lines[#lines + 1] = line
	end
	local tail = {}
	for index = math.max(1, #lines - 9), #lines do
		tail[#tail + 1] = lines[index]
	end
	local bottom = table.concat(tail, " ")
	return if string.find(bottom, "esc", 1, true) and string.find(bottom, "interrupt", 1, true)
		then "Working…"
		else nil
end

local function agent_status(agent, text)
	local activity = nil
	if agent == "claude" then
		activity = parse_star_activity(text)
	elseif agent == "codex" then
		activity = parse_codex_activity(text)
	else
		activity = parse_opencode_activity(text)
	end
	if activity then
		return activity
	end
	local lower = string.lower(text or "")
	local asking = false
	if agent == "claude" then
		asking = string.find(lower, "enter to select", 1, true) ~= nil
	elseif agent == "codex" then
		asking = string.find(lower, "to submit answer", 1, true) ~= nil
	else
		asking = string.find(lower, "select", 1, true) ~= nil
			and string.find(lower, "submit", 1, true) ~= nil
			and string.find(lower, "dismiss", 1, true) ~= nil
	end
	if asking then
		return "asking"
	end
	return "active"
end

local function animated(detail)
	if not string.find(detail, "…", 1, true) and detail ~= "Working" then
		return detail
	end
	return string.gsub(detail, "…", string.rep(".", math.floor(bootty.time() * 5) % 4 + 1))
end

local function minimal_facts()
	return {
		process_style = { icon = "terminal", color = MUTED_COLOR },
	}
end

local function refreshed_session_facts(session)
	local process, pane_id, pane_pid = tmux_active_pane(session)
	local progress_text = tmux_option(session, "@sidebar_progress")
	local agent_name = agent_command(process)
	if not agent_name and pane_pid then
		agent_name = descendant_agent(process_snapshot(), pane_pid)
	end
	local agent_detail = if agent_name then agent_status(agent_name, captured_pane(pane_id)) else nil
	local diff_added, diff_removed = git_diff(session.cwd)
	return {
		process = process,
		display_process = agent_name or process,
		process_style = PROCESS_STYLE[command_basename(process)] or { icon = "terminal", color = MUTED_COLOR },
		branch = git_branch(session.cwd),
		diff_added = diff_added,
		diff_removed = diff_removed,
		status = tmux_option(session, "@sidebar_status"),
		progress = if progress_text then ui.clamp(tonumber(progress_text) or 0, 0, 100) else nil,
		agent_name = agent_name,
		agent_detail = agent_detail,
		agent_style = AGENT_STYLE[agent_name],
	}
end

local function cached_session_facts(session)
	return FACTS_CACHE[session.id] or minimal_facts()
end

local function prune_fact_cache(sessions)
	local live = {}
	for _, session in ipairs(sessions) do
		live[session.id] = true
	end
	for id, _ in pairs(FACTS_CACHE) do
		if not live[id] then
			FACTS_CACHE[id] = nil
		end
	end
end

local function next_refresh_session(sessions)
	if #sessions == 0 then
		return nil
	end
	for _, session in ipairs(sessions) do
		if session.selected and not FACTS_CACHE[session.id] then
			return session
		end
	end
	if refresh_index > #sessions then
		refresh_index = 1
	end
	local session = sessions[refresh_index]
	refresh_index = refresh_index % #sessions + 1
	return session
end

local function refresh_one_session(sessions)
	local session = next_refresh_session(sessions)
	if not session then
		return
	end
	FACTS_CACHE[session.id] = refreshed_session_facts(session)
end

local function session_right(_, facts)
	if facts.diff_added and facts.diff_removed then
		return {
			ui.text(
				"+" .. tostring(facts.diff_added),
				{ x = 1, x_px = -46, color = SUCCESS_COLOR, align = "right_center", size = 12 }
			),
			ui.text(
				"-" .. tostring(facts.diff_removed),
				{ x = 1, x_px = -12, color = DESTRUCTIVE_COLOR, align = "right_center", size = 12 }
			),
		}
	elseif facts.display_process and facts.display_process ~= "" then
		return {
			ui.text(
				facts.display_process,
				{ x = 1, x_px = -12, color = MUTED_COLOR, align = "right_center", size = 12 }
			),
		}
	end
	return {}
end

local function session_details(_, facts)
	return {
		{
			key = "process",
			icon = facts.process_style.icon,
			label = if facts.agent_name then nil else facts.process,
			color = facts.process_style.color,
		},
		{
			key = "agent",
			icon = if facts.agent_style then facts.agent_style.icon else nil,
			label = facts.agent_name,
			color = if facts.agent_style then facts.agent_style.color else nil,
			right = if facts.agent_detail then animated(facts.agent_detail) else nil,
			right_color = MUTED_COLOR,
		},
		{ key = "branch", icon = "git-branch", label = facts.branch, color = MUTED_COLOR },
		{ key = "status", icon = "info", label = facts.status, color = MUTED_COLOR },
	}
end

local function session_progress(ctx, facts)
	if not facts.progress then
		return nil
	end
	return {
		key = "progress",
		value = facts.progress,
		track = TRACK_COLOR,
		fill = if facts.progress >= 100 then SUCCESS_COLOR else ctx.session.dim_color,
		label_color = ctx.session.dim_color,
	}
end

return {
	interval = 0.5,
	render = function()
		local sessions = bootty.sessions()
		prune_fact_cache(sessions)
		if first_render then
			first_render = false
		else
			refresh_one_session(sessions)
		end
		return ui.session_items({
			sessions = sessions,
			facts = cached_session_facts,
			right = session_right,
			details = session_details,
			progress = session_progress,
		})
	end,
}
