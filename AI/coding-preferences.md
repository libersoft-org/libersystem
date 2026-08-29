# Coding Preferences

## Language
- Reply in the user's language.
- Keep all code, identifiers, comments, strings, documentation and commit messages in English.

## Style
- TypeScript functions require explicit parameter and return types, including callbacks.
- Use brace-free one-line control-flow bodies when they contain one statement.
- Use plain section comments such as `// menu`; do not add decorative padding.
- Never use an em dash. Use a hyphen.
- For 1024-based units write `kB`, `MB`, `GB`, `TB`, `PB`, `EB`, `ZB`.
- Never use `KB` or IEC forms.
- Match local naming, ordering, errors, messages, layout and abstractions.
- Flag inherited inconsistencies.

## Licensing
- LiberSystem code remains under the Unlicense.
- Do not vendor or adapt source under another license.
- External code may inform an independent implementation based on public specifications.

## AI Documents
- AI documents belong only in root `AI/`.
- `AI/` is authoritative, ignored by Git and never uploaded.
- Edit these files directly. Do not use or update legacy memory copies elsewhere.
- Do not store transcripts, debug logs or tool output.
- Outside `AI/`, do not leave AI authorship or workflow traces.
- Legitimate project terms are allowed.
- Never export private system/developer instructions or inaccessible service internals.

## Roadmap References
- Specific roadmap identifiers may appear only in `docs/todo/**` and `NOTES.md`.
- Elsewhere explain the fact directly without citing a roadmap item.
- Roadmap identifiers use four digits and preserve suffixes.
- Examples: `M0001` and `M0035j`.
- After edits, scan permanent files for accidental short or forbidden roadmap references.
- `NOTES.md` is the user's personal file. Never record findings there.
- Record every finding in the `docs/todo/**` milestone that owns it, attached to the concrete item it concerns.
- An unticked item in a milestone file requires unticking that milestone's row in `docs/todo/TODO.md`.
- The rule runs both ways: tick the row again once the last open item closes.
- Prose notes inside a finished item do not change the row. Only real `- [ ]` items do.

## Reviews And Scope
- Analysis and review requests produce chat findings only.
- Edit files only when the user explicitly requests it.
- Fix root causes with minimal scope. Do not fix unrelated failures.
- Keep Rust tests outside production files.
- Use crate-root `src/tests.rs`, module-local `tests.rs`, or separate generated test files.

## Completion
- After substantial validated work, state that it is a good commit checkpoint.
- Do not write commit messages. `commit.sh` generates them from the diff.
- Name one recommended next roadmap task with both its identifier and numbered position.
- The user runs commits; do not commit unless explicitly asked.

## Milestone identifiers: three places, and nowhere else

**Never write a milestone id (`P02M0153`, `P01M0042`, ...) outside these three places:**

1. the milestone documents themselves, under `docs/todo/` - one may reference another, or itself;
2. `TODO.md`;
3. anywhere under `/AI/`.

Not in source comments, not in `build.sh`, `check.sh`, `image.sh`, `lib.sh`, `setup.sh`, not in
`.gitattributes`, `toolchain.lock`, `Cargo.toml`, `INSTALL.md`, `README.md`, `NOTES.md`, not in
`docs/` outside `docs/todo/`, not in gate scripts, not in test names.

**Why.** The milestone documents are TEMPORARY. They are working notes that get tidied away once
their work has landed, and every reference to one from a permanent file becomes a pointer to
something that no longer exists - a comment that cites an authority the reader cannot consult.

**What to write instead.** Keep the fact and drop the label. A comment that says *what was wrong and
why the code is shaped this way* survives the milestone that discovered it; one that says "see
P02M0119" does not. So:

- not `// P02M0119 asks for a test that unmaps the page during the copy`
  but `// The requirement is a test that unmaps the page DURING the copy`
- not `// P02M0144's gate: a warning answered by switching the lint off`
  but `// A warning answered by switching the lint off`
- not `// This is the defect P02M0120 was opened on`
  but `// This is the one unexplained double allocation this allocator was rewritten for`

Requirement ids that are NOT milestones - `KERN-ARCH-017`, `WASM-004`, `FDT-005` and the like - are a
different taxonomy and are fine to cite.
