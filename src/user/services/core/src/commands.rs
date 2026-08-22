// The shell's builtins - the command words the shell dispatches itself rather than
// launching from the system volume's bin/. Shared between the shell (which prints the
// matches of a completion request) and ConsoleService's line discipline (which completes
// the command word on Tab): completion offers these plus the live bin/ listing, the way
// bash completes its builtins plus $PATH. Grown in step with the shell's dispatch table.
//
// THE SYNOPSES ARE NOT HERE, and they were: this module compiles into BOTH binaries and only the
// shell reads them, so every console_service build reported two dead items and the note explaining
// why sat in this file. They live in `synopses.rs`, which only the shell declares. The two lists
// are still grown together - a builtin without a synopsis is a command `help` cannot describe.

pub const BUILTINS: &[&str] = &[
	"bg",
	"cd",
	"clear",
	"env",
	"exit",
	"fg",
	"graph",
	"help",
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
