# Bugs / changes

- after "ls" command console freezes
- "ls" command orders files and directories randomly - by default - by name, first directories, then files
- "ls" command should show directories with other color than files and should show size in next column and timestamp of last modification in another column, -h should make size human readable, at the and down there add summary - total directories, total files, total size of files
- Replace font8x8_latin.bin with something nicer - only one with CC license (!!!)
- Arrays in .rs files are written in 1 row - make it multiline, so it's better readable
- Check what is in coreutils package (Debian)
- After bootup, in shell it automatically presses enter after a few seconds - why?
- Where are system tools stored? When I enter "ls" in vol://system/, I can see just 2 .txt files
- Remove "Hello from the OS ramdisk!" after start and add 1 empty line before userspace (before MOTD)
- Remove "help" command, instead double tab should show the list of commands (the same as on Linux)
- Is our shell a separated binary (like on Linux - bash, dash, fish etc.)?
- Add commands for listing hw resources - lsblk, lspci etc.
- Search for big source code files, sort them by number of lines, create a plan to atomize them
- GPU driver keeps failing probably (screen sometimes blinking after few minutes - probably restartind driver or something)
- "exit" command should not halt the machine, but just exits the shell and shows up the parent shell (shell that started this console). If there is no parent, just reload the whole shell

# New features

- M42 - app package format
- M35k - console login and lock
- M35f - non-US keyboard layout
- When phase 2 is done, check if all matches with CONCEPT_EN/CZ.md
- THREAT_MODEL.md - check if everything in this document is correct and current, add it to README as a link
- Look for other FS compatibility (NTFS, ext4, xfs etc.)
- add "cd" command
- check for necessary utilities - https://popcon.debian.org/by_inst
- add "route" command
- add mc-like commander (lc?), mcedit-like editor and mcview file viewer
- Boot log - [ OK ] [FAIL] [INFO] [WARN]
- just run spice -> run spice - it doesnt show boot log, only >
- Nicer OS boot - colours in shell
- Optimize the code
- Find the dead code
- Find the duplicate / repetitive code
- There are "build" / "target" directories in "src", should it be somewhere else (a directory above)?
- SSH server

# Questions / other

- How does ramdisk work? Does it have some file system?
- What is the difference between SYS_CLOCK_GET, SYS_CLOCK_RTC and SYS_CLOCK_MONO_NS? Also clock(), clock_rtc() and clock_ns()?
- Virtio drivers - MSI-X only? Remove the old IRQ?
