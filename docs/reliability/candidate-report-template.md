# Candidate qualification report

Copy the blank template for each candidate SHA. Fill every row with **PASS**, **FAIL**, **PARTIAL**, or **NOT RUN**. Do not merge tiers into one result.

Chris decides live acceptance and deployment. A report can recommend GO or NO-GO. It cannot authorize either. Implementation agents do not self-authorize a live GH Enterprise or SVN run, production access, or deployment.

Rules that apply to every report:

- Name the exact review base SHA, the candidate head SHA, and the artifact digest. A later head is a different candidate.
- #62's matrix and #63's upgrade/restore fixtures must already be PASS at that same head before any active-environment acceptance. Until the installed version is known, report that version's migration coverage as **NOT RUN**.
- Both directions need content and provenance assertions. Data-integrity failures are NO-GO even if the aggregate error rate is fine.
- Untested optional behavior stays disabled and unqualified: cancellation/restart, late pairing, rewrite containment, verified snapshot initialization, coordinated refresh, normal sync after upgrade, credential inheritance/rotation.
- After new Git or SVN writes, recovery is stop, reconcile, and roll forward. No blind watermark reset and no old database restore.
- local/offline PASS is not enterprise/live PASS.
- Historical offline or dry-run evidence, including February 2026, is not PASS for a new SHA.

The runbook is [`docs/enterprise-soak-runbook.md`](../enterprise-soak-runbook.md). Tier 3 network steps, if ever approved, are [`docs/ghe-live-validation-guide.md`](../ghe-live-validation-guide.md).

## Blank template

```text
STATUS: DRAFT | READY_FOR_CHRIS | NO-GO
EPIC / ISSUES: Refs #41. Refs #61. (does not close either)

REVIEW BASE SHA:
CURRENT HEAD SHA:
ARTIFACT DIGEST:
GOAL.md SHA-256: 16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8

INSTALLED VERSION:
INSTALLED CONFIGURATION SOURCE: (how it was obtained, without secrets)
MIGRATION COVERAGE FOR THAT VERSION: PASS | FAIL | PARTIAL | NOT RUN

HOST / TOOL VERSIONS:
PERMISSIONS USED: (dedicated disposable credentials, or NOT RUN)
ALLOWLIST: (target identity, or NOT RUN)
CHRIS/ADMIN APPROVAL: (reference, or NOT RUN)

TIER OUTCOMES (do not combine):
- Tier 1 local engine / enterprise-soak.sh file://: PASS | FAIL | PARTIAL | NOT RUN
- Tier 2 local HTTP-provider / offline self-tests: PASS | FAIL | PARTIAL | NOT RUN
- Tier 3 disposable allowlisted enterprise: PASS | FAIL | PARTIAL | NOT RUN
- Tier 4 active-production acceptance: PASS | FAIL | PARTIAL | NOT RUN

#62 MATRIX AT THIS HEAD: PASS | FAIL | PARTIAL | NOT RUN
#63 UPGRADE/RESTORE FIXTURES AT THIS HEAD: PASS | FAIL | PARTIAL | NOT RUN

BOTH-DIRECTION CONTENT AND PROVENANCE: PASS | FAIL | PARTIAL | NOT RUN
DATA INTEGRITY: PASS | FAIL | NOT RUN
  (any FAIL is NO-GO even if error rate is acceptable)

OPTIONAL FEATURES (untested => disabled and unqualified):
- Cancellation/restart:
- Late pairing:
- Rewrite containment:
- Verified snapshot initialization:
- Coordinated refresh:
- Normal sync after upgrade:
- Credential inheritance/rotation:

BACKUP / RESTORE / ROLL-FORWARD REVIEWED: PASS | FAIL | NOT RUN
PRODUCTION WRITERS QUIESCENT: PASS | NOT RUN | NOT APPLICABLE
ONE AUTHORITATIVE WRITER: PASS | FAIL | NOT RUN
SYNTHETIC COMMITS IN ACTIVE REPOS: NOT DONE | SEPARATELY APPROVED

KNOWN LIMITATIONS:

RECOMMENDATION FOR CHRIS: GO | NO-GO
RATIONALE:
CHRIS DECISION: PENDING | GO | NO-GO
```

## Worked example

**ILLUSTRATIVE / NOT a live run.** This block shows the shape of a report for the post-#86 main tip named below. It is not soak evidence, not a measurement, and not a GO. Replace every field when a real candidate is evaluated. Do not cite this example as qualification of that SHA or of any later pull-request head.

```text
STATUS: NO-GO
EPIC / ISSUES: Refs #41. Refs #61. This example does not close either.

REVIEW BASE SHA: de9ef4725e314e1cec62bb8378bceaf091a07e7f
CURRENT HEAD SHA: de9ef4725e314e1cec62bb8378bceaf091a07e7f
ARTIFACT DIGEST: NOT RUN — no release artifact was hashed for this illustration
GOAL.md SHA-256: 16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8

INSTALLED VERSION: NOT ESTABLISHED
INSTALLED CONFIGURATION SOURCE: NOT RUN
MIGRATION COVERAGE FOR THAT VERSION: NOT RUN

HOST / TOOL VERSIONS: NOT RUN
PERMISSIONS USED: NOT RUN — no enterprise credentials used
ALLOWLIST: NOT RUN
CHRIS/ADMIN APPROVAL: NOT RUN

TIER OUTCOMES (do not combine):
- Tier 1 local engine / enterprise-soak.sh file://: NOT RUN
- Tier 2 local HTTP-provider / offline self-tests: NOT RUN
- Tier 3 disposable allowlisted enterprise: NOT RUN
- Tier 4 active-production acceptance: NOT RUN

#62 MATRIX AT THIS HEAD: NOT RUN in this illustration
  (a host catalog check of the matrix files is not the isolated #62 runner)
#63 UPGRADE/RESTORE FIXTURES AT THIS HEAD: NOT RUN

BOTH-DIRECTION CONTENT AND PROVENANCE: NOT RUN
DATA INTEGRITY: NOT RUN

OPTIONAL FEATURES (untested => disabled and unqualified):
- Cancellation/restart: NOT RUN — leave disabled where unqualified
- Late pairing: NOT RUN — leave disabled where unqualified
- Rewrite containment: NOT RUN — leave disabled where unqualified
- Verified snapshot initialization: NOT RUN — leave disabled where unqualified
- Coordinated refresh: NOT RUN — leave disabled where unqualified
- Normal sync after upgrade: NOT RUN — leave disabled where unqualified
- Credential inheritance/rotation: NOT RUN — leave disabled where unqualified

BACKUP / RESTORE / ROLL-FORWARD REVIEWED: PARTIAL — procedures are documented;
  no restore drill was executed for this illustration
PRODUCTION WRITERS QUIESCENT: NOT APPLICABLE — no production change
ONE AUTHORITATIVE WRITER: NOT RUN
SYNTHETIC COMMITS IN ACTIVE REPOS: NOT DONE

KNOWN LIMITATIONS:
- Illustration only. February 2026 offline/GHE dry-run notes are historical
  and are not PASS for de9ef4725e314e1cec62bb8378bceaf091a07e7f.
- local/offline PASS is not enterprise/live PASS.
- Installed-version migration coverage remains NOT RUN until that version is known.

RECOMMENDATION FOR CHRIS: NO-GO
RATIONALE: Enterprise and live tiers are NOT RUN. Migration coverage for the
  installed version is NOT RUN. No artifact digest is bound to this SHA.
  Data-integrity evidence was not collected. Agents do not self-authorize.
CHRIS DECISION: PENDING
```
