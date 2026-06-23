--!strict
-- Built-in sidebar `codexbar` module. Override with ~/.config/bootty/sidebar/codexbar.luau.
-- Backed by the CodexBar CLI. The built-in default only probes Codex; add rows to PROVIDERS
-- in a user override to surface other configured providers.
local ui = bootty.ui

-- base color is the healthy-meter color; the icon distinguishes the provider. Keep it clear of
-- ORANGE_COLOR/RED_COLOR below so a full quota never reads as a warning.
local PROVIDERS = {
	{ id = "codex", label = "codex", icon = "openai", color = "#74c7ec" },
}

local ORANGE_COLOR = "#fab387"
local RED_COLOR = "#ef4444"
local GREEN_COLOR = "#a6e3a1"
local TRACK_COLOR = "#313244"
local TEXT_COLOR = "#cdd6f4"
local MUTED_COLOR = "#6c7086"
local empty_renders = 0

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

local function quota_color(window, base_color)
	local remaining_pct = math.max(0, math.min(100, 100 - window.used_percent))
	if remaining_pct <= 5 then
		return RED_COLOR
	elseif remaining_pct <= 15 then
		return ORANGE_COLOR
	end
	return base_color
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

local function usage_item(provider, label, window)
	local remaining_pct = math.max(0, math.min(100, 100 - window.used_percent))
	local expected_remaining_pct = expected_remaining_percent(window)
	local color = quota_color(window, provider.color)
	local pace, pace_color = pace_label(window, remaining_pct, expected_remaining_pct)
	local reset = if window.reset_secs and window.reset_secs > 0 then ui.format_duration(window.reset_secs) else ""
	return ui.footer_meter({
		text = provider.label .. " " .. label,
		icon = provider.icon,
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

local function status_item(label)
	return {
		kind = "footer",
		text = "codexbar " .. label,
		fg = MUTED_COLOR,
	}
end

local function short_status(value)
	local first = string.match(value, "^[^\n]*") or value
	if #first > 42 then
		return string.sub(first, 1, 39) .. "..."
	end
	return first
end

local function error_message(data)
	local err = data.error
	if not err and data[1] then
		err = data[1].error
	end
	if err and err.message then
		return short_status(err.message)
	end
	return nil
end


local function usage_command(provider)
	local command = "codexbar usage --provider " .. provider.id .. " --format json --json-only 2>&1"
	return "out=$(" .. command .. "); status=$?; if [ -n \"$out\" ]; then printf '%s' \"$out\"; else printf 'codexbar exited %s with empty output' \"$status\"; fi"
end

local function provider_items(provider)
	local raw = bootty.run(usage_command(provider))
	if raw == "" then
		return {}, nil
	end
	local ok, data = pcall(function()
		return bootty.json.decode(raw)
	end)
	if not ok or not data then
		return {}, short_status(raw)
	end
	local err = error_message(data)
	if err then
		return {}, err
	end
	data = data[1] or data
	data = data.usage or data
	local primary = parse_window(data, "primary", 5 * 3600)
	local secondary = parse_window(data, "secondary", 7 * 24 * 3600)
	local items = {}
	if primary then
		items[#items + 1] = usage_item(provider, "5h", primary)
	end
	if secondary then
		items[#items + 1] = usage_item(provider, "7d", secondary)
	end
	return items, nil
end

local function usage_items()
	local items = {}
	local status = nil
	for _, provider in ipairs(PROVIDERS) do
		local provider_items_result, provider_status = provider_items(provider)
		status = status or provider_status
		for _, item in ipairs(provider_items_result) do
			items[#items + 1] = item
		end
	end
	if #items > 0 then
		empty_renders = 0
		return items
	end
	empty_renders += 1
	return { status_item(if status then status else "loading") }
end

return {
	-- The extension host re-runs this module on this cadence; no Bootty usage cache exists.
	-- Each provider is one CodexBar fetch, so keep PROVIDERS short to bound per-tick work.
	interval = 60,
	render = usage_items,
}
