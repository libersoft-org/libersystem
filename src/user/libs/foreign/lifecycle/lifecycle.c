/* The lifecycle fixture: a provider that RECORDS what ran and in what order.

   WHY A FIXTURE AND NOT AN ASSERTION. The converged closure of the pinned foreign configuration
   names one constructor and one destructor, which makes static initialisation a mechanism this
   system ADMITS rather than forbids - and an admitted mechanism needs its order, its failure and its
   crash behaviour observed rather than described. None of that is observable from outside a process,
   and a constructor runs before the console is adopted, so it cannot print. It can WRITE, and the
   program can print what it finds afterwards: this file is that written record.

   THE PROVIDER RECORDS AND THE CONSUMER READS, which is what makes the DAG order visible. A
   constructor here must run before one in anything built against this, so a sequence that reads
   `provider` then `consumer` is the ordering rule holding, and any other reading is it failing. */

#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* THE SEQUENCE IS A FIXED BUFFER AND NOT AN ALLOCATION. A constructor runs before anything has had a
   chance to set an allocator up, and an initialisation that needs one is an initialisation that can
   fail for a reason that has nothing to do with what it is testing. */
#define LIBER_LIFECYCLE_MAX 32
static char sequence[LIBER_LIFECYCLE_MAX + 1];
static size_t recorded;

/* WHAT A CONSTRUCTOR THAT COULD NOT FINISH LEAVES BEHIND. It is set BEFORE the step that fails and
   cleared after it, so a reader can tell "never started" from "started and did not finish" - which
   is the whole of what a partial initialisation is. */
static int incomplete;

/* AND WHAT THE ONE THAT DID NOT FINISH LEFT. Separate from `incomplete` on purpose: one says "the
   step that completes this ran", the other says "a step that should have completed did not". A
   single flag would make a constructor that never started look like one that stopped half way. */
static int partial;

void liber_lifecycle_note(char mark) {
	if (recorded < LIBER_LIFECYCLE_MAX) {
		sequence[recorded] = mark;
		recorded += 1;
		sequence[recorded] = '\0';
	}
}

const char *liber_lifecycle_sequence(void) { return sequence; }

int liber_lifecycle_incomplete(void) { return incomplete; }

int liber_lifecycle_partial(void) { return partial; }

/* `errno` IS PROCESS-WIDE HERE, and this is how that is observed rather than asserted: TWO IMAGES
   ask for it and must get one address. The storage and the accessor are in this provider and the
   consumer reaches the accessor across the image boundary, which is the same shape the real
   substrate has - it defines the accessor and the pinned configuration's objects call it.

   WHY THE ACCESSOR AND NOT A VARIABLE. `errno` is a macro over a function in every C library that
   has a per-thread model, and keeping that shape is what makes the answer meaningful: under a
   per-thread model this function would return a different address once a second thread existed.
   Under this one it returns the same address for the life of the process, which is what the
   no-thread-creation pin makes correct.

   THE NAME IS THE FIXTURE'S OWN, AND THAT IS A CORRECTION. It was `__liber_errno_location` - the
   substrate's own name - until the export-collision check on the audit-linked artifact found that
   the artifact and this fixture both defined it. They are never staged together today, but "never
   together today" is not a property anybody is holding fixed, and a symbol with two owners is the
   defect that check exists to find. What is being observed is the SHAPE - an accessor two images
   share - and the shape does not depend on the name. */
static int errno_storage;

int *liber_lifecycle_errno_location(void) { return &errno_storage; }

const void *liber_lifecycle_errno_slot(void) { return (const void *)liber_lifecycle_errno_location(); }

/* WHERE A DESTRUCTOR CAN SAY SOMETHING. A constructor runs before the console is adopted and can
   only write; a DESTRUCTOR runs while the program is still alive and its console still attached, so
   it can report - but this is a foreign library and has no console of its own. The consumer installs
   a reporter, and the provider's destructor calls it. That is what makes the destruction ORDER
   visible rather than merely recorded: the consumer's destructor prints, then this one does. */
static void (*reporter)(const char *);

void liber_lifecycle_set_reporter(void (*report)(const char *)) { reporter = report; }

/* THE CONSTRUCTORS AND THE DESTRUCTOR ARE NOT EXPORTED. They are reached through `.init_array` and
   `.fini_array`, which hold their addresses directly - so the surface of this library stays the
   functions above, and a reader of its export table sees what a caller may call rather than what the
   loader will run.

   TWO CONSTRUCTORS, AND THE SECOND ONE DOES NOT FINISH. A mechanism that only ever succeeds has no
   observable failure, and what this milestone has to show is what a PARTIAL initialisation leaves
   behind. The second one sets its marker, does the first half of its work, and returns without
   reaching the step that would clear it - which is exactly the shape a C constructor that cannot
   complete has, since it has no way to report anything to the loader that called it. */
__attribute__((constructor(101))) static void provider_constructed(void) {
	memset(sequence, 0, sizeof(sequence));
	recorded = 0;
	incomplete = 1;
	liber_lifecycle_note('P');
	incomplete = 0;
}

__attribute__((constructor(102))) static void provider_partially_constructed(void) {
	partial = 1;
	liber_lifecycle_note('Q');
	/* The step that would clear `partial` is not reached. Nothing above unwinds: what an
	   initialisation that stopped here leaves behind is precisely the marker and the note. */
	return;
	/* NOTREACHED */
}

__attribute__((destructor)) static void provider_destroyed(void) {
	liber_lifecycle_note('p');
	if (reporter != NULL) {
		reporter("lifecheck: provider destructor");
	}
}
