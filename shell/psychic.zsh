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

# HIST_NO_STORE: whether zsh keeps the line `$1` out of its history, as
# should_ignore_line in zsh's hist.c decides it on the parsed line (aliases
# expanded, words joined by single spaces): `history`, `r` or `fc -l` (any
# option letters before the first non-letter include an `l`), after an
# optional `builtin`, and followed by a space or nothing (so `history;x` is
# kept, `history|x` is not); a function of that name runs instead unless
# `builtin` was given, and then the line is kept.
function __psychic_history_no_store() {
    \builtin emulate -L zsh
    \builtin local -a words
    words=(${(z)1})
    \builtin local forced='' depth=0
    while (( depth++ < 10 )); do
        if [[ "${words[1]-}" == builtin ]]; then
            forced=1
            \builtin shift words
        elif [[ -z "${forced}" && -n "${words[1]-}" && -n "${aliases[${words[1]}]-}" ]]; then
            words=(${(z)aliases[${words[1]}]} "${(@)words[2,-1]}")
        else
            break
        fi
    done
    [[ "${words[2]-}" == ';' ]] && \builtin return 1
    case "${words[1]-}" in
        history|r) ;;
        fc) [[ "${words[2]-}" == -* && "${${words[2]#-}%%[^[:alpha:]]*}" == *l* ]] || \builtin return 1 ;;
        *) \builtin return 1 ;;
    esac
    [[ -n "${forced}" ]] || ! \builtin functions "${words[1]}" >/dev/null 2>&1
}

# Before a command runs: remember it, where it was typed and when.
function __psychic_preexec() {
    __psychic_cmd=''
    __psychic_history_enabled || \builtin return 0
    [[ -n "$1" ]] || \builtin return 0
    [[ -o histignorespace && "$1" == ' '* ]] && \builtin return 0
    [[ -n "${HISTORY_IGNORE-}" && "$1" == ${~HISTORY_IGNORE} ]] && \builtin return 0
    [[ -o histnostore ]] && __psychic_history_no_store "$1" && \builtin return 0
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

# =============================================================================
# Ctrl-R: search command history in psychic
# =============================================================================

# The picker, searching for $1 first. Prints the pick, or nothing (Escape).
__psychic_history_run() {
    \command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        history --session="${__psychic_session}" --cwd="${PWD}" --query="$1"
}

# Enter on a command replaces the line being edited with it; Escape leaves the
# line alone. What is already typed becomes the search.
__psychic_history_widget() {
    \builtin local result
    result="$(__psychic_history_run "${BUFFER}")"
    if [[ -n "${result}" ]]; then
        BUFFER="${result}"
        CURSOR=${#BUFFER}
    fi
    \builtin zle reset-prompt
}
\builtin zle -N __psychic_history_widget

# Bound to Ctrl-R only where history search is on, in each of the emacs and
# viins keymaps where Ctrl-R is still one of zsh's own (its default, or a
# history search oh-my-zsh and others bind). A plugin that binds Ctrl-R after
# this runs wins: load this last. Set PSYCHIC_NO_CTRL_R before loading this to
# skip it; bind the widget yourself with, e.g.,
#   bindkey '^X^R' __psychic_history_widget
# What became of it goes into PSYCHIC_CTRL_R, for `psychic history --status`:
# bound (both keymaps), partly, taken (neither: something else has Ctrl-R),
# recording-off or opted-out.
if [[ -n "${PSYCHIC_NO_CTRL_R-}" ]]; then
    PSYCHIC_CTRL_R=opted-out
elif ! __psychic_history_enabled; then
    PSYCHIC_CTRL_R=recording-off
else
    __psychic_keymaps=''
    for __psychic_keymap in emacs viins; do
        case "$(\builtin bindkey -M "${__psychic_keymap}" '^R' 2>/dev/null)" in
            *history-incremental-search-backward|*history-incremental-pattern-search-backward|*redisplay|*undefined-key|'')
                \builtin bindkey -M "${__psychic_keymap}" '^R' __psychic_history_widget
                __psychic_keymaps+=' bound' ;;
            *__psychic_history_widget) __psychic_keymaps+=' bound' ;; # an earlier `source`
            *) __psychic_keymaps+=' taken' ;;
        esac
    done
    case "${__psychic_keymaps}" in
        ' bound bound') PSYCHIC_CTRL_R=bound ;;
        *bound*) PSYCHIC_CTRL_R=partly ;;
        *) PSYCHIC_CTRL_R=taken ;;
    esac
    \builtin unset __psychic_keymap __psychic_keymaps
fi
export PSYCHIC_CTRL_R

# `ph [words]`: the same picker, typed instead of bound, searching for the
# words. The pick goes onto the next prompt's line (`print -rz`), ready to
# edit and run; nothing is run.
__psychic_history_typed() {
    \builtin local result
    result="$(__psychic_history_run "$*")" || \builtin return
    [[ -n "${result}" ]] && \builtin print -rz -- "${result}"
    \builtin return 0
}

# Defined unless PSYCHIC_NO_PH is set or `ph` is already the user's (an alias,
# function or command); kept as ours when this script is sourced again.
if [[ -z "${PSYCHIC_NO_PH-}" ]] && { [[ -n "${__psychic_ph_ours-}" ]] || ! \builtin whence ph >/dev/null; }; then
    __psychic_ph_ours=1
    # Search command history; the pick goes on your prompt to edit and run
    function ph {
        __psychic_history_typed "$@"
    }
fi
