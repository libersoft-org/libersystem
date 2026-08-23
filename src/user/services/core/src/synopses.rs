// The one-line synopses the shell's `help` prints. SEPARATE FROM `commands.rs` because that module
// compiles into ConsoleService too, which completes command words and never describes them.
//
// A one-line synopsis for every command the shell knows - the builtins it runs itself
// and the governed / net tools it launches. The `help` builtin prints the whole table
// (sorted); `<command> --help` (and `help <command>`) prints one row. Kept here, one
// place, so a command gets `--help` without every tool ELF hand-rolling a usage string.
// Each entry is (command word, "synopsis - what it does"). Grown in step with the
// dispatch tables (TOOLS / NET_TOOLS in shell.rs and the builtin arms).
pub const SYNOPSES: &[(&str, &str)] = &[
	// shell builtins
	("cd", "cd [dir] - change the working directory (no arg: the system volume)"),
	("env", "env - list the environment variables"),
	("unset", "unset NAME - remove an environment variable (NAME=VALUE sets one)"),
	("jobs", "jobs - list background and stopped jobs"),
	("fg", "fg [job] - resume a job in the foreground"),
	("bg", "bg [job] - resume a stopped job in the background"),
	("time", "time COMMAND - run COMMAND and print its wall-clock time"),
	("clear", "clear - clear the screen"),
	("size", "size - print the terminal size"),
	("resize", "resize COLSxROWS - request a new terminal size"),
	("graph", "graph [json|json-min] - render the live system graph"),
	("mouse", "mouse - toggle the mouse-reporting demo view"),
	("exit", "exit - log out of this shell (also: quit)"),
	("quit", "quit - log out of this shell (also: exit)"),
	("reboot", "reboot - restart the machine"),
	("poweroff", "poweroff - power the machine off (also: shutdown)"),
	("shutdown", "shutdown - power the machine off (also: poweroff)"),
	("help", "help [command] - list the commands, or show one command's usage"),
	// filesystem / inventory tools
	("ls", "ls [-s KEY][-u UNIT][json] [path] - list a directory"),
	("du", "du [-s][-h][json] [path] - recursive disk usage of a tree"),
	("cat", "cat PATH - print a file"),
	("lico", "lico [DIRECTORY] - open the two-panel file manager"),
	("licoview", "licoview PATH - view a bounded text preview"),
	("licoedit", "licoedit PATH - edit a bounded buffer without persistence"),
	("write", "write PATH TEXT - write TEXT to a file"),
	("rm", "rm PATH - remove a file"),
	("pwd", "pwd [json] - print the inherited working directory"),
	("kill", "kill [--term|--kill|--stop|--continue] JOB... - signal a session job"),
	("sort", "sort [-r][-u][-n][-k FIELD] PATH - order a file's lines"),
	("cut", "cut -b|-c|-f RANGES [-d CHAR] PATH - select parts of each line"),
	("tree", "tree [-L N][-d|-f][-s][json] [path] - render a directory tree"),
	("find", "find [path...] [--name GLOB][--type f|d] - print matching paths"),
	("grep", "grep [-i][-v][-n][-c][-l] TEXT PATH... - print matching lines"),
	("cp", "cp [-f] SRC DST - copy a file, published only when complete"),
	("mv", "mv [-f] SRC DST - rename within a volume, copy-verify-delete across"),
	("clear", "clear - erase the display and home the cursor"),
	("which", "which NAME... - print the artifact each command name resolves to"),
	("wc", "wc PATH... [json] - count lines, words, bytes and characters"),
	("head", "head [-n N][-c N][-q] PATH... - print the first lines or bytes"),
	("tail", "tail [-n N][-f] PATH - print the last lines, optionally following"),
	("hexdump", "hexdump [-s SKIP][-n LEN] PATH - offsets, hex bytes and ASCII"),
	("tee", "tee [-a][--fail-fast] PATH... - copy the input stream to stdout and to files"),
	("watch", "watch [-n SECONDS] COMMAND [ARG...] - rerun a command and show its latest output"),
	("less", "less [-N][-S] [PATH] - page a file or the input stream one screen at a time"),
	("traceroute", "traceroute [-m HOPS][-q PROBES] HOST [json] - the path a packet takes, hop by hop"),
	("truncate", "truncate -s SIZE PATH... - set a file length (+/- is relative)"),
	("touch", "touch [-c][-d SECS] PATH... - stamp mtime, creating when asked"),
	("mkdir", "mkdir PATH - create a directory"),
	("rmdir", "rmdir PATH - remove an empty directory"),
	("snap", "snap <create|list|delete|restore> ... - volume snapshots"),
	("volume", "volume <status|compress|...> ... - volume health and policy"),
	("lsvol", "lsvol [json] - list the volumes with size / used / free"),
	("lsblk", "lsblk [json] - list the block devices and their volumes"),
	("lsdev", "lsdev [json] - list the device nodes"),
	("lscpu", "lscpu [json] - print the CPU inventory"),
	("lsmem", "lsmem [json] - print the boot memory map"),
	("lsirq", "lsirq [json] - print the device-interrupt vectors"),
	("lspci", "lspci [json] - print the PCI bus scan"),
	("lsusb", "lsusb [json] - print the USB bus inventory"),
	("lssvc", "lssvc [json] [prefix] - list the services and their state"),
	// system / status tools
	("ps", "ps [json] - list the running processes"),
	("free", "free [-h] - print the memory totals"),
	("uname", "uname - print the system identity"),
	("uptime", "uptime - print how long the system has been up"),
	("date", "date - print the wall-clock date and time"),
	("dmesg", "dmesg - print the kernel boot log"),
	("log", "log [--boot N] [json] - query the structured journal"),
	("config", "config [KEY | set KEY VALUE] - read or set the config tree"),
	("set", "set KEY VALUE - set a config node (also: config set)"),
	("perm", "perm [json] - print the permission audit trail"),
	("usage", "usage [json] - print the per-domain resource budgets"),
	("beep", "beep [FREQ [MS]] - play a tone"),
	("audiorec", "audiorec [-r HZ][-c N][-s SECS][-f] PATH - record PCM audio to a WAV file"),
	("audioconv", "audioconv [--format F][--rate HZ][--channels N][--bits N][-f] IN OUT - convert audio"),
	("stop", "stop SERVICE - stop a service and its dependents"),
	("run", "run NAME [args] - launch a governed tool by name"),
	// network tools
	("ip", "ip - show the network interfaces (also: net)"),
	("net", "net - show the network interfaces (also: ip)"),
	("arp", "arp - show the neighbour (ARP) cache"),
	("ss", "ss - show the sockets (also: netstat)"),
	("netstat", "netstat - show the sockets (also: ss)"),
	("ping", "ping HOST - probe a host with ICMP echo"),
	("nslookup", "nslookup HOST - resolve a host name (also: host)"),
	("host", "host HOST - resolve a host name (also: nslookup)"),
	("tcp", "tcp HOST PORT - open a TCP connection"),
	("nc", "nc HOST PORT - a netcat-style TCP client"),
	("httpd", "httpd [port] - serve the system volume over HTTP (backgrounds)"),
];

// The synopsis for `command`, if it is a known command.
pub fn synopsis(command: &[u8]) -> Option<&'static str> {
	for &(name, text) in SYNOPSES {
		if name.as_bytes() == command {
			return Some(text);
		}
	}
	None
}
