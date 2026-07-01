--!strict
-- Built-in `windows` module: the active session's windows as tmux-style mux
-- cells. Override by creating ~/.config/bootty/status/windows.luau.

return {
	interval = 0.2,
	render = function()
		-- Tint the active window tab with the focused session's color so the status bar matches the
		-- sidebar; falls back to the theme accent when no session color is available.
		local accent = bootty.session_color() or bootty.theme.accent
		local windows = bootty.windows()
		local items = {}
		local radius = 6
		local wedge = 8

		local function left_radius()
			return { nw = radius, sw = radius }
		end

		local function right_radius(enabled)
			return { ne = if enabled then radius else 0, se = if enabled then radius else 0 }
		end

		local function rect(fill, corners)
			return { type = "rect", fill = fill, radius = corners }
		end

		local function underlay(fill)
			return { type = "rect", fill = fill, w = 0, w_px = radius }
		end

		local function previous_tab_fill(index)
			if index <= 1 then
				return nil
			end
			return if windows[index - 1].active then bootty.theme.surface else bootty.theme.base
		end


		local function left_chevron(fill)
			return {
				type = "polygon",
				fill = fill,
				points = {
					{ x = 0, dx = -1, y = 0 },
					{ x = 0, dx = wedge, y = 0.5 },
					{ x = 0, dx = -1, y = 1 },
				},
			}
		end

		local function push(window, item)
			item.action = "activate-window:" .. window.id
			-- Both cells of a window share the window id, so they form one draggable tab.
			item.reorder_anchor = window.id
			items[#items + 1] = item
		end

		for index, window in ipairs(windows) do
			local last = index == #windows
			local active = window.active
			local index_fill = if active then accent else bootty.theme.base
			local name_fill = if active then bootty.theme.surface else bootty.theme.base
			local index_fg = if active then bootty.theme.base else bootty.theme.subtext
			local name_fg = if active then bootty.theme.text else bootty.theme.subtext

			local index_primitives = {}
			local previous_fill = previous_tab_fill(index)
			if previous_fill then
				index_primitives[#index_primitives + 1] = underlay(previous_fill)
			end
			index_primitives[#index_primitives + 1] = rect(index_fill, left_radius())

			local name_primitives = { rect(name_fill, right_radius(last)) }
			if active then
				name_primitives[#name_primitives + 1] = left_chevron(accent)
			end

			push(window, {
				text = tostring(window.index),
				fg = index_fg,
				primitives = index_primitives,
				join = false,
				gap = false,
			})
			push(window, {
				text = window.name,
				fg = name_fg,
				primitives = name_primitives,
				pad_left = wedge,
				gap = false,
			})
		end
		return items
	end,
	-- Dragging a window tab reorders it via tmux. `source`/`before` are window ids; `before` nil
	-- means drop at the end. tmux renumbers windows on insert.
	on_reorder = function(source, before)
		local quote = bootty.ui.shell_quote
		local moved = false
		if before and before ~= "" then
			bootty.run("tmux move-window -b -s " .. quote(source) .. " -t " .. quote(before))
			moved = true
		else
			local windows = bootty.windows()
			local last = windows[#windows]
			if last and last.id ~= source then
				bootty.run("tmux move-window -a -s " .. quote(source) .. " -t " .. quote(last.id))
				moved = true
			end
		end
		-- Renumber so indices stay gapless; `-r` respects base-index (0 or 1). It must be a
		-- separate command: `move-window -b -r` ignores the move and only renumbers.
		if moved then
			bootty.run("tmux move-window -r -t " .. quote(source))
		end
	end,
}
