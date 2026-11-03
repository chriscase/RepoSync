# Import and snapshot confinement and independent verification (#88)

The shared `import::copy_tree_with_policy` is used by full-history import,
team snapshot materialization and Personal imports/synchronization. It admits
an export root with no-follow metadata and inventories the entire source before
its first destination write. `remove_stale_files` performs the same preflight
before removal. Symlinks (including dangling, directory and loop entries),
non-regular special nodes and multiply linked regular files refuse. Reserved
VCS metadata `.git`, `.svn`, `.hg` and `.bzr` is deliberately excluded at every
depth; `CopyStats.exclusions` records each exclusion and policy-skipped file.
Ordinary dotfiles and nested dot-directories are not blanket exclusions.
Unsupported input is not silently interpreted as an empty or partial tree.

Source expectations are `independent_tree_manifest` facts, not the output of a
second copier invocation. A policy projection retains independent source paths,
regular-file lengths, SHA-256 bytes and owner-execute mode. Unrepresentable
non-UTF8 paths refuse instead of aliasing through lossy text. Unix backslash
names remain literal; separators come from path components. Directory modes are
inventoried but are not a Git content contract. Empty directories are checked in
the workdir and naturally absent from a committed Git tree.

`verify_export_projection` checks the complete destination after full-export
copy/stale removal and before commit in full-history and Personal paths. Shared
snapshot verification independently re-exports the already-pinned revision.
`project_snapshot_tree` now retains that raw export (including policy-excluded
inputs); it does not copy another expected workdir. Both team and Personal
snapshot callers retain their existing API and use the same verifier. Git
post-commit verification reads actual commit objects, checks canonical regular
file modes and fails on unreadable/missing objects, non-UTF8 names, symlink or
submodule entries. Dirty workdir changes cannot substitute for commit evidence.
The pin, reconcile finish and publication/receipt machinery are unchanged.

## LFS and generated attributes

Workdir payload hashes remain the original exported file bytes. Committed LFS
files must be pointers whose SHA-256 OID and size match those independent bytes;
a fat blob or wrong pointer refuses. Root `.gitattributes` is the sole generated
file exception: existing engine-owned tracking metadata under `.git` records
historical tracking rules, and every currently required rule is independently
derived from source paths/size and the active policy and must be present. The
expected generated body is computed from those rules and exported attributes,
never read back from the materialized `.gitattributes` as its own oracle.
Export attributes without current LFS generation retain their exact bytes.
Generated attributes are ordinary text in Git, not an LFS payload. Attempting
to LFS-track the source root `.gitattributes` itself explicitly refuses.
This extends existing projection checks; it does not introduce a new attribute
interpreter, external filter policy or LFS transport implementation.

## Acceptance evidence

Retained tests cover source/destination file/directory symlinks, dangling and
loop links, FIFO targets, hardlinks, reserved metadata, executable and binary
files, root/nested dotfiles and the historical double-copy canary. New tests
exercise the production verifier itself, unchanged destination on whole-source
refusal, owner-execute semantics, unexpected destination bytes, literal paths,
and unsupported committed types. The real pinned-SVN snapshot regression
checks raw policy inputs, exported bytes/mode, a real Git commit, HEAD advancing
after the pin and independent mismatch refusal. Existing Personal LFS, cycle,
snapshot and cancellation/reconcile controls must be qualified on the exact
candidate, with counts and raw logs recorded separately.

Canary assertions prove the tested invalid nodes are refused and their bytes
are not part of an admitted manifest/destination. They do not imply every
filesystem access or concurrent hostile same-UID rename was instrumented.
The copier targets managed export/workdir roots; this is not an arbitrary
same-UID sandbox or a Windows race-confinement qualification. Partial failures
remain failures and are not upgraded by later runs. #68, enterprise soak and
frozen #69 recovery/collection qualification remain separate.

The historical root `GOAL.md` remains absent. Tracked
`docs/reliability/GOAL.md` retains SHA-256
`16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
