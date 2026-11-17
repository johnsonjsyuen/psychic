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
