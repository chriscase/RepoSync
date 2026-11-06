# Keyboard and assistive-technology verification (#121)

The shared native `Dialog` supplies an accessible title and description, inert background, explicit forward/reverse Tab wrapping, conservative initial focus and a busy Escape/Cancel policy. A control becoming disabled cannot leave focus outside the modal. Closing returns focus to the connected opener; when the action removes that opener, the current main landmark is the fallback. Closing a retired dialog does not steal focus from a replacement route/session. Pending request presentation does not prove durable operation completion or release a recovery hold.

RepoDetail's Skip, pairing preview, pause/disable, branch removal and managed removal dialogs, repository creation and admin user dialogs reuse this primitive. Existing request ownership, preview/refusal and remote-deletion guards remain authoritative. Branch navigation and audit disclosure use actual links/buttons. Reusable and page-specific form labels are associated with their controls; field-specific revision errors and generic form errors have descriptions/alerts. Conflict view buttons expose their selected state, and the manual editor has a label. Manual resolution sends the edited content through the already-supported API argument.

Import meters expose bounded reported revision progress and state; unsupported counters remain indeterminate. Their polite status changes only with lifecycle/phase or a ten-percent bucket, not every revision or log update. Logs are explicitly non-live and remain inspectable. Reduced-motion preference disables animation, transitions and smooth scrolling. Contrast corrections preserve the underlying status, configuration and action data.

## Reproducible local profile

Use synthetic sessions only. No real accounts, server URLs, credentials or repository effects are needed. Do not change browser/OS permissions, disable the browser sandbox, install a browser, or bypass a failed execution profile.

From `web-ui`, with the locked dependencies installed:

```sh
npm run typecheck
npm run lint
npm test -- --reporter=verbose
node --test node-tests/receipt-compatibility.test.mjs
npm run build
REPOSYNC_A11Y_BROWSER='/path/to/an/existing/approved/Chrome' npm run test:a11y
```

The last command starts owned loopback fixture/Vite servers and a fresh headless context in the specified existing browser, with `chromiumSandbox: true`. It blocks non-loopback browser requests, exercises the real React routes and uses pinned axe-core. Setup and each case have explicit deadlines; teardown closes the owned browser and servers. It does not download a browser, contact a production API or run a RepoSync engine. A failed launch or missing executable is a failed gate, never a skipped pass. The fixture reports synthetic errors/completion receipts solely to check UI ownership, focus and error paths; it does not qualify real cleanup, publication or recovery.

Ten browser cases cover keyboard-only traversal after synthetic login: managed-removal focus/Escape/busy/error/return; branch links and typed confirmation with remote choices initially off; conflict manual editing/submission and deferral; repository creation plus native progress/reduced motion/non-live logs; and a synthetic completed removal whose opener is gone. The sixth case audits settings, admin-user creation, LDAP, configuration and setup forms without submitting account/credential changes. Three correction regressions exercise separate Git/SVN validation failures and recovery, synthetic LDAP/notification save/test failures with retained inputs, and distinct selected usernames with equal display names/email through editing, pending save and focus return. The tenth verifies the rendered Skip dialog marker, labelled purpose, initial selection refusal, Escape/focus return, exact selected-prefix/tip request and no further request when scoped status changes to held or unknown. All writes reach only the in-memory fixture. Axe checks actual modal/editor/progress/error states without disabling contrast or label rules. Automated checks find only some accessibility defects and do not replace a screen-reader pass or a whole-product accessibility certification.

## Required actual screen-reader pass

This is a separate acceptance gate. ARIA markup, an accessibility-tree snapshot, SSR, axe and keyboard automation are not a screen-reader pass.

An authorized tester with an already available screen reader can run `npm run test:a11y:serve` in `web-ui`. It binds only `127.0.0.1:39731`, prints the synthetic fixture address, and stops on Ctrl-C or after 15 minutes. If the port is busy, it refuses instead of disturbing another service. Open the printed UI in the supported browser, sign in as `fixture-admin` / `synthetic-only`, and record the exact source SHA, browser/OS/screen-reader versions and observed announcements:

1. Visit `/repos/C`, find Remove from RepoSync by keyboard, open it and confirm the title/purpose and safe initial Cancel focus are announced. Traverse both ways with Tab, read the dependency information, and Escape. Confirm focus returns to its opener.
2. Visit `/repos/A`. Find the Synthetic branch link and navigate with Enter. Return, open Delete branch pair, identify each default-off remote-deletion checkbox and the labelled typed confirmation. Cancel without deleting.
3. Visit `/conflicts/conflict-1`, select Manual Edit, identify the labelled content and selected view, edit only the synthetic text, and defer. Confirm the action/error feedback is announced and navigation remains usable.
4. Visit `/`. Identify the Synthetic parent import meter and its current value/state. Verify coarse status changes are announced while ordinary log lines are not repeatedly announced. Inspect the logs deliberately. Verify reduced-motion presentation if that preference is already available in the admitted test profile.
5. Open Add Branch Pair on `/repos/A`. Enter invalid Git and SVN values separately, identify each control's associated explanation, and correct it without submitting a preview. Visit `/ldap` and `/config` to verify the fixture's refused save/test feedback is announced and values remain available. These responses cannot change real configuration.
6. Visit `/users`, open Edit for `fixture-alpha` and `fixture-beta` (which share display name/email), and verify the immutable username is announced and remains clear while editable values change. Cancel without touching real accounts.

Record PASS/FAIL with observations rather than checklist intent. Any missing reader access, unavailable announcements, unexpected permission prompt or interrupted UI transport leaves this gate OPEN. Do not enable scripting, grant accessibility/privacy permissions or change security settings to manufacture evidence. Preserve the baseline and restore any specifically authorized temporary reader state after the pass.

Standard hosted Rust CI has no npm accessibility step. Its result must remain separate from exact-source UI, browser and screen-reader evidence. Independent source review, exact-head CI and post-merge CI also remain distinct gates. No deployment, main promotion, real-identity-provider qualification, frozen #69 activation or whole-epic completion is implied.
