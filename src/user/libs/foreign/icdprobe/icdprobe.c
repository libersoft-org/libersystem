/* The admitted synthetic ICD: two exports, versions 2 through 6.

   EVERYTHING NOT EXPORTED IS `static`. A shared library built from C exports every non-static
   function by default, and this artifact's surface is exactly two names - so the audit that checks
   an export has one owner would otherwise be checking helpers nobody meant to publish. */

#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* The Loader/Driver interface's own result values, spelled here rather than included: the pinned
   Vulkan headers are audit-only and this artifact is built from this system's profile sysroot. Both
   numbers are part of the interface, not of any header's implementation. */
#define ICD_SUCCESS 0
#define ICD_ERROR_INCOMPATIBLE_DRIVER (-9)

/* WHAT THIS DRIVER WILL SPEAK. The floor is 2 because negotiation does not exist below it; the
   ceiling is 6 because at 7 the interface functions MAY be obtained through
   `vk_icdGetInstanceProcAddr` instead of being exported, and a driver that stopped exporting them is
   one this substrate cannot resolve. */
#define ICD_LOWEST 2u
#define ICD_HIGHEST 6u

/* A NEGOTIATION AND NOT AN ANNOUNCEMENT. The caller writes the highest version IT supports; this
   writes back the highest both can speak, or refuses when there is none. One driver therefore covers
   both ends of the admitted range, which is what lets a gate exercise the range rather than one
   convenient member of it. */
int32_t vk_icdNegotiateLoaderICDInterfaceVersion(uint32_t *version) {
	if (version == NULL) {
		return ICD_ERROR_INCOMPATIBLE_DRIVER;
	}
	if (*version < ICD_LOWEST) {
		/* The caller cannot speak anything this driver speaks. The value is left as the caller
		   wrote it: reporting a version that was never agreed is how a loader comes to call an
		   entry point that is not there. */
		return ICD_ERROR_INCOMPATIBLE_DRIVER;
	}
	if (*version > ICD_HIGHEST) {
		*version = ICD_HIGHEST;
	}
	return ICD_SUCCESS;
}

/* THE NAME TABLE, AND IT IS EMPTY OF VULKAN. An ICD answers this with the entry points it
   implements; this one implements none, so every query is answered with the null pointer - which is
   the same answer a real driver gives for a function it does not have, and is therefore a shape the
   caller already handles rather than a special case this fixture invents.

   The one name it does answer is its own negotiation function, which is what an interface-version-7
   caller would ask for. Answering it at every admitted version is deliberate: the substrate never
   negotiates above 6, so this is the one query whose answer proves the table is consulted at all. */
static int same_name(const char *name, const char *known, size_t length) {
	/* `memcmp` AND NOT `strcmp`, because the substrate provides the four memory functions and no
	   string comparison at all - which is the exact-surface rule showing through into a caller. The
	   terminator is compared with the rest, so a name that merely starts the same does not match. */
	return memcmp(name, known, length + 1) == 0;
}

void *vk_icdGetInstanceProcAddr(void *instance, const char *name) {
	static const char negotiate[] = "vk_icdNegotiateLoaderICDInterfaceVersion";
	(void)instance;
	if (name == NULL) {
		return NULL;
	}
	if (same_name(name, negotiate, sizeof(negotiate) - 1)) {
		return (void *)(uintptr_t)&vk_icdNegotiateLoaderICDInterfaceVersion;
	}
	return NULL;
}
