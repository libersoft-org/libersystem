// The shell's builtins - the command words the shell dispatches itself rather than
// launching from the system volume's bin/. Shared between the shell (which prints the
// matches of a completion request) and ConsoleService's line discipline (which completes
// the command word on Tab): completion offers these plus the live bin/ listing, the way
// bash completes its builtins plus $PATH. Grown in step with the shell's dispatch table.

pub const BUILTINS: &[&str] = &[
	"bg",
	"cd",
	"clear",
	"env",
	"exit",
	"fg",
	"graph",
	"host",
	"jobs",
	"mouse",
	"net",
	"netstat",
	"poweroff",
	"quit",
	"reboot",
	"resize",
	"shutdown",
	"size",
	"unset",
];
