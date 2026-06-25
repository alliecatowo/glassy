# glassy shell integration for zsh.
#
# Emits OSC 133 semantic prompt marks so glassy can group output into command
# blocks and show an exit-status badge + duration next to each prompt:
#
#   OSC 133 ; A ST          prompt start
#   OSC 133 ; B ST          prompt end / command start (user is about to type)
#   OSC 133 ; C ST          command executed (output begins)
#   OSC 133 ; D ; <exit> ST command finished, with its exit code
#
# It also emits OSC 7 (cwd) so new tabs/splits inherit the working directory.
#
# Source it from your ~/.zshrc:
#
#   [[ -n "$GLASSY_VERSION" ]] && source /path/to/shell-integration/glassy.zsh
#
# It is a no-op outside glassy and harmless to source unconditionally.

# Only activate inside glassy, and only once per shell.
if [[ -n "${GLASSY_SHELL_INTEGRATION:-}" ]]; then
	return 0
fi
if [[ "${TERM_PROGRAM:-}" != "glassy" && -z "${GLASSY_FORCE_INTEGRATION:-}" ]]; then
	return 0
fi
typeset -g GLASSY_SHELL_INTEGRATION=1

__glassy_esc() {
	# OSC <payload> ST   (ST = ESC \).
	printf '\033]%s\033\\' "$1"
}

__glassy_osc7() {
	# zsh's ${(j::)...} + character loop percent-encodes the cwd path.
	local path="${PWD}" enc='' i ch
	for (( i = 1; i <= ${#path}; i++ )); do
		ch="${path[i]}"
		case "$ch" in
			([a-zA-Z0-9/._~-]) enc+="$ch" ;;
			(*) enc+=$(printf '%%%02X' "'$ch") ;;
		esac
	done
	__glassy_esc "7;file://${HOST}${enc}"
}

# precmd: runs right before each prompt is drawn. Emit D (with the just-finished
# command's exit code) if a command actually ran, then A (prompt start) + cwd.
__glassy_precmd() {
	local exit=$?
	if [[ -n "${__glassy_cmd_running:-}" ]]; then
		__glassy_esc "133;D;${exit}"
		__glassy_cmd_running=
	fi
	__glassy_esc "133;A"
	__glassy_osc7
}

# preexec: runs after the user hits enter, before the command executes. Emit C
# (command executed / output begins) and remember that a command is running so
# precmd knows to emit D for it.
__glassy_preexec() {
	__glassy_cmd_running=1
	__glassy_esc "133;C"
}

# B (prompt end / command start) is appended to PS1 so it lands right where the
# user starts typing. Guard against double-wrapping on re-source.
if [[ -z "${__glassy_ps1_wrapped:-}" ]]; then
	PS1="${PS1}%{$(__glassy_esc '133;B')%}"
	typeset -g __glassy_ps1_wrapped=1
fi

autoload -Uz add-zsh-hook 2>/dev/null
if (( $+functions[add-zsh-hook] )); then
	add-zsh-hook precmd __glassy_precmd
	add-zsh-hook preexec __glassy_preexec
else
	# Fallback for ancient zsh without add-zsh-hook.
	precmd_functions+=(__glassy_precmd)
	preexec_functions+=(__glassy_preexec)
fi
