# glassy shell integration for fish.
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
# Source it from your ~/.config/fish/config.fish:
#
#   if set -q GLASSY_VERSION
#       source /path/to/shell-integration/glassy.fish
#   end
#
# It is a no-op outside glassy and harmless to source unconditionally.

# Only activate inside glassy, and only once per shell.
if set -q GLASSY_SHELL_INTEGRATION
	exit 0 2>/dev/null; or return 0
end
if test "$TERM_PROGRAM" != glassy; and not set -q GLASSY_FORCE_INTEGRATION
	return 0
end
set -g GLASSY_SHELL_INTEGRATION 1

function __glassy_esc
	# OSC <payload> ST   (ST = ESC \).
	printf '\033]%s\033\\' $argv[1]
end

function __glassy_osc7
	# fish exposes the URL-encoded cwd directly; fall back to a manual encode.
	set -l path (string escape --style=url -- "$PWD" | string replace -a '%2F' '/')
	__glassy_esc "7;file://"(hostname)"$path"
end

# A (prompt start) + cwd, emitted from fish_prompt via an event so it does not
# disturb a user-defined fish_prompt function.
function __glassy_prompt_start --on-event fish_prompt
	# Emit D for the just-finished command (if one ran). $status here is the
	# status of the last command, captured before anything else clobbers it.
	if set -q __glassy_cmd_running
		__glassy_esc "133;D;$__glassy_last_status"
		set -e __glassy_cmd_running
	end
	__glassy_esc "133;A"
	__glassy_osc7
	__glassy_esc "133;B"
end

# C (command executed) + remember that a command is running, emitted right
# before execution via the preexec event.
function __glassy_preexec --on-event fish_preexec
	set -g __glassy_cmd_running 1
	__glassy_esc "133;C"
end

# Capture $status at the very start of fish_postexec so the D mark reports the
# real exit code of the command that just ran.
function __glassy_postexec --on-event fish_postexec
	set -g __glassy_last_status $status
end
