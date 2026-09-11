# THE FOREIGN PROFILE'S ABI, IN ONE PLACE, because two descriptions of one ABI is how a C object and
# a Rust object come to disagree about a type - and this file exists because there were about to be
# three: the cross files, the portable static target's build, and the image build that compiles a
# foreign library.
#
# IT IS SOURCED, NOT RUN. `foreign_profile_abi <arch>` sets three variables and returns:
#
#   foreign_triple      the clang target triple, which is the SAME triple the Rust half is built for
#   foreign_abi_flags   everything the triple does not already fix, as an array
#   foreign_arch        the short name, which is what the per-target profile header is called
#
# THE FLAGS ARE WHAT THE TRIPLE DOES NOT SAY, and no more than that. An ABI value the triple already
# fixes is one this file must not repeat: `-mgeneral-regs-only` on aarch64 was exactly that mistake,
# a second description of an ABI the Rust half fixes, and the pinned sources' use of `double` is what
# made it fail rather than merely disagree.

foreign_profile_abi() {
	local arch="$1"
	case "$arch" in
	x86_64 | x86_64-unknown-none)
		foreign_arch="x86_64"
		foreign_triple="x86_64-unknown-none-elf"
		foreign_abi_flags=(-mno-mmx -msse -msse2 -mno-sse3 -mno-ssse3 -mno-sse4.1 -mno-sse4.2 -mno-avx -mno-avx2 -mfxsr -mno-red-zone)
		;;
	aarch64 | aarch64-unknown-none)
		foreign_arch="aarch64"
		foreign_triple="aarch64-unknown-none"
		# NO FLAGS. `aarch64-unknown-none` is the hardfloat AAPCS variant and the Rust half reports
		# `target_feature="neon"` under it, so clang's defaults for this triple already match.
		foreign_abi_flags=()
		;;
	riscv64 | riscv64gc | riscv64gc-unknown-none-elf)
		foreign_arch="riscv64"
		foreign_triple="riscv64-unknown-none-elf"
		foreign_abi_flags=(-march=rv64gc -mabi=lp64d -mcmodel=medany)
		;;
	*)
		echo "foreign-profile-abi: unsupported architecture '$arch'" >&2
		return 2
		;;
	esac
}
