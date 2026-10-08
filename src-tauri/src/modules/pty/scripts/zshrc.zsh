# Modified for RCode. See NOTICE.
# rcode-shell-integration (zshrc)
#
# Emits OSC 7 (cwd) + OSC 133 A/B/C/D (prompt-start / prompt-end / pre-exec /
# command-done-with-exit-code) so the host can detect command boundaries and
# track cwd without re-parsing the prompt. `status` is a read-only special in
# zsh, so we shadow $? into `_rcode_ret`.

{
  _rcode_user_zdotdir="${RCODE_USER_ZDOTDIR:-$HOME}"
  [ -f "$_rcode_user_zdotdir/.zshrc" ] && source "$_rcode_user_zdotdir/.zshrc"
  unset _rcode_user_zdotdir
}

if [[ -n "$RCODE_CLI" && -x "$RCODE_CLI" ]]; then
  rcode() {
    command "$RCODE_CLI" "$@"
  }
fi

# Re-source guard within a single shell (e.g. user runs `source ~/.zshrc`).
# This is NOT exported, so each nested zsh installs its own hooks : desired,
# since every interactive shell needs its own prompt integration.
if [[ -z "$__RCODE_HOOKS_LOADED" ]]; then
  __RCODE_HOOKS_LOADED=1
  autoload -Uz add-zsh-hook 2>/dev/null

  # URL-encode $PWD byte-wise so multi-byte paths stay valid in the `file://`
  # URI emitted via OSC 7. `no_multibyte` forces ${s[i]} to index bytes (not
  # code points), and LC_ALL=C keeps the [a-zA-Z0-9...] class single-byte.
  _rcode_urlencode() {
    emulate -L zsh
    setopt localoptions no_multibyte
    local LC_ALL=C s="$1" i byte
    for (( i=1; i<=${#s}; i++ )); do
      byte="${s[i]}"
      case "$byte" in
        [a-zA-Z0-9/._~-]) printf '%s' "$byte" ;;
        *) printf '%%%02X' "'$byte" ;;
      esac
    done
  }

  _rcode_precmd() {
    local _rcode_ret=$?
    printf '\e]133;D;%s\e\\' "$_rcode_ret"
    printf '\e]7;file://%s%s\e\\' "${HOST}" "$(_rcode_urlencode "$PWD")"
    # In block mode the host renders its own input bar, so suppress the shell
    # prompt entirely (keep only the OSC 133 B marker) and add a leading blank
    # line so frozen command blocks get vertical breathing room.
    if [[ -n "$RCODE_BLOCKS" ]]; then
      # Spacing reserved for the host-drawn block header lives in the prompt
      # (the grid is WebGL, not CSS). Later prompts get two blank rows: the
      # upper one is the previous block's end gap (above its divider), the lower
      # one is this command's header row. The very first prompt has no block
      # above it, so it gets a single row (header only) to avoid a tall top gap.
      if [[ -n "$_rcode_block_seen" ]]; then
        PS1=$'\n\n%{\e]133;B\e\\%}'
      else
        PS1=$'\n%{\e]133;B\e\\%}'
      fi
      RPROMPT=''
    elif [[ "$PS1" != *$'\e]133;B\e\\'* ]]; then
      # Re-inject prompt-end marker in case a framework rebuilt PS1 (p10k, starship).
      PS1=$'%{\e]133;B\e\\%}'"$PS1"
    fi
    printf '\e]133;A\e\\'
  }

  _rcode_preexec() {
    # Mark that a real command ran, so the next prompt switches from one blank
    # row (first prompt, no block above) to two (end gap + header row).
    [[ -n "$RCODE_BLOCKS" ]] && _rcode_block_seen=1
    local cmd="${1//[[:cntrl:]]/ }"
    printf '\e]133;C;%s\e\\' "${cmd[1,256]}"
  }

  if (( $+functions[add-zsh-hook] )); then
    add-zsh-hook precmd _rcode_precmd
    add-zsh-hook preexec _rcode_preexec
  fi

  # Stop at the word end while preserving explicit user key bindings.
  if (( $+widgets[emacs-forward-word] )) \
     && [[ "$(bindkey '\ef')" == '"^[f" forward-word' ]]; then
    bindkey '\ef' emacs-forward-word
  fi

  _rcode_precmd
fi
:
