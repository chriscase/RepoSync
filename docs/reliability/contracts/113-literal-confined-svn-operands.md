# Literal and confined SVN operands (#113)

Repository names are data, never command options, peg expressions, helper
configuration, or filesystem authority. The supported ordinary bridge uses
UTF-8 relative components. Spaces, Unicode, XML metacharacters, leading hyphens
and literal `@` remain exact. Empty components, dot/parent components, absolute
paths, backslashes, drive syntax, VCS administrative components and control
characters refuse before the projected write set is mutated. Installed SVN
also rejects tab/newline names with E160005; these are negative cases, not
silently normalized names or claimed supported publications.

## Command and URL boundary

`svn::operand` owns repository-relative validation, URL component encoding and
peg construction. Raw filename components are URL-encoded once. A literal
working-copy target gets the empty peg suffix; a historical source gets an
explicit positive revision. Configured URL authority is validated separately;
query/fragment, embedded passwords and discarded or nonliteral URL components
are refused. `cat` cannot switch out of its configured source subtree.

Every client command places authentication and private configuration options
before the operand separator. Typed command methods put only data after `--`.
The internal WC compatibility entry rebuilds its supported verb's option,
property-value and target roles separately, rejects caller overrides of client
credentials/configuration/helpers, and preserves the pinned merge source. `merge`'s destination is instead a plain
validated WCPATH after `--`: SVN does not parse a peg there, so an empty peg
suffix must not be appended to that role.
The two web SVN connection probes reuse the same client boundary. Configured
branches use `configured_branch_url` in import/reimport, setup import, scheduler,
reconciliation and late-pair clients. These are raw names, not encoded URL
suffixes: `a%40b` means that literal name and differs from `a@b`. Empty and `/`
select the root, and a single leading `/` is the existing root-relative spelling.
Import callers retain their existing empty-branch default of `trunk`.

Repository-root containment is checked in the URL domain before the branch
suffix is decoded, one validated component at a time, into the literal XML log
namespace. Encoded separators/dot components and foreign roots refuse. Literal
percent data is not decoded again. Unrepresentable log names refuse observation
instead of becoming an empty successful change set. Journal reconciliation does
not reinterpret a retained namespace that differs from the checked live target.

URL validation errors omit the original URL. Constructor/span/Debug diagnostics
never expose unvalidated userinfo or query/fragment text, and public probe errors
use shared redaction. Password refusal remains before launch. The stricter
registered refresh execution contract still validates its original canonical
ASCII-only endpoint policy before ordinary URL encoding; #113 does not admit
previously unsupported refresh authority or make read-only pins executable.

No shell interpolation is introduced. The #228 commit-message path is retained:
owner-only private file input is attached to stdin for `--file -`, remains alive
through process completion, and is not converted back to argv message data.

## Working-copy boundary

Before the first content mutation, team and Personal apply preflight the whole
projected destination set. `ConfinedWorkingCopy` holds a directory descriptor
and walks parents with no-follow directory opens. A regular-file target must
not be a symlink, special node, directory or multiply-linked file. Writes use a
fresh regular inode and descriptor-relative publication, never write through a
planted link, and recheck the destination. New parent creation and owned cleanup
also use descriptor-relative operations. Unsupported non-Unix mutation backends
refuse; this does not assert Windows qualification.

The anchor is the caller's managed working-copy directory, not an arbitrary
path supplied by Git. This is not a new host sandbox, ownership controller or
proof of recovery-process collection. The shared import/snapshot copier and
its independent manifest are unchanged; this does not close #88.

Staging uses explicit included paths and shallow ancestor registration.
`add_with_retry` does not recursively add neighboring unversioned files.
Personal replay refuses pre-existing tracked dirt and newly observed tracked
changes outside its owned projection. Unknown files are not staging authority.
Production staging, pending-change and abort-cleanliness decisions use XML
status entries. Ordinary log/diff path parsing decodes XML without trimming
filename whitespace or splitting a name into lines. Unsupported/conflicted
status entries refuse rather than become an empty successful observation.
Declared svn:externals and svn:special refuse before mirrored content
mutation/export/publication. Ignoring externals alone is not treated as
successful handling of omitted work. Other property projection obligations
remain with the existing changeset contract; this is not a new translator.

## Qualification and recovery

The native client regressions cover ordinary controls, `@`, option-like names,
spaces, Unicode, XML metacharacters, nesting, selected historical reads,
revert and rename/delete; negative sets include tab/newline, traversal,
administrative names, links and hardlinks. Existing #62 qualified-pair and
Personal fixtures exercise the actual apply consumers. Exact command results,
publication, receipt and checkpoint assertions are required; inventory and
source review alone are not executed acceptance. The branch regression uses
independent histories for full import: the existing copied-branch historical
path-relocation limitation is not claimed repaired by this operand story.

Outbound publication records the exact emitted SVN revision in its applied
receipt and advances the Git cursor. It does not invent advancement of the
separate inbound SVN cursor. A refusal must not create a false applied receipt
or advance those checkpoints. Repeats must preserve the published result.

On unsupported input, preserve the work and correct/review the source path or
projection. Do not delete metadata, reset a checkpoint, force-push or broaden
credentials. Existing durable intent/reconciliation behavior governs an
uncertain remote effect; this story does not authorize blind replay or release
an existing hold. Main promotion, deployment, #69's frozen collector work and
its separate execution/host qualification remain outside this delivery.
