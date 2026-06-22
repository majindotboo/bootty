--!strict
-- Built-in sidebar `codexbar` module. Override with ~/.config/bootty/sidebar/codexbar.luau.
-- Usage collection belongs to this extension and depends on the installed CodexBar CLI.
local ui = bootty.ui

local CODEX_COLOR = "#74c7ec"
local ORANGE_COLOR = "#fab387"
local RED_COLOR = "#ef4444"
local GREEN_COLOR = "#a6e3a1"
local TRACK_COLOR = "#313244"
local TEXT_COLOR = "#cdd6f4"
local MUTED_COLOR = "#6c7086"

local function parse_window(data, key, fallback_window_secs)
	local window = data[key]
	if not window or not window.usedPercent then
		return nil
	end
	local reset_epoch = ui.parse_iso_epoch(window.resetsAt)
	return {
		used_percent = ui.clamp(window.usedPercent, 0, 100),
		window_secs = if window.windowMinutes and window.windowMinutes > 0
			then window.windowMinutes * 60
			else fallback_window_secs,
		reset_secs = if reset_epoch then reset_epoch - bootty.time() else nil,
	}
end

local function quota_color(window)
	local remaining_pct = math.max(0, math.min(100, 100 - window.used_percent))
	if remaining_pct <= 5 then
		return RED_COLOR
	elseif remaining_pct <= 15 then
		return ORANGE_COLOR
	end
	return CODEX_COLOR
end

local function marker_color(remaining_percent, expected_remaining_percent)
	if remaining_percent >= expected_remaining_percent then
		return GREEN_COLOR
	elseif expected_remaining_percent - remaining_percent < 10 then
		return ORANGE_COLOR
	end
	return ORANGE_COLOR
end

local function expected_remaining_percent(window)
	if not window.reset_secs or window.window_secs <= 0 or window.reset_secs <= 0 then
		return nil
	end
	return math.max(0, math.min(100, window.reset_secs / window.window_secs * 100))
end

local function pace_label(window, remaining_pct, expected_remaining_pct)
	if not expected_remaining_pct then
		return "", nil
	end
	local deficit = math.floor(expected_remaining_pct - remaining_pct + 0.5)
	if deficit > 0 then
		return string.format("%d%% def", deficit), ORANGE_COLOR
	end
	local ahead = math.floor(remaining_pct - expected_remaining_pct + 0.5)
	if ahead > 0 then
		return string.format("+%d%%", ahead), MUTED_COLOR
	end
	return "", nil
end

local function usage_item(label, window)
	local remaining_pct = math.max(0, math.min(100, 100 - window.used_percent))
	local expected_remaining_pct = expected_remaining_percent(window)
	local color = quota_color(window)
	local pace, pace_color = pace_label(window, remaining_pct, expected_remaining_pct)
	local reset = if window.reset_secs and window.reset_secs > 0 then ui.format_duration(window.reset_secs) else ""
	return ui.footer_meter({
		text = "codex " .. label,
		icon = "openai",
		label = string.format("%s %d%% left", label, math.floor(remaining_pct + 0.5)),
		text_color = TEXT_COLOR,
		muted_color = MUTED_COLOR,
		track = TRACK_COLOR,
		color = color,
		fill = remaining_pct / 100,
		pace = pace,
		pace_color = pace_color,
		reset = reset,
		marker = if expected_remaining_pct then expected_remaining_pct / 100 else nil,
		marker_color = if expected_remaining_pct then marker_color(remaining_pct, expected_remaining_pct) else nil,
	})
end

local function codexbar_usage_items()
	local ok, data = pcall(function()
		return bootty.json.decode(bootty.run("codexbar usage --provider codex --format json --json-only"))
	end)
	if not ok or not data then
		return {}
	end
	data = data[1] or data
	data = data.usage or data
	local primary = parse_window(data, "primary", 5 * 3600)
	local secondary = parse_window(data, "secondary", 7 * 24 * 3600)
	local items = {}
	if primary then
		items[#items + 1] = usage_item("5h", primary)
	end
	if secondary then
		items[#items + 1] = usage_item("7d", secondary)
	end
	return items
end

return {
	-- The extension host re-runs this module on this cadence; no Bootty usage cache exists.
	interval = 60,
	render = codexbar_usage_items,
}
