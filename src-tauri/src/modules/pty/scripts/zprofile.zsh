# rcode-shell-integration (zprofile)
#
# See zshenv.zsh for the rationale on the trailing `:`.
{
  _rcode_user_zdotdir="${RCODE_USER_ZDOTDIR:-$HOME}"
  [ -f "$_rcode_user_zdotdir/.zprofile" ] && source "$_rcode_user_zdotdir/.zprofile"
  unset _rcode_user_zdotdir
}
:
