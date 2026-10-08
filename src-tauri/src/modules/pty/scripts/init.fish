# rcode-shell-integration (fish)
# Emits OSC 7 (cwd) + OSC 133 A/B/C/D so the host tracks cwd and prompt
# boundaries without re-parsing the prompt. fish 4.0+ writes its own OSC 133
# A/B (the `mark-prompt` feature); RCode disables it at spawn via
# fish_features=no-mark-prompt so these markers aren't emitted twice.

# Sourced by RCode after the user configuration has loaded.
if not set -q RCODE_TERMINAL
    exit 0
end
if set -q __RCODE_HOOKS_LOADED
    exit 0
end
set -g __RCODE_HOOKS_LOADED 1

if set -q RCODE_CLI; and test -x "$RCODE_CLI"
    function rcode
        command "$RCODE_CLI" $argv
    end
end

# RCode is a clean terminal; drop fish's default startup greeting. A user who
# sets their own in config.fish (sourced after this) keeps it.
function fish_greeting
end

set -g __RCODE_HOST (uname -n 2>/dev/null; or echo localhost)

# URL-encode a path keeping `/` intact so it stays valid inside file://.
function __rcode_urlencode_path
    set -l parts (string split '/' -- $argv[1])
    set -l out
    for p in $parts
        if test -n "$p"
            set out $out (string escape --style=url -- $p)
        else
            set out $out ""
        end
    end
    string join '/' $out
end

function __rcode_restore_status
    return $argv[1]
end

function __rcode_capture_user_prompt
    if not functions -q fish_prompt
        return
    end
    if functions fish_prompt | string match -q '*__rcode_user_prompt*'
        return
    end
    functions -e __rcode_user_prompt 2>/dev/null
    functions -c fish_prompt __rcode_user_prompt
end

# Wrapped so `fish -C __rcode_install_prompt` can re-run it AFTER config.fish,
# where a framework prompt (starship etc.) would otherwise override fish_prompt
# and drop our markers.
function __rcode_install_prompt
    # ponytail: cover Conda's named wrapper; generalize if another prompt
    # framework preserves RCode indirectly.
    if not set -q RCODE_BLOCKS
        and functions -q __fish_prompt_orig
        and functions fish_prompt | string match -q '*__fish_prompt_orig*'
        and functions __fish_prompt_orig | string match -q '*__rcode_user_prompt*'
        return
    end
    __rcode_capture_user_prompt
    if set -q RCODE_BLOCKS
        function fish_right_prompt
        end
        function fish_greeting
        end
    end
    function fish_prompt
        set -l __rcode_status $status
        printf '\e]133;D;%d\e\\' $__rcode_status
        printf '\e]7;file://%s%s\e\\' "$__RCODE_HOST" (__rcode_urlencode_path "$PWD")
        printf '\e]133;A\e\\'
        # Block mode: host renders its own input bar, so suppress the shell prompt
        # (B marker only) and reserve header/gap rows, mirroring zsh.
        if set -q RCODE_BLOCKS
            if set -q __rcode_block_seen
                printf '\n\n'
            else
                printf '\n'
            end
            printf '\e]133;B\e\\'
            return
        end
        __rcode_restore_status $__rcode_status
        if functions -q __rcode_user_prompt
            __rcode_user_prompt
        else
            printf '%s > ' (prompt_pwd)
        end
        printf '\e]133;B\e\\'
    end
end
__rcode_install_prompt

function __rcode_preexec --on-event fish_preexec
    set -g __rcode_block_seen 1
    set -l cmd (string replace -ra '[\x00-\x1f\x7f]' ' ' -- "$argv")
    printf '\e]133;C;%s\e\\' (string sub -l 256 -- "$cmd")
end
