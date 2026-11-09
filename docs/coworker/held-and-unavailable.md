# Held and unavailable

*Back to the [guide index](README.md).*

This page lists exactly what the [#71](https://github.com/chriscase/RepoSync/issues/71)
acceptance criteria ask for that this documentation outcome does **not**
deliver, why, and what is safe to do meanwhile. Issue #71 stays open. Nothing
here is split off or closed, and nothing below is described as implemented.

The documentation outcome covers: Git-centric workflows for ordinary sync, late
pairing, refresh requests, status and reconciliation; the SVN-origin rule with a
valid and an invalid example; the branch-policy recommendation; and a
current-versus-planned FAQ. The items below are the remainder.

## The held remainder

| #71 criterion | Status here | Depends on | Safe next step |
| --- | --- | --- | --- |
| An **opt-in, manually triggered workflow or client** that requests a preview and an approved operation through the authenticated API, pinning repo and pair IDs, expected tips or plan identity, and an idempotency key, and reports the actual result | **Held.** This guide documents the underlying API calls directly and does not describe a qualified client. | The live refresh request and status integration depends on the **frozen #69** design, which this guide cannot establish. | Use the read-only previews in [Requesting a refresh](refresh.md) through an administrator. |
| **Webhook handlers** that validate authenticity, authorize scope, deduplicate event IDs, and tolerate stale, out-of-order and missing events; `forced`, `before` and `after` as hints only | **Held.** Receivers exist and treat `forced` as a hint, but they check authenticity only if a secret happens to be configured, and track no delivery IDs. See [Ordinary sync](ordinary-sync.md#webhooks-that-already-exist). | Frozen #69 for anything that would trigger a refresh. | Don't rely on webhooks for correctness. Polling is what detects a rewrite. Don't create or change a webhook. |
| **Workflow permissions** that are least-privilege and separate from user and admin credentials, with redaction, authenticated status and cancel, and declared network requirements for self-hosted or enterprise environments | **Held** with the workflow. Guidance appears only as cautions in [Branch policy](branch-policy-and-hooks.md#push-triggered-actions). | The workflow above. | Keep operation tokens away from any job that runs pull-request code. |
| **Hook installation and removal** that preserve an existing `core.hooksPath` or hook chain, handle the `pre-rebase` arguments, linked worktrees and offline behavior, and never modify a user's clone automatically | **Held.** There is no installer. The behaviors a future one must handle are described in [Branch policy](branch-policy-and-hooks.md#local-hooks-useful-nudges-not-enforcement). | None beyond design and review. | Don't script changes to other people's clones. |
| **Live application** of the branch-policy recommendation | **Not done, and not part of this work.** | An authorized administrator's decision (Chris or another admin), including any bypass grant. | Review the recommendation, rehearse on a throwaway repository, then decide. |
| The **tests** listed in the issue (authorized request/status/cancel, wrong repo/pair/token, stale approval, repeated request, missing or out-of-order webhook, tampered signature, polling-only detection, untrusted branch input, unavailable service, hooks, protected-ref rejection) | **Not part of this documentation outcome.** No test or build was run for it. | The held code above. | n/a |
| **Exact release support and sandbox evidence links** in the FAQ | **Partly.** The FAQ states plainly that no release exists and links the evidence that does exist. It cannot link a sandbox run of the held pieces, because none is claimed. | A tested release and a qualification run. | Read the [FAQ](faq.md#which-release-supports-this). |

## Unavailable behavior, by name

These are refused or absent today. None has a bypass:

- Executing a refresh through the **v1** route: `refresh_execute_not_implemented`.
- **Re-anchor or recreate** a pair: `reanchor_not_implemented`.
- **Automatic or push-triggered refresh**, and any "auto-rebase the SVN side after a force push".
- A **hook installer or remover**.
- A **request workflow** or **client** this guide stands behind.
- **Hardened webhook** deduplication and out-of-order handling.
- **Live qualification** of any of the above, and **any release number** that supports them.

## The first-slice artifacts already in the tree

An earlier slice of #71 (merged before this guide) left these files in the repository:

- `docs/reliability/git-workflows.md`, `docs/reliability/coworker-FAQ.md` and `docs/reliability/contracts/71-git-operation-requests.md`
- `scripts/git_operation_request.py` with `scripts/test_git_operation_request.py`
- `.github/workflows/git-operation-request.yml` (a `workflow_dispatch` workflow)

This documentation outcome does **not** edit any of them, and does **not** endorse them as the
request path. Specifically:

- The opt-in workflow and client are the held item above. They have not been run with live secrets or a
  live RepoSync, and by their own description they cover only the v1 preview, import and SVN-commit
  status and import cancel. They do not know about the v2 refresh API or about reconciling an SVN commit or a Git push.
- Some of the older prose is out of date against the source; the specifics are in
  [the FAQ](faq.md#where-older-documents-disagree-with-the-source). If you use those documents,
  reconcile them with this guide set first.
- The workflow requires an environment and secrets that this documentation outcome does not create. Nothing
  here asks anyone to create them.

## What this guide set did and did not touch

- It adds files only under `docs/coworker/`.
- It changes no code, test, workflow, configuration, README or other document.
- It performs no live branch-rule, webhook, credential, token, installation or external-service change.
- It does not claim a runtime, full-lab, native, live, deployment or resource-hold release.
