# glassy shell integration for bash.
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
# Source it from your ~/.bashrc:
#
#   [ -n "$GLASSY_VERSION" ] && source /path/to/shell-integration/glassy.bash
#
# It is a no-op outside glassy and harmless to source unconditionally.

# Only activate inside glassy, and only once per shell.
if [ -n "${GLASSY_SHELL_INTEGRATION:-}" ]; then
	return 0 2>/dev/null || true
fi
if [ "${TERM_PROGRAM:-}" != "glassy" ] && [ -z "${GLASSY_FORCE_INTEGRATION:-}" ]; then
	return 0 2>/dev/null || true
fi
GLASSY_SHELL_INTEGRATION=1

__glassy_esc() {
	# OSC <code> ; <payload> ST   (ST = ESC \). Args are joined verbatim.
	printf '\033]%s\033\\' "$1"
}

# OSC 7: report the cwd as a file:// URL so glassy can inherit it.
__glassy_osc7() {
	local path="${PWD}"
	# Percent-encode everything but the safe unreserved set + '/'.
	local enc='' i ch
	for (( i = 0; i < ${#path}; i++ )); do
		ch="${path:$i:1}"
		case "$ch" in
			[a-zA-Z0-9/._~-]) enc+="$ch" ;;
			*) enc+=$(printf '%%%02X' "'$ch") ;;
		esac
	done
	__glassy_esc "7;file://${HOSTNAME}${enc}"
}

# Prompt-start mark (133;A) + cwd, emitted at the very top of PS1.
__glassy_prompt_start() {
	__glassy_esc "133;A"
	__glassy_osc7
}

# Command-finished mark (133;D;<exit>), emitted at the top of PROMPT_COMMAND
# (which runs after the previous command, before the next prompt is drawn). The
# very first prompt has no preceding command, so suppress the bogus D then.
__glassy_precmd() {
	local exit=$?
	if [ -n "${__glassy_cmd_running:-}" ]; then
		__glassy_esc "133;D;${exit}"
		__glassy_cmd_running=
	fi
}

# Command-start mark (133;C), emitted just before a command runs via the DEBUG
# trap. We mark only the first command of a prompt (the trap fires per simple
# command); __glassy_preexec_done gates it.
__glassy_preexec() {
	# Skip while drawing the prompt itself (PROMPT_COMMAND runs commands too).
	if [ -n "${COMP_LINE:-}" ]; then return; fi
	if [ -n "${__glassy_preexec_done:-}" ]; then return; fi
	__glassy_preexec_done=1
	__glassy_cmd_running=1
	__glassy_esc "133;C"
}

# Wire PROMPT_COMMAND (precmd) and the DEBUG trap (preexec).
case "${PROMPT_COMMAND:-}" in
	*__glassy_precmd*) : ;;
	*) PROMPT_COMMAND="__glassy_precmd${PROMPT_COMMAND:+; }${PROMPT_COMMAND:-}" ;;
esac

# Reset the preexec gate when a new prompt is drawn (PS0 fires on enter, after
# the trap; PROMPT_COMMAND re-arms the gate for the next line).
PROMPT_COMMAND="${PROMPT_COMMAND}; __glassy_preexec_done="

trap '__glassy_preexec' DEBUG

# Wrap PS1 with the prompt-start (A) prefix and the command-start (B) suffix so
# glassy sees the prompt zone boundaries. The `\$(...)` is escaped so bash
# re-evaluates it on EVERY prompt (fresh A mark + cwd), not once at wrap time.
if [ -z "${__glassy_ps1_wrapped:-}" ]; then
	PS1="\[\$(__glassy_prompt_start)\]${PS1}\[\$(__glassy_esc '133;B')\]"
	__glassy_ps1_wrapped=1
fi
