# LSIDL - LiberSystem Interface Definition Language

LSIDL is the interface definition language for LiberSystem. One `.lsidl` file is
the single source of truth for a service contract: from it the toolchain
generates the binary wire codec, a Rust client and server, a CLI formatter, JSON
and CBOR representations, reference documentation, and compatibility tests.

It belongs to the same family as Android's **AIDL**, macOS/Mach **MIG**, Windows
**MIDL**, and Fuchsia **FIDL** - an IDL written *for one operating system's own
IPC*. LSIDL describes contracts spoken over LiberSystem channels, where messages
carry not just data but kernel **capabilities** (handles with rights) and shared
**DMA buffers**. No off-the-shelf IDL models those primitives, which is why
LiberSystem has its own (see the *IDL language* section of the Concept for the
full rationale).

LSIDL borrows its type vocabulary from WIT (records, enums, variants, results,
lists, tuples, options) and adds three first-class system types WIT lacks:
`handle<T>`, `buffer`, and `stream<T>`.

- Status: design draft for milestone M25.
- File extension: `.lsidl`
- Generator: `lsidl-gen` (host tool), driven by `just gen`
- Generated output: the `proto` crate (`no_std`) plus `docs/gen/`

---

## Table of contents

- [1. Overview](#1-overview)
- [2. Lexical structure](#2-lexical-structure)
- [3. Top-level structure](#3-top-level-structure)
- [4. Types](#4-types)
- [5. Interfaces and methods](#5-interfaces-and-methods)
- [6. Wire layout and ABI](#6-wire-layout-and-abi)
- [7. Code generation](#7-code-generation)
- [8. Versioning and compatibility](#8-versioning-and-compatibility)
- [9. Grammar (EBNF)](#9-grammar-ebnf)
- [10. Worked example: the Log interface](#10-worked-example-the-log-interface)

---

## 1. Overview

A minimal LSIDL file declares a versioned package, some shared types, and one or
more interfaces:

```lsidl
// system.lsidl
package liber:system@1;

enum severity {
	trace, debug, info, warn, error, fatal,
}

enum error {
	denied, not-found, invalid, again, closed,
}

record field {
	key:   string,
	value: string,
}

record entry {
	timestamp: u64,
	severity:  severity,
	source:    string,
	fields:    list<field>,
}

interface log {
	@op(1) emit:  func(e: entry) -> result<unit, error>;
	@op(2) query: func(min: severity) -> result<list<entry>, error>;
}
```

The design goals, in priority order:

1. **Express LiberSystem IPC exactly** - capabilities, shared buffers, and
   streams are first-class, not bolted on.
2. **Decode-cheap, stable wire** - a fixed little-endian layout that a `no_std`
   target can read without allocation; the byte layout is part of the contract,
   not an implementation detail.
3. **One source, many backends** - the same declaration renders as binary, JSON,
   CBOR, CLI text, and docs, so representations never drift from the contract.
4. **Small and legible** - LSIDL is a contract language, not a general-purpose
   serialization framework. If a feature is not needed to describe a system
   service, it is not in the language.

---

## 2. Lexical structure

### Identifiers

Identifiers are **kebab-case**: a lowercase letter followed by lowercase letters,
digits, and single hyphens.

```text
ident      = lower (lower | digit | "-")*
lower      = "a".."z"
digit      = "0".."9"
```

`min-severity`, `file-handle`, and `entry` are valid; `MinSeverity`, `_x`, and
`2fast` are not. The generators map kebab-case to each target's convention -
Rust types become `CamelCase`, Rust fields and methods become `snake_case`.

### Package names

A package name is one or more identifiers joined by `:`, carrying a version:

```text
package-name = ident (":" ident)* "@" version
version      = digit+
```

Example: `liber:system@1`, `liber:storage@1`.

### Comments

```lsidl
// line comment
/* block
   comment */
```

### Annotations

Annotations attach metadata to declarations. They begin with `@`:

| Annotation | Applies to | Meaning |
| --- | --- | --- |
| `@op(n)` | method | the method's stable opcode (required) |
| `@reserved(n)` | interface, enum | a retired opcode or ordinal that must never be reused |
| `@rights(r, ...)` | `handle<T>` param | the minimum capability rights the method requires |
| `@since(v)` | any item | the package version the item was added in (documentation) |

### Keywords

```text
package  use  interface  func
record   enum  variant  flags  resource
handle   buffer  stream
result   option  list  tuple  unit
bool  u8 u16 u32 u64  i8 i16 i32 i64  f32 f64  string
```

---

## 3. Top-level structure

A file contains, in order: exactly one `package` declaration, zero or more `use`
imports, and any number of type and interface declarations (order-independent
among themselves; forward references are allowed).

```lsidl
package liber:storage@1;

use liber:system.{error, severity};

resource file;

record open-opts { /* ... */ }

interface volume { /* ... */ }
```

### `use` imports

`use` brings named items from another package into scope:

```lsidl
use liber:system.{error};          // import the `error` type
use liber:system.{error, severity};
```

Imported names are referenced unqualified. A `use` does not re-export; it only
makes the names visible in the current file.

### `resource` declarations

A `resource` declares an opaque, kernel-backed object type that is never
serialized by value - it only ever crosses the wire as a `handle<T>`:

```lsidl
resource file;
resource process;
```

The kernel's own object types are available as **built-in resources** without a
declaration - most importantly `channel`, which `stream<T>` builds on (a stream is
returned as `handle<channel>`). A package declares its own resources as above and
refers to the built-in ones by name.

---

## 4. Types

### 4.1 Primitive types

| Type | Wire size | Notes |
| --- | --- | --- |
| `bool` | 1 byte | `0` = false, `1` = true |
| `u8 u16 u32 u64` | 1/2/4/8 | unsigned, little-endian |
| `i8 i16 i32 i64` | 1/2/4/8 | signed two's complement, little-endian |
| `f32 f64` | 4/8 | IEEE-754, little-endian |
| `string` | varies | UTF-8, length-prefixed `[len u16][bytes]` |
| `unit` | 0 bytes | the empty type, used for `result<unit, error>` |

### 4.2 Compound types

**`record`** - a product type; fields are encoded positionally, in declaration
order, with no per-record header:

```lsidl
record field {
	key:   string,
	value: string,
}
```

**`enum`** - a closed set of unit-valued cases; encoded as a single `u8` ordinal.
Ordinals start at 0 and follow declaration order, or may be pinned explicitly for
wire-critical enums:

```lsidl
enum severity {
	trace = 0, debug = 1, info = 2, warn = 3, error = 4, fatal = 5,
}
```

**`variant`** - a tagged union; each case may carry a payload. Encoded as
`[tag u8][payload]`, where the payload is the case's type (absent for payload-less
cases):

```lsidl
variant value {
	none,
	integer(i64),
	text(string),
	bytes(list<u8>),
}
```

**`flags`** - a bitset; encoded as the smallest unsigned integer that covers the
declared flags (<= 8 -> `u8`, <= 16 -> `u16`, ...):

```lsidl
flags open-mode { read, write, create, truncate }
```

**`option<T>`** - `[present u8][T if present]`.

**`list<T>`** - `[count u16][elem...]`, elements encoded in order.

**`tuple<T1, T2, ...>`** - the elements in order, no header.

**`result<T, E>`** - `[is-ok u8][T]` when ok, `[is-ok u8 = 0][E]` when error.

### 4.3 System types

These three types are the reason LSIDL exists; they do not appear in WIT.

**`handle<R>`** - the transfer of a kernel capability to a resource `R`. A handle
is **not** part of the byte stream. Instead, the message carries the capability
out-of-band; in the byte stream a `handle<R>` is a `u32` placeholder. On receipt
the kernel installs the capability - with its rights and badge intact - in the
receiver's handle space. This is the same split between data and handles that
Binder, Mach ports, and FIDL use, and it is how a capability keeps its unforgeable
authority across the boundary.

The generator implements `handle<R>` today, with one restriction set by the
kernel channel: **at most one handle travels per message** (the channel's single
out-of-band slot). Encoding a `handle<R>` calls `set_handle` on the writer and
writes the `u32` placeholder; the dispatch/client glue threads that single handle
through `Transport::call(request, request_handle) -> (reply, reply_handle)`, and
decoding recovers it with `take_handle` (ignoring the placeholder). A future
multi-handle message would reintroduce the index-table form; the placeholder is
already a `u32` so that change is wire-compatible.

```lsidl
interface volume {
	@op(1) open: func(o: open-opts) -> result<handle<file>, error>;
	@op(2) read: func(@rights(read) f: handle<file>, into: buffer, len: u32)
		-> result<u32, error>;
}
```

`@rights(...)` documents and enforces the minimum rights the method needs on that
handle; the generated server rejects an under-privileged handle with
`error::denied` before dispatch.

**`buffer`** - a shared, zero-copy memory region backed by a DMA buffer object
(`SYS_DMA_BUFFER_CREATE` / `_MAP` / `_PHYS`). The bytes are never copied into the
message; the stream carries only a descriptor `[dma-id u32][offset u64][len u64]`
that the receiver maps. `buffer` is how bulk I/O (disk sectors, network frames)
moves without per-message copies.

**`stream<T>`** - a backpressured sequence of `T`. A method returning `stream<T>`
replies with a `handle<channel>` to a freshly created sub-channel; the producer
sends elements (each framed as `[seq u32][T]`) and the consumer drains them with
ordinary channel receives. Flow control is the bounded channel itself: when the
channel is full the producer's `wait` blocks until the consumer drains, so there
is no unbounded buffering. This matches LiberSystem's existing wait-drained
channel semantics rather than an async runtime.

---

## 5. Interfaces and methods

An `interface` is one protocol spoken over one channel. Each method is a request
or reply pair identified by an explicit opcode.

```lsidl
interface log {
	@op(1) emit:  func(e: entry) -> result<unit, error>;
	@op(2) query: func(q: query) -> result<list<entry>, error>;
	@op(3) tail:  func(q: query) -> result<stream<entry>, error>;
}
```

- A method takes zero or more named parameters and returns exactly one type,
  conventionally a `result<T, error>`.
- A method that returns nothing meaningful uses `result<unit, error>`.
- The opcode `@op(n)` is **mandatory** and must be unique within the interface.
  Opcodes are declared, never derived from position, so reordering or inserting
  methods never shifts the wire (see [Versioning](#8-versioning-and-compatibility)).

---

## 6. Wire layout and ABI

### Message framing

Every method call is a request frame answered by a reply frame on the same
channel. Correlation ids let multiple calls be in flight at once.

```text
request = [ op u16 ][ corr u32 ]( arg... )      args in declared order
reply   =           [ corr u32 ]( result )      the method's return type
```

- `op` - the method's `@op(n)`, little-endian.
- `corr` - a client-assigned correlation id, monotonic per channel; the reply
  echoes it so the client can match it to the pending call.
- Arguments are encoded back-to-back using the per-type rules in
  [section 4](#4-types); the result is encoded the same way.

A generated client call is therefore: encode `[op][corr][args]`, `channel_send`,
`wait`, `channel_recv`, match `corr`, decode the result.

### Encoding summary

All multi-byte integers are little-endian. There is no padding and no alignment;
the layout is exactly as written.

| Type | Encoding |
| --- | --- |
| `bool` | `u8` (0 / 1) |
| `uN` / `iN` | N/8 bytes, little-endian |
| `fN` | IEEE-754, little-endian |
| `string` | `[len u16][utf-8 bytes]` |
| `enum` | `[ordinal u8]` |
| `flags` | smallest covering `uN` bitset |
| `option<T>` | `[present u8][T?]` |
| `list<T>` | `[count u16][T...]` |
| `tuple<...>` | elements in order |
| `record` | fields in declaration order |
| `variant` | `[tag u8][payload?]` |
| `result<T,E>` | `[is-ok u8][T or E]` |
| `handle<R>` | `[placeholder u32]`; the capability rides the message's out-of-band handle slot |
| `buffer` | `[dma-id u32][offset u64][len u64]` |
| `stream<T>` | returned as `handle<channel>` |

`string` and `list` length prefixes are `u16`, capping either at 65535 elements
or bytes per field - the same limit the current hand-written log codec uses.
Larger payloads belong in a `buffer`, not inline.

---

## 7. Code generation

`just gen` runs `lsidl-gen` over every `src/idl/*.lsidl` and writes:

| Output | Destination | Contents |
| --- | --- | --- |
| binary codec | `proto` crate (`no_std`) | `encode` / `decode` for every type |
| Rust client | `proto` crate | one method per `func`, returning a decoded result |
| Rust server | `proto` crate | a trait plus an opcode `dispatch` the service implements |
| JSON / CBOR | `proto` crate | the same value rendered as JSON or CBOR |
| CLI formatter | `proto` crate | human-readable rendering of a request or record |
| reference docs | `docs/gen/<package>.md` | per-interface tables, opcode list, type layouts |
| compat tests | `proto` crate tests | golden wire bytes per type and opcode |

Name mapping: kebab-case identifiers become `CamelCase` for Rust types and
`snake_case` for Rust fields and methods (`min-severity` -> `min_severity`,
`file-handle` -> `FileHandle`).

The generated `proto` crate is `no_std` so the kernel and userspace components can
link it. `lsidl-gen` itself is an ordinary host binary (it may use `std`).

The per-interface docs under `docs/gen/` are **generated** - they are not edited
by hand. This file (`docs/LSIDL.md`) is the hand-written language spec; the two
must not be confused.

---

## 8. Versioning and compatibility

The wire layout is part of the contract, so LSIDL makes breaking it loud and
deliberate.

**Stable by construction:**

- Opcodes are explicit. **Never** reuse or renumber an `@op(n)`. To retire a
  method, delete it and mark its opcode `@reserved(n)` so it can never come back
  with a different meaning.
- Enum ordinals are fixed. Append new cases at the end; never renumber. Retire a
  case with `@reserved(n)`.
- Records are positional and have no length header, so **adding a field to an
  existing record is a breaking change**. Either add it as a new type/method, or
  bump the package version.

**The package version** (`@N`) is the ABI generation. Bump it on any breaking
change. A client and a service that disagree on the major version do not
interoperate; the generated handshake refuses the mismatch.

**Compatibility tests** capture golden wire bytes for each type and opcode. Any
change that shifts the bytes fails the test, forcing an explicit version bump
rather than a silent break.

> Forward-compatible *extensible records* (a length-prefixed record whose unknown
> trailing fields are skipped by older decoders) are a possible future addition.
> The MVP keeps records fixed and positional, matching the existing log codec.

---

## 9. Grammar (EBNF)

```ebnf
file        = package use* item* ;

package     = "package" package-name ";" ;
package-name= ident ( ":" ident )* "@" version ;
version     = digit+ ;

use         = "use" package-path "." "{" ident ( "," ident )* "}" ";" ;
package-path= ident ( ":" ident )* ;

item        = annotation* ( record | enum | variant | flags | resource | interface ) ;

record      = "record" ident "{" field ( "," field )* ","? "}" ;
field       = annotation* ident ":" type ;

enum        = "enum" ident "{" enum-entry ( "," enum-entry )* ","? "}" ;
enum-entry  = annotation* ident ( "=" digit+ )? | reserved ;

variant     = "variant" ident "{" var-case ( "," var-case )* ","? "}" ;
var-case    = annotation* ident ( "(" type ")" )? ;

flags       = "flags" ident "{" ident ( "," ident )* ","? "}" ;

resource    = "resource" ident ";" ;

interface   = "interface" ident "{" ( method | reserved ";" )* "}" ;
method      = annotation* ident ":" "func" "(" params? ")" "->" type ";" ;
params      = param ( "," param )* ;
param       = annotation* ident ":" type ;

reserved    = "@reserved" "(" digit+ ")" ;

type        = prim
            | "option" "<" type ">"
            | "list" "<" type ">"
            | "tuple" "<" type ( "," type )* ">"
            | "result" "<" type "," type ">"
            | "handle" "<" ident ">"
            | "stream" "<" type ">"
            | "buffer"
            | ident ;                 (* a named record/enum/variant/flags *)

prim        = "bool" | "u8" | "u16" | "u32" | "u64"
            | "i8" | "i16" | "i32" | "i64"
            | "f32" | "f64" | "string" | "unit" ;

annotation  = "@" ident ( "(" arg ( "," arg )* ")" )? ;
arg         = ident | digit+ ;

ident       = lower ( lower | digit | "-" )* ;
lower       = "a".."z" ;
digit       = "0".."9" ;
```

---

## 10. Worked example: the Log interface

The Log interface is LiberSystem's first real LSIDL contract. Its canonical
record encoding is defined to reproduce the existing hand-written `abi::log` wire
layout **byte for byte**, so `LogService` can switch to the generated codec
without changing a single stored byte.

### 10.1 `src/idl/system.lsidl`

```lsidl
package liber:system@1;

// A common error, rendered identically in binary / JSON / CLI. Each case names a
// kernel ERR_* condition, but the wire value is the enum's own 0-based ordinal
// (denied = 0, not-found = 1, ...), not the negative ERR_* number.
enum error {
	denied,     // ~ ERR_ACCESS_DENIED
	not-found,  // no such object
	invalid,    // ~ ERR_INVALID
	again,      // ~ ERR_WOULD_BLOCK
	closed,     // ~ ERR_PEER_CLOSED
}

enum severity {
	trace = 0, debug = 1, info = 2, warn = 3, error = 4, fatal = 5,
}

record field {
	key:   string,
	value: string,
}

record entry {
	timestamp: u64,          // monotonic ns
	severity:  severity,
	source:    string,
	fields:    list<field>,
}

record query {
	since:        option<u64>,
	min-severity: option<severity>,
	source:       option<string>,
	limit:        u32,
}

interface log {
	@op(1) emit:  func(e: entry) -> result<unit, error>;
	@op(2) query: func(q: query) -> result<list<entry>, error>;
	@op(3) tail:  func(q: query) -> result<stream<entry>, error>;
}
```

### 10.2 The `entry` record on the wire

`entry` encodes exactly as the current `abi::log` record:

```text
[ timestamp u64 ][ severity u8 ][ source_len u16 ][ source bytes ]
[ field_count u16 ]( [ key_len u16 ][ key ][ val_len u16 ][ val ] )*
```

For `entry { timestamp: 42, severity: info, source: "kernel", fields: [] }`:

```text
2a 00 00 00 00 00 00 00   timestamp = 42
02                        severity  = info (2)
06 00                     source_len = 6
6b 65 72 6e 65 6c         "kernel"
00 00                     field_count = 0
```

### 10.3 A generated `emit` call

The generated client wraps that record in the request envelope:

```text
01 00                     op   = 1 (emit), little-endian u16
07 00 00 00               corr = 7
<entry bytes from 10.2>   the e: entry argument
```

`LogService`'s generated server matches `op = 1`, decodes the `entry`, stores the
canonical bytes, and replies `[ corr u32 ][ is-ok u8 = 1 ]` for
`result<unit, error>`.

### 10.4 The same call as other backends

From the one `entry` value the generators also produce, via `to_text` and
`to_json`:

```text
CLI : {timestamp=42, severity=info, source=kernel, fields=[]}
JSON: {"timestamp":42,"severity":"info","source":"kernel","fields":[]}
```

One declaration, one canonical value, many representations - which is the whole
point of having LSIDL.
