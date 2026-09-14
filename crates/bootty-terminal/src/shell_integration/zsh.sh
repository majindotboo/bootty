[[ -o interactive ]] || return
[[ ${__bootty_shell_pid-} != $$ ]] || return
typeset -g __bootty_shell_pid=$$
typeset -g __bootty_command_active=
__bootty_cwd() {
    local dir=$PWD
    dir=${dir//\%/%25}; dir=${dir// /%20}; dir=${dir//\#/%23}; dir=${dir//\?/%3F}
    dir=${dir//$'\e'/%1B}; dir=${dir//$'\a'/%07}; dir=${dir//$'\n'/%0A}; dir=${dir//$'\r'/%0D}; dir=${dir//$'\t'/%09}
    builtin printf '\e]7;file://localhost%s\a' "$dir"
}
__bootty_finish() {
    local code=$?
    if [[ -n $__bootty_command_active ]]; then
        builtin printf '\e]133;D;%s\a' "$code"
        __bootty_command_active=
    fi
    return "$code"
}
__bootty_prompt() {
    __bootty_cwd
    builtin printf '\e]133;A\a'
    local editable=1
    [[ $(bindkey -lL main) == *viins* ]] && editable=0
    if [[ $PROMPT == ${__bootty_wrapped_prompt-} ]]; then PROMPT=$__bootty_original_prompt; fi
    typeset -g __bootty_original_prompt=$PROMPT
    local encoded=$(builtin printf %s "${HISTFILE-}" | command base64 | command tr -d '\r\n')
    PROMPT+="%{"$'\e]133;P;zsh;'"$encoded;$editable"$'\a'"%}"
    typeset -g __bootty_wrapped_prompt=$PROMPT
}
__bootty_preexec() {
    __bootty_command_active=1
    builtin printf '\e]133;C\a'
    if [[ -n ${HISTFILE-} && $HISTSIZE -gt 0 && $1 != [[:space:]]* ]]; then
        builtin printf '\e]133;E;%s\a' "$(builtin printf %s "$1" | command base64 | command tr -d '\r\n')"
    fi
}
autoload -Uz add-zsh-hook
add-zsh-hook preexec __bootty_preexec
add-zsh-hook precmd __bootty_prompt
precmd_functions=(__bootty_finish ${precmd_functions:#__bootty_finish})
