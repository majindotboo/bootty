# Preserve existing shell integrations, especially DEBUG traps.
[[ $- == *i* ]] || return
[[ ${__bootty_shell_pid-} != "$$" ]] || return
[[ -z $(trap -p DEBUG) ]] || return
__bootty_shell_pid=$$
__bootty_command_active=
__bootty_at_prompt=
__bootty_cwd() {
    local dir=$PWD
    dir=${dir//%/%25}; dir=${dir// /%20}; dir=${dir//\#/%23}; dir=${dir//\?/%3F}
    dir=${dir//$'\e'/%1B}; dir=${dir//$'\a'/%07}; dir=${dir//$'\n'/%0A}; dir=${dir//$'\r'/%0D}; dir=${dir//$'\t'/%09}
    builtin printf '\e]7;file://localhost%s\a' "$dir"
}
__bootty_finish() {
    local code=$?
    __bootty_at_prompt=
    if [[ -n $__bootty_command_active ]]; then
        builtin printf '\e]133;D;%s\a' "$code"
        __bootty_command_active=
    fi
    return "$code"
}
__bootty_prompt() {
    __bootty_cwd
    builtin printf '\e]133;A\a'
    local editable=1 history_path=
    [[ -o vi ]] && editable=0
    [[ -o history ]] && history_path=${HISTFILE-}
    if [[ ${PS1-} == "${__bootty_wrapped_prompt-}" ]]; then PS1=$__bootty_original_prompt; fi
    __bootty_original_prompt=${PS1-}
    local encoded=$(builtin printf %s "$history_path" | command base64 | command tr -d '\r\n')
    PS1+="\["$'\e]133;P;bash;'"$encoded;$editable"$'\a'"\]"
    __bootty_wrapped_prompt=$PS1
    __bootty_at_prompt=1
}
__bootty_preexec() {
    [[ -n $__bootty_at_prompt && ${BASH_SUBSHELL:-0} == 0 ]] || return 0
    case ${FUNCNAME[1]-}:$1 in *__bootty_*) return 0;; esac
    __bootty_at_prompt=
    __bootty_command_active=1
    builtin printf '\e]133;C\a'
    if [[ -n ${HISTFILE-} && -o history ]]; then
        local HISTTIMEFORMAT= line
        line=$(builtin history 1)
        if [[ $line =~ ^[[:space:]]*([0-9]+)[[:space:]]+(.*)$ ]] && [[ ${BASH_REMATCH[1]} != "${__bootty_history_number-}" ]]; then
            __bootty_history_number=${BASH_REMATCH[1]}
            builtin printf '\e]133;E;%s\a' "$(builtin printf %s "${BASH_REMATCH[2]}" | command base64 | command tr -d '\r\n')"
        fi
    fi
}
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]] && (( BASH_VERSINFO[0] >= 4 )); then
    PROMPT_COMMAND=(__bootty_finish "${PROMPT_COMMAND[@]}" __bootty_prompt)
else
    PROMPT_COMMAND="__bootty_finish;${PROMPT_COMMAND:+$PROMPT_COMMAND;}__bootty_prompt"
fi
trap '__bootty_preexec "$BASH_COMMAND"' DEBUG
