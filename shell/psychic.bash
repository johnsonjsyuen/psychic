# Psychic shell integration for bash
# Add this to your ~/.bashrc:
#   eval "$(psychic bash)"
# or run `psychic init bash`, which does that for you.

# Tells psychic the integration is loaded, so the TUI does not suggest setting it up.
export PSYCHIC_SHELL=bash

# =============================================================================
# Hook to track directory changes
# =============================================================================

# bash has no chpwd hook, so this runs before every prompt and acts only when
# the directory has changed since the last one. Starting at the directory the
# shell opened in, so opening a shell is not counted as a visit.
__psychic_last_pwd="${PWD}"
__psychic_hook() {
    # Returned at the end, so PROMPT_COMMAND entries after this one still see
    # the status of the user's command.
    \builtin local __psychic_status=$?
    if [[ "${__psychic_last_pwd}" != "${PWD}" ]]; then
        __psychic_last_pwd="${PWD}"
        # In a subshell, so the shell prints no job number or "Done" line.
        (\command psychic track-visit "${PWD}" >/dev/null 2>&1 &)
    fi
    \builtin return "${__psychic_status}"
}

if [[ ";${PROMPT_COMMAND:-};" != *";__psychic_hook;"* ]]; then
    PROMPT_COMMAND="__psychic_hook${PROMPT_COMMAND:+;${PROMPT_COMMAND}}"
fi

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
# quoted for the shell. Needs bash 4 or newer, for READLINE_LINE; macOS ships
# bash 3.2, where the binding is skipped.
__psychic_insert_path() {
    \builtin local result
    result="$(\command psychic --on-file-click=print-to-stdout --on-dir-click=print-to-stdout)" || return
    [[ -n "${result}" ]] || return
    [[ "${result}" == "${PWD}/"* ]] && result="${result#"${PWD}"/}"
    result="$(\builtin printf '%q ' "${result}")"
    READLINE_LINE="${READLINE_LINE:0:READLINE_POINT}${result}${READLINE_LINE:READLINE_POINT}"
    READLINE_POINT=$((READLINE_POINT + ${#result}))
}

# Bound to Ctrl-T only in an interactive bash 4+, and only where Ctrl-T still
# does what readline does by default, so an fzf (or anything else) binding is
# never taken over. Set PSYCHIC_NO_CTRL_T before loading this to skip it; bind
# the function yourself with, e.g.,
#   bind -x '"\C-x\C-f": __psychic_insert_path'
if [[ $- == *i* && -z "${PSYCHIC_NO_CTRL_T-}" && "${BASH_VERSINFO[0]}" -ge 4 ]]; then
    if ! \builtin bind -X 2>/dev/null | \command grep -q '^"\\C-t"' &&
        \builtin bind -p 2>/dev/null | \command grep -q '^"\\C-t": transpose-chars'; then
        \builtin bind -x '"\C-t": __psychic_insert_path'
    fi
fi

# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
#
# bash has no preexec hook (and a DEBUG trap or PS0 would collide with other
# tools), so this works from PROMPT_COMMAND alone. Two functions share the
# work: __psychic_history_hook runs first, before anything can change `$?` or
# load other shells' lines into the history list, and __psychic_history_sync
# runs last, after every other entry (Starship, direnv, `history -a;
# history -n`) has had its turn with the list. At the next prompt, the newest
# entry is the user's command only if the shell ran one and bash added it
# after the sync: one entry more than the sync counted, or (erasedups replaced
# an older copy) as many, but stamped after the sync. Lines other shells wrote
# to HISTFILE are loaded after the hook and before the sync, so they never
# count. docs/ctrl-r/01-record.md section 11.3.
#
# Kept when the script is sourced again (`source ~/.bashrc`), so the session
# and what the sync saw carry on.
: "${__psychic_session:=$$-${RANDOM}${RANDOM}}"
: "${__psychic_cmd_pwd:=${PWD}}"
: "${__psychic_hist_on=}"      # recording was on at this prompt
: "${__psychic_hist_cmdno=}"   # \# at the last prompt: how many commands the shell had run
: "${__psychic_hist_count=}"   # the history count when the sync ran; empty = nothing to compare with
: "${__psychic_hist_time=}"    # the time the sync ran, Unix seconds
: "${__psychic_hist_trusted=}" # the sync ran last in PROMPT_COMMAND
__psychic_hist_re='^[[:space:]]*([0-9]+)[*]?[[:space:]]+([0-9]+)[[:space:]](.*)$'
# bash 5.1 runs each element of a PROMPT_COMMAND array in turn, so the sync
# gets an element of its own: a `PROMPT_COMMAND="$PROMPT_COMMAND; ..."` later
# in ~/.bashrc then still runs before it. Older bash runs a string only. Also
# from 5.1, $HISTCMD inside PROMPT_COMMAND is the history count (before, it
# is always 1, and the count is the newest entry's number, at one fork).
__psychic_hist_new=''
((BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1))) && __psychic_hist_new=1
# \# in a prompt string counts the commands the shell has run; ${x@P} (bash
# 4.4) expands it without a fork. An empty line, Ctrl-C at the prompt and a
# syntax error leave it alone.
__psychic_hist_cmdno_format=''
((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4))) && __psychic_hist_cmdno_format='\#'

__psychic_history_enabled() {
    case "${PSYCHIC_HISTORY-}" in
        0) \builtin return 1 ;;
        1) \builtin return 0 ;;
    esac
    [[ -e "${PSYCHIC_DATA_DIR:-${HOME}/.local/share/psychic}/history-enabled" ]]
}

