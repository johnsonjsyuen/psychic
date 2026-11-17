# Psychic shell integration for fish
# Add this to your ~/.config/fish/config.fish:
#   psychic fish | source
# or run `psychic init fish`, which does that for you.

# Tells psychic the integration is loaded, so the TUI does not suggest setting it up.
set -gx PSYCHIC_SHELL fish

# =============================================================================
# Hook to track directory changes
# =============================================================================

# Runs whenever $PWD changes. Disowned, so fish prints no "Job has ended" line.
function __psychic_hook --on-variable PWD
    command psychic track-visit "$PWD" >/dev/null 2>&1 &
    disown 2>/dev/null
end

# =============================================================================
# Navigation commands
# =============================================================================

# Escape in psychic exits 0 with no output, and fish's `cd` with no argument
# goes to $HOME, so each command checks for a result before it cds.

# Jump to a directory using psychic
# Ctrl-J in psychic will print the directory and exit
function p
    set -l result (command psychic --on-cwd-visit=print-to-stdout)
    and test -n "$result"
    and builtin cd -- $result
end

# Jump to a directory using psychic (directories only)
# Enter on a directory will print it and exit (no navigation mode)
# `pd xy`: cd to xy if it is a directory, else to the best match for xy without
#   opening the TUI (`psychic jump`); if nothing matches, open it with xy typed in
# `pd -i xy`: open the TUI with xy typed in
function pd
    set -l result
    if test "$argv[1]" = -i
        set result (command psychic --filter=dirs --on-dir-click=print-to-stdout --query="$argv[2..-1]")
    else if test (count $argv) -eq 0
        set result (command psychic --filter=dirs --on-dir-click=print-to-stdout)
    else if test (count $argv) -eq 1; and test -d "$argv[1]"
        set result $argv[1]
    else
        set result (command psychic jump -- "$argv")
        or set result (command psychic --filter=dirs --on-dir-click=print-to-stdout --query="$argv")
    end
    and test -n "$result"
    and builtin cd -- $result
end

# Jump to a directory using psychic (current directory files only)
# Navigate within current dir, Ctrl-J to cd there
function pc
    set -l result (command psychic --filter=cwd --on-cwd-visit=print-to-stdout)
    and test -n "$result"
    and builtin cd -- $result
end

# =============================================================================
# Ctrl-T: insert a path picked in psychic at the cursor
# =============================================================================

# Enter on a file or directory prints it instead of opening it. A pick under
# the current directory is inserted relative to it, anything else absolute,
# quoted for the shell.
function __psychic_insert_path
    set -l result (command psychic --on-file-click=print-to-stdout --on-dir-click=print-to-stdout)
    if test -n "$result"
        if string match -q -- "$PWD/*" $result
            set result (string sub --start (math (string length -- "$PWD/") + 1) -- $result)
        end
        commandline --insert -- (string escape -- $result)" "
    end
    commandline --function repaint
end

# Bound to Ctrl-T only where nothing else (fzf, say) has claimed it. Set
# PSYCHIC_NO_CTRL_T before loading this to skip it; bind the function yourself
# with, e.g.,
#   bind \cx\cf __psychic_insert_path
if not set -q PSYCHIC_NO_CTRL_T
    set -l __psychic_current (bind \ct 2>/dev/null)
    if test -z "$__psychic_current"; or string match -q -- '*transpose-chars*' $__psychic_current
        bind \ct __psychic_insert_path
        bind -M insert \ct __psychic_insert_path
    end
end

# =============================================================================
# Command history: record each command for `psychic history` (Ctrl-R)
# =============================================================================

# Off until `psychic history --enable`. PSYCHIC_HISTORY=0 or 1 overrides.
set -g __psychic_session "$fish_pid-"(random)
set -g __psychic_cmd_pwd $PWD

function __psychic_history_enabled
    switch "$PSYCHIC_HISTORY"
        case 0
            return 1
        case 1
            return 0
    end
    set -l dir $HOME/.local/share/psychic
    set -q PSYCHIC_DATA_DIR; and set dir $PSYCHIC_DATA_DIR
    test -e $dir/history-enabled
end

# Before a command runs: remember where it was typed.
function __psychic_preexec --on-event fish_preexec
    set -g __psychic_cmd_pwd $PWD
end

