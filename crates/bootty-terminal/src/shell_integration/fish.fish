status is-interactive; or return
if set -q __bootty_shell_pid; and test "$__bootty_shell_pid" = "$fish_pid"
    return
end
set -g __bootty_shell_pid $fish_pid
status test-feature mark-prompt 2>/dev/null; and set -g __bootty_native_markers 1
function __bootty_preexec --on-event fish_preexec
    set -g __bootty_command_active 1
    set -q __bootty_native_markers; or printf '\e]133;C\a'
    if not set -q fish_private_mode; and begin; not set -q fish_history; or test -n "$fish_history"; end
        printf '\e]133;E;%s\a' (printf %s "$argv[1]" | command base64 | string join '')
    end
end
function __bootty_postexec --on-event fish_postexec
    set -l code $status
    if set -q __bootty_command_active
        set -q __bootty_native_markers; or printf '\e]133;D;%s\a' $code
        set -e __bootty_command_active
    end
end
function __bootty_prompt --on-event fish_prompt
    set -l dir (string escape --style=url -- $PWD | string replace -a '%2F' '/')
    printf '\e]7;file://localhost%s\a' "$dir"
    set -q __bootty_native_markers; or printf '\e]133;A\a'
end

# Wrap only rendering; fish retains its key bindings, completion and input buffer.
functions -c fish_prompt __bootty_original_fish_prompt
function fish_prompt
    __bootty_original_fish_prompt
    set -l code $status
    set -l editable 1
    if test "$fish_key_bindings" = fish_vi_key_bindings
        set editable 0
    end
    set -l history_path
    if not set -q fish_private_mode
        set -l data "$HOME/.local/share"
        set -q XDG_DATA_HOME; and set data "$XDG_DATA_HOME"
        set -l session fish
        set -q fish_history; and set session "$fish_history"
        if test -n "$session"
            set history_path "$data/fish/"$session"_history"
        end
    end
    printf '\e]133;P;fish;%s;%s\a' (printf %s "$history_path" | command base64 | string join '') "$editable"
    return $code
end
