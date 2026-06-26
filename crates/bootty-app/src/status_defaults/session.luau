--!strict
-- Built-in `session` module: the active session name tinted with the current theme accent.
-- Override by creating ~/.config/bootty/status/session.luau.

return {
	interval = 0.5,
	render = function()
		local name = bootty.session()
		if not name then
			return {}
		end
		return { icon = "folder", text = name, fg = bootty.theme.accent }
	end,
}
