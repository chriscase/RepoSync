# Captures of the #263 design package v1.6

The publisher rendered the static, synthetic v1.6 prototypes in a browser. This folder holds a representative set of those renders, the results of every capture, and the tool that made them.

**These are pictures of design prototypes, not of RepoSync.** They are not keyboard, screen-reader, zoom or accessibility evidence, and they claim no accessibility pass.

## What was rendered

- **Package:** `../../v1.6/`, whose `SHA256SUMS` has SHA-256 `3824ca81b64bb453a83e446efd39740e89908807f7df60b6c44f1b4465963d61`. A fresh copy was made, and all 287 entries were verified before rendering.
- **Browser:**
  - Chromium 153.0.8010.12 (build 1243), headless, with its sandbox enabled.
  - Driven by playwright-core 1.63.0.
  - Run as a non-root user with a private home and temporary directory.
  - Every request outside the package directory was aborted and recorded.
  - Device scale factor 1; reduced motion.
- **Pages:**
  - 207 prototype screens: 69 for each of Ledger, Console and Guide.
  - The 3 flow pages and the prototype index.
  - 12 screens with the "More" control open, via the `-menu` fragment.
  - The package's 21 document pages.
- **Themes and sizes:** each page in light and dark, at 1440×900 ("wide") and 390×844 ("narrow"), as full-page screenshots.
  - Prototype pages used the fragment `#light-nobar` or `#dark-nobar`.
  - Document pages used the device colour scheme, emulated.
- **Run:** 2026-11-16, 21:06:38Z to 21:13:05Z. Exit 0, nothing on stderr. The browser processes exited on their own.

## Results

976 captures, all loaded.

| Check | Result |
|---|---|
| Page errors and console errors | 0 |
| Requests outside the package | 0 |
| Failed requests inside the package | 0 |
| Pages wider than the window | 0 |
| Visible elements past the window edge | 0 |
| Theme applied different from the theme requested | 0. The sign-in pages are designed to stay dark and were left out of this check. |

`capture-results.json` has one entry per capture. Its `file` field holds the capture's file name. In the publisher's original results that field held a local path; the original file has SHA-256 `c89ec66b4c7e93e2db43a263bfed0bb3e59188d8fd218b9c1bfc12e82684871d`, the value listed in `PNG-SHA256SUMS`. Nothing else was changed.

## Files here

| Path | What it is |
|---|---|
| `screens/*.webp` | 264 of the 976 captures. For each option: the 18 representative pages its README lists and the 4 open-menu captures, in both themes and at both sizes. |
| `capture-results.json` | the result of every capture: errors, blocked or failed requests, page width, elements past the edge, theme applied and page height |
| `PNG-SHA256SUMS` | SHA-256 of the original PNG of every capture and of the results file. The SHA-256 of this list is `53eb3a841834f6b7fefd4fb6141331425c6f7a34c325de2d119eafc61fe117b1`. |
| `tool/capture.mjs`, `tool/pages.json` | the capture script and the list of pages and themes it was given |
| `SHA256SUMS` | SHA-256 of every other file in this folder |

The WebP files are lossless copies of the PNG captures. Each was decoded again after encoding and compared with its PNG pixel for pixel, and all 264 matched. The full set of 976 PNGs is not committed because of its size. It can be made again from `../../v1.6/` with `tool/capture.mjs`.

File names are `<option>_<page>__<theme>__<wide|narrow>.webp`. A name ending `-menu` was rendered with the "More" control open.

**Phone-width captures:** a full-page capture at phone width draws a bar fixed to the bottom of the screen where it sits on the first screen. On a tall capture, that is partway down the image. This is how full-page screenshots work, not a layout fault.

## Earlier runs

The publisher rendered v1, v1.2, v1.3, v1.4 and v1.5 the same way. Those runs found the faults that later versions corrected. `../../v1.6/README.md` records them by checksum. Their images are not in the repository.
