# Psychic shell integration for zsh
# Add this to your ~/.zshrc:
#   eval "$(psychic zsh)"
# or run `psychic init`, which does that for you.

# Tells psychic the integration is loaded, so the TUI does not suggest setting it up.
export PSYCHIC_SHELL=zsh

# =============================================================================
# Hook to track directory changes
# =============================================================================

# Hook function that runs after every directory change
function __psychic_hook() {
    \command psychic track-visit "$(\builtin pwd)" 2>/dev/null &!
}

# Initialize hook (add to chpwd_functions array)
\builtin typeset -ga chpwd_functions
# Remove any existing instances of our hook (avoid duplicates)
# shellcheck disable=SC2034,SC2296
chpwd_functions=("${(@)chpwd_functions:#__psychic_hook}")
# Add our hook
chpwd_functions+=(__psychic_hook)

# =============================================================================
# Navigation commands
# =============================================================================

# Jump to a directory using psychic
# Ctrl-J in psychic will print the directory and exit
p() {
    \builtin local result
    result="$(\command psychic --on-cwd-visit=print-to-stdout)" && \builtin cd -- "${result}"
}

# Jump to a directory using psychic (directories only)
# Enter on a directory will print it and exit (no navigation mode)
# `pd xy`: cd to xy if it is a directory, else to the best match for xy without
#   opening the TUI (`psychic jump`); if nothing matches, open it with xy typed in
# `pd -i xy`: open the TUI with xy typed in
pd() {
    \builtin local result
    if [[ "$1" == -i ]]; then
        \builtin shift
        result="$(\command psychic --filter=dirs --on-dir-click=print-to-stdout --query="$*")"
    elif (( $# == 0 )); then
        result="$(\command psychic --filter=dirs --on-dir-click=print-to-stdout)"
    elif [[ $# -eq 1 && -d "$1" ]]; then
        result="$1"
    else
        result="$(\command psychic jump -- "$*")" ||
            result="$(\command psychic --filter=dirs --on-dir-click=print-to-stdout --query="$*")"
    fi && \builtin cd -- "${result}"
}

# Jump to a directory using psychic (current directory files only)
# Navigate within current dir, Ctrl-J to cd there
pc() {
    \builtin local result
    result="$(\command psychic --filter=cwd --on-cwd-visit=print-to-stdout)" && \builtin cd -- "${result}"
}

# =============================================================================
# Ctrl-T: insert a path picked in psychic at the cursor
# =============================================================================

# Enter on a file or directory prints it instead of opening it. A pick under
# the current directory is inserted relative to it, anything else absolute,
# quoted for the shell.
__psychic_insert_path() {
    \builtin local result
    result="$(\command psychic --on-file-click=print-to-stdout --on-dir-click=print-to-stdout)"
    if [[ -n "${result}" ]]; then
        [[ "${result}" == "${PWD}/"* ]] && result="${result#"${PWD}"/}"
        LBUFFER+="${(q)result} "
    fi
    \builtin zle reset-prompt
}
\builtin zle -N __psychic_insert_path

# Bound to Ctrl-T only where Ctrl-T still does what zsh does by default, so an
# fzf (or anything else) binding is never taken over. Set PSYCHIC_NO_CTRL_T
# before loading this to skip it; bind the widget yourself with, e.g.,
#   bindkey '^X^F' __psychic_insert_path
if [[ -z "${PSYCHIC_NO_CTRL_T-}" ]]; then
    for __psychic_keymap in emacs viins; do
        case "$(\builtin bindkey -M "${__psychic_keymap}" '^T' 2>/dev/null)" in
            *transpose-chars|*undefined-key|*self-insert|'')
                \builtin bindkey -M "${__psychic_keymap}" '^T' __psychic_insert_path ;;
        esac
    done
    \builtin unset __psychic_keymap
fi

# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
\builtin zmodload zsh/datetime 2>/dev/null
\builtin typeset -g __psychic_session="$$-${RANDOM}${RANDOM}"
\builtin typeset -g __psychic_cmd='' __psychic_cmd_pwd='' __psychic_cmd_start=''

function __psychic_history_enabled() {
    case "${PSYCHIC_HISTORY-}" in
        0) \builtin return 1 ;;
        1) \builtin return 0 ;;
    esac
    [[ -e "${PSYCHIC_DATA_DIR:-${HOME}/.local/share/psychic}/history-enabled" ]]
}

# Before a command runs: remember it, where it was typed and when.
function __psychic_preexec() {
    __psychic_cmd=''
    __psychic_history_enabled || \builtin return 0
    [[ -n "$1" ]] || \builtin return 0
    [[ -o histignorespace && "$1" == ' '* ]] && \builtin return 0
    [[ -n "${HISTORY_IGNORE-}" && "$1" == ${~HISTORY_IGNORE} ]] && \builtin return 0
    __psychic_cmd="$1"
    __psychic_cmd_pwd="${PWD}"
    __psychic_cmd_start="${EPOCHREALTIME-}"
}

# Before the next prompt: one background call with how it went.
function __psychic_precmd() {
    \builtin local __psychic_status=$?
    [[ -n "${__psychic_cmd}" ]] || \builtin return 0
    \builtin local __psychic_ms=''
    [[ -n "${__psychic_cmd_start}" ]] && (( __psychic_ms = (EPOCHREALTIME - __psychic_cmd_start) * 1000 ))
    \command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        track-command --cwd "${__psychic_cmd_pwd}" --exit-code "${__psychic_status}" \
        ${__psychic_cmd_start:+--started-at} ${__psychic_cmd_start:+"${__psychic_cmd_start%.*}"} \
        ${__psychic_ms:+--duration-ms} ${__psychic_ms:+"${__psychic_ms%.*}"} \
        --session "${__psychic_session}" -- "${__psychic_cmd}" >/dev/null 2>&1 &!
    __psychic_cmd=''
}

\builtin autoload -Uz add-zsh-hook
add-zsh-hook preexec __psychic_preexec
add-zsh-hook precmd __psychic_precmd