# After it: one background call with how it went. Disowned, so fish prints no
# "Job has ended" line.
function __psychic_postexec --on-event fish_postexec
    set -l __psychic_status $status
    __psychic_history_enabled; or return
    set -q fish_private_mode; and return
    test -n "$argv[1]"; or return
    # fish keeps a command out of its history when it starts with a space,
    # or, if the user defined fish_should_add_to_history, when that says so
    # (it then decides alone, leading space included).
    if functions -q fish_should_add_to_history
        fish_should_add_to_history "$argv[1]" >/dev/null 2>&1; or return
    else
        string match -q -- ' *' "$argv[1]"; and return
    end
    set -l __psychic_bin psychic
    set -q PSYCHIC_BIN; and set __psychic_bin $PSYCHIC_BIN
    # Not installed (or not on PATH): fish would print "command not found",
    # and the command line with it, at every prompt.
    command -q $__psychic_bin; or return
    set -l __psychic_dir
    set -q PSYCHIC_DATA_DIR; and set __psychic_dir --data-dir $PSYCHIC_DATA_DIR
    command $__psychic_bin $__psychic_dir track-command --cwd "$__psychic_cmd_pwd" \
        --exit-code $__psychic_status --duration-ms "$CMD_DURATION" \
        --session "$__psychic_session" -- "$argv[1]" >/dev/null 2>&1 &
    disown 2>/dev/null
end

# =============================================================================
# Ctrl-R: search command history in psychic
# =============================================================================

# The picker, searching for $argv[1] first. Prints the pick, or nothing
# (Escape). Fails, printing nothing, when the binary is not installed.
function __psychic_history_run
    set -l __psychic_bin psychic
    set -q PSYCHIC_BIN; and set __psychic_bin $PSYCHIC_BIN
    command -q $__psychic_bin; or return 1
    set -l __psychic_dir
    set -q PSYCHIC_DATA_DIR; and set __psychic_dir --data-dir $PSYCHIC_DATA_DIR
    command $__psychic_bin $__psychic_dir history --session="$__psychic_session" --cwd="$PWD" --query="$argv[1]"
end

# Enter on a command replaces the command line with it; Escape leaves it alone.
function __psychic_history_widget
    set -l result (__psychic_history_run (commandline | string collect) | string collect)
    if test -n "$result"
        commandline --replace -- $result
    end
    commandline --function repaint
end

# Bound to Ctrl-R only where history search is on and nothing else has claimed
# the key, in the default and insert modes each. Set PSYCHIC_NO_CTRL_R before
# loading this to skip it; bind the function yourself with, e.g.,
#   bind \cx\cr __psychic_history_widget
# What became of it goes into PSYCHIC_CTRL_R, for `psychic history --status`:
# bound (both modes), partly, taken (neither: something else has Ctrl-R),
# recording-off or opted-out.
if set -q PSYCHIC_NO_CTRL_R
    set -gx PSYCHIC_CTRL_R opted-out
else if not __psychic_history_enabled
    set -gx PSYCHIC_CTRL_R recording-off
else
    # --user: fish 4 lists the preset binding beside the user's own. Each
    # mode on its own: a user's insert-mode Ctrl-R is theirs too.
    set -l __psychic_bound 0
    for __psychic_mode in default insert
        set -l __psychic_current (bind --user -M $__psychic_mode \cr 2>/dev/null)
        if string match -q -- '*__psychic_history_widget*' $__psychic_current
            set __psychic_bound (math $__psychic_bound + 1) # an earlier `source`
        else if test -z "$__psychic_current"; or string match -q -- '*history-pager*' $__psychic_current
            bind -M $__psychic_mode \cr __psychic_history_widget
            set __psychic_bound (math $__psychic_bound + 1)
        end
    end
    switch $__psychic_bound
        case 2
            set -gx PSYCHIC_CTRL_R bound
        case 1
            set -gx PSYCHIC_CTRL_R partly
        case '*'
            set -gx PSYCHIC_CTRL_R taken
    end
end

# `ph [words]`: the same picker, typed instead of bound, searching for the
# words. The pick goes onto the next prompt's command line, ready to edit and
# run; nothing is run.
function __psychic_history_typed
    set -l result (__psychic_history_run "$argv" | string collect)
    if test -n "$result"
        commandline --replace -- $result
    end
    return 0
end

# Defined unless PSYCHIC_NO_PH is set or `ph` is already the user's (a
# function or command); kept as ours when this script is sourced again.
if not set -q PSYCHIC_NO_PH; and begin; set -q __psychic_ph_ours; or not type -q ph; end
    set -g __psychic_ph_ours 1
    function ph --description 'Search command history in psychic; the pick goes on the prompt'
        __psychic_history_typed $argv
    end
end
