# Bugs / changes

- "play" doesn't play any sound file
- check where in directory structures are non-system-essential apps (like image viewer, audio player etc.) and move them somewhere else
- add audio test file (.mp3 - "the audio system works correctly...")
- audio player (different formats)
- audio recorder (microphone)
- audio device selector tool (both input and output, when system has multiple sound devices)
- create image conversion tool
- web camera viewer / recorder
- tests are taking too long after every small task - optimization needed
- Can it run Doom?
- text selection by mouse lags horribly
- when I press and hold enter in shell, sometimes it writes "vol://system> vol://system>" on the same row (not just "vol://system>" on next row)
- why there is a "help" command again when it was deleted already?
- qemu - be on the same network like host
- lsvol - add column - device
- mountpoints as mount://
- liberfs - access time
- disk quotas per user (??)
- limits for subprocesses?
- lsblk doesn't show device id
- lsblk size doesn't corespond with volume size - find out why
- lscpu --json doesn't show the name and other attributes
- lsdev, lssvc - without --json parameter it still shows json, not a CLI text output
- lsirq - show as table with columns (same like lsvol)
- ls* - find out what should it show
- find out what other ls* should be added (lsof etc.)
- Is OS loading only drivers it detected or all?
- src/boot/qemu-*.sh scripts - find duplicities and make it one script only with parameters, use it in Justfile and fix it in documentation
- Remove Limine mentioning everywhere
- Add nvme and other generic drivers (add to CONCEPT_*.md that generic drivers will be available already in phase 2)
- Apps in vol://storage/bin/ are huge (hundreds of kB) - find out why
- Some commands are missing --help parameter
- How does the format of our binary files look like? describe it somewhere
- Every command (even simple ones as ls or lsvol) has the delay at the beginning for no reason ... something that linux shows instantly - find out why and fix.
- Some tools in vol://storage/bin/ miss --json and --json-min (of course not all of them can have it - like cat, echo, beep etc. - those are excluded for obvious reason)
- df, du
- lsblk is showing the type of block device (virtio-blk), mountpoint (vol://...) and size, but not the name of the device in device tree... also there should be table headers (device, type, volume, size)
- Selecting something from console by mouse is lagging a lot... the whole console lags a lot even when paging (shift + pg up/down)
- Console does not autocomplete by tab key as on Linux - it should autocomplete commands and local files (like cat ./mot -> should complete cat ./motd.txt)
- Where is vol://system/ physically stored? On ram disk or hard disk?
- Where are system tools stored? When I enter "ls" in vol://system/, I can see just 2 .txt files
- Is our shell a separated binary (like on Linux - bash, dash, fish etc.)?
- commands.rs shows up the list of builtin shell commands - some of them should be separated binaries
- Add commands for listing hw resources - lsblk, lspci etc.
- Search for big source code files, sort them by number of lines, create a plan to atomize them
- GPU driver keeps failing probably (screen sometimes blinking after few minutes - probably restarting driver or something)
- "exit" command should not halt the machine, but just exits the shell and shows up the parent shell (shell that started this console). If there is no parent, just reload the whole shell
- "exit" command gracefully stops everything and then halts, but poweroff doesn't... do the same gracefullness for poweroff
- Check what is in coreutils package (Debian)

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
- Demo showing graphics and sound capabilities
