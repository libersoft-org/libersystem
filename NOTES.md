# Bugs / changes

- Move fat and liberfs directories to a new "fs" directory where all future file systems will be placed in their own folders
- LiberFS lib.rs has 2700 lines of code - is it OK or should it be divided to multiple files?
- GPU driver keeps failing probably (screen sometimes blinking after few minutes - probably restartind driver or something)
- "exit" command should not halt the machine, but just exits the shell and shows up the parent shell (shell that started this console). If there is no parent, just reload the whole shell
- List the available volumes "ls" - change to "lsvol"?

# New features

- Look for other FS compatibility (NTFS, ext4, xfs, CD-ROM etc.)
- Compare LiberFS with other file systems (fat, ntfs, ext4, zfs, btrfs, xfs and other modern file systems) and compare their abilities - write the MD document about it
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
- There are "build" / "target" folders in "src", should it be somewhere else (a folder above)?
- SSH server

# Questions / other

- What is the difference between SYS_CLOCK_GET, SYS_CLOCK_RTC and SYS_CLOCK_MONO_NS? Also clock(), clock_rtc() and clock_ns()?
- Virtio drivers - MSI-X only? Remove the old IRQ?