# The newest history entry, into __psychic_hist_{num,start,text}; fails when
# the list is empty.
__psychic_history_newest() {
    \builtin local __psychic_entry
    __psychic_entry="$(HISTTIMEFORMAT='%s ' \builtin history 1)"
    [[ "${__psychic_entry}" =~ ${__psychic_hist_re} ]] || \builtin return 1
    __psychic_hist_num="${BASH_REMATCH[1]}"
    __psychic_hist_start="${BASH_REMATCH[2]}"
    __psychic_hist_text="${BASH_REMATCH[3]}"
}

# PROMPT_COMMAND's first command, into __psychic_hist_word, and its last.
# bash-preexec keeps `__bp_precmd_invoke_cmd` first and `__bp_interactive_mode`
# last, putting them back at every prompt; neither touches the history list,
# so they are looked past, rather than fought over.
__psychic_history_ends() {
    \builtin local __psychic_all='' __psychic_i
    for __psychic_i in "${!PROMPT_COMMAND[@]}"; do
        # Only element 0 runs before bash 5.1.
        [[ -n "${__psychic_hist_new}" || "${__psychic_i}" == 0 ]] || continue
        __psychic_all+="${PROMPT_COMMAND[__psychic_i]}"$'\n'
    done
    # Commands are separated by `;` or a newline; strip blanks and separators.
    __psychic_all="${__psychic_all#"${__psychic_all%%[![:space:];]*}"}"
    __psychic_all="${__psychic_all%"${__psychic_all##*[![:space:];]}"}"
    __psychic_hist_first="${__psychic_all%%[[:space:];]*}"
    if [[ "${__psychic_hist_first}" == __bp_precmd_invoke_cmd ]]; then
        __psychic_hist_first="${__psychic_all#__bp_precmd_invoke_cmd}"
        __psychic_hist_first="${__psychic_hist_first#"${__psychic_hist_first%%[![:space:];]*}"}"
        __psychic_hist_first="${__psychic_hist_first%%[[:space:];]*}"
    fi
    __psychic_hist_last="${__psychic_all##*[[:space:];]}"
    if [[ "${__psychic_hist_last}" == __bp_interactive_mode ]]; then
        __psychic_hist_last="${__psychic_all%__bp_interactive_mode}"
        __psychic_hist_last="${__psychic_hist_last%"${__psychic_hist_last##*[![:space:];]}"}"
        __psychic_hist_last="${__psychic_hist_last##*[[:space:];]}"
    fi
}

# Move the hook to the front of PROMPT_COMMAND, from the next prompt on.
__psychic_history_place_hook() {
    \builtin local __psychic_i __psychic_first=''
    \builtin local __psychic_value
    for __psychic_i in "${!PROMPT_COMMAND[@]}"; do
        __psychic_value="${PROMPT_COMMAND[__psychic_i]//__psychic_history_hook;/}"
        __psychic_value="${__psychic_value//__psychic_history_hook/}"
        [[ "${__psychic_value}" == "${PROMPT_COMMAND[__psychic_i]}" ]] ||
            PROMPT_COMMAND[__psychic_i]="${__psychic_value}"
        [[ -n "${__psychic_first}" ]] || __psychic_first="${__psychic_i}"
    done
    [[ -n "${__psychic_hist_new}" ]] || __psychic_first=0
    PROMPT_COMMAND[${__psychic_first:-0}]="__psychic_history_hook;${PROMPT_COMMAND[${__psychic_first:-0}]-}"
}

# Move the sync to the end of PROMPT_COMMAND, from the next prompt on.
__psychic_history_place_sync() {
    if [[ -n "${__psychic_hist_new}" ]]; then
        \builtin local __psychic_i
        for __psychic_i in "${!PROMPT_COMMAND[@]}"; do
            [[ "${PROMPT_COMMAND[__psychic_i]}" == __psychic_history_sync ]] &&
                \builtin unset 'PROMPT_COMMAND[__psychic_i]'
        done
        PROMPT_COMMAND+=(__psychic_history_sync)
    else
        # A newline, not `;`: it ends any command, even one with a trailing
        # `&` or comment.
        PROMPT_COMMAND="${PROMPT_COMMAND//$'\n'__psychic_history_sync/}"$'\n'__psychic_history_sync
    fi
}

# The history count: $HISTCMD from bash 5.1, else the newest entry's number
# (one fork). Into __psychic_hist_now.
__psychic_history_count() {
    if [[ -n "${__psychic_hist_new}" ]]; then
        __psychic_hist_now="${HISTCMD-}"
    elif __psychic_history_newest; then
        __psychic_hist_now="${__psychic_hist_num}"
    else
        __psychic_hist_now=0
    fi
}

# First in PROMPT_COMMAND: if the user ran a command bash added to its
# history, record it.
__psychic_history_hook() {
    \builtin local __psychic_status=$?
    \builtin local __psychic_pwd="${__psychic_cmd_pwd}"
    __psychic_cmd_pwd="${PWD}"
    if ! __psychic_history_enabled; then
        __psychic_hist_on='' __psychic_hist_count=''
        \builtin return "${__psychic_status}"
    fi
    \builtin local __psychic_cmdno=''
    [[ -n "${__psychic_hist_cmdno_format}" ]] && __psychic_cmdno="${__psychic_hist_cmdno_format@P}"
    \builtin local __psychic_count="${__psychic_hist_count}" __psychic_since="${__psychic_hist_time}"
    \builtin local __psychic_trusted="${__psychic_hist_trusted}" __psychic_ran=1 __psychic_first=1
    [[ -n "${__psychic_cmdno}" && "${__psychic_cmdno}" == "${__psychic_hist_cmdno}" ]] && __psychic_ran=''
    __psychic_hist_on=1 __psychic_hist_count='' __psychic_hist_time='' __psychic_hist_cmdno="${__psychic_cmdno}"
    # Where the two functions stand, judged now: for this prompt (the hook)
    # and for the sync of this prompt. Something run before the hook may
    # have loaded other shells' lines since the last sync: nothing is
    # recorded at this prompt, and the hook goes back to the front.
    \builtin local __psychic_hist_first __psychic_hist_last
    __psychic_history_ends
    if [[ "${__psychic_hist_first}" != __psychic_history_hook ]]; then
        __psychic_first=''
        __psychic_history_place_hook
    fi
    if [[ "${__psychic_hist_last}" == __psychic_history_sync ]]; then
        __psychic_hist_trusted=1
    else
        __psychic_hist_trusted=''
        __psychic_history_place_sync
    fi

    # Nothing to compare with (first prompt, recording just turned on), the
    # shell ran nothing (empty line, Ctrl-C, syntax error), or the hook was
    # not first.
    [[ -n "${__psychic_count}" && -n "${__psychic_ran}" && -n "${__psychic_first}" ]] ||
        \builtin return "${__psychic_status}"

    # Did bash add the command to its history? With a trusted count: one more
    # entry is a yes; as many is a no, or erasedups dropped an older copy, and
    # only the entry's time tells; anything else (the command itself ran
    # `history -c`, say) is a no. Without one, only the time tells.
    \builtin local __psychic_appended='' __psychic_hist_num='' __psychic_hist_start='' __psychic_hist_text='' __psychic_hist_now=''
    if [[ -n "${__psychic_trusted}" ]]; then
        __psychic_history_count
        if [[ "${__psychic_hist_now}" == "$((__psychic_count + 1))" ]]; then
            __psychic_appended=1
        elif [[ "${__psychic_hist_now}" != "${__psychic_count}" ]]; then
            \builtin return "${__psychic_status}"
        fi
    fi
    [[ -n "${__psychic_appended}" || -n "${__psychic_since}" ]] || \builtin return "${__psychic_status}"
    [[ -n "${__psychic_hist_num}" ]] || __psychic_history_newest || \builtin return "${__psychic_status}"
    [[ -n "${__psychic_appended}" ]] || ((__psychic_hist_start > __psychic_since)) || \builtin return "${__psychic_status}"

    \builtin local __psychic_start="${__psychic_hist_start}" __psychic_now=''
    \builtin printf -v __psychic_now '%(%s)T' -1 2>/dev/null || __psychic_now=''
    # In a subshell, so the shell prints no job number or "Done" line.
    (\command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        track-command --cwd "${__psychic_pwd}" --exit-code "${__psychic_status}" \
        --started-at "${__psychic_start}" \
        ${__psychic_now:+--duration-ms} ${__psychic_now:+"$(((__psychic_now - __psychic_start) * 1000))"} \
        --session "${__psychic_session}" -- "${__psychic_hist_text}" >/dev/null 2>&1 &)
    \builtin return "${__psychic_status}"
}

# Last in PROMPT_COMMAND: note where the history list stands once every other
# entry is done with it.
__psychic_history_sync() {
    \builtin local __psychic_status=$?
    if [[ -n "${__psychic_hist_on}" ]]; then
        \builtin local __psychic_hist_now='' __psychic_hist_num='' __psychic_hist_start='' __psychic_hist_text=''
        __psychic_history_count
        __psychic_hist_count="${__psychic_hist_now}"
        \builtin printf -v __psychic_hist_time '%(%s)T' -1 2>/dev/null || __psychic_hist_time=''
    fi
    \builtin return "${__psychic_status}"
}

if [[ ";${PROMPT_COMMAND:-};" != *";__psychic_history_hook;"* ]]; then
    PROMPT_COMMAND="__psychic_history_hook${PROMPT_COMMAND:+;${PROMPT_COMMAND}}"
fi
__psychic_history_ends
[[ "${__psychic_hist_last}" == __psychic_history_sync ]] || __psychic_history_place_sync
\builtin unset __psychic_hist_first __psychic_hist_last

# =============================================================================
# Ctrl-R: search command history in psychic
# =============================================================================

# Enter on a command replaces the line being edited with it; Escape leaves the
# line alone. Needs bash 4 or newer, for READLINE_LINE.
__psychic_history_widget() {
    \builtin local result
    result="$(\command "${PSYCHIC_BIN:-psychic}" ${PSYCHIC_DATA_DIR:+--data-dir} ${PSYCHIC_DATA_DIR:+"${PSYCHIC_DATA_DIR}"} \
        history --session="${__psychic_session}" --cwd="${PWD}" --query="${READLINE_LINE}")" || return
    [[ -n "${result}" ]] || return
    READLINE_LINE="${result}"
    READLINE_POINT=${#READLINE_LINE}
}

# Bound only in an interactive bash 4+, where history search is on and Ctrl-R
# still does what readline does by default. Set PSYCHIC_NO_CTRL_R before
# loading this to skip it; bind the function yourself with, e.g.,
#   bind -x '"\C-x\C-r": __psychic_history_widget'
if [[ $- == *i* && -z "${PSYCHIC_NO_CTRL_R-}" && "${BASH_VERSINFO[0]}" -ge 4 ]] && __psychic_history_enabled; then
    if ! \builtin bind -X 2>/dev/null | \command grep -q '^"\\C-r"' &&
        \builtin bind -p 2>/dev/null | \command grep -q '^"\\C-r": reverse-search-history'; then
        \builtin bind -x '"\C-r": __psychic_history_widget'
    fi
fi
