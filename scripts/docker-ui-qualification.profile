qualification: product Docker UI and API startup
status: NOT_RUN
blocker: no admitted Docker daemon or host profile for a product image build and UI/API startup
base: acf027bec8b6c9c0ceb3f53fe5a9e794d40a2b81
base_tree: 092db346ca953e95ad281711ed019145d46a22eb
preserved_private_work: /agent/repos/RepoSync-release-118 cursor/118-release-artifact-3f83 bf348c5c0a2bbaf6b9828d1f10ddc1a5b36a380d
preserved_private_tree: ce818284604a84066e78d96566503d9b14d2c834
preserved_private_parent: dd1d82853fbeaede9105211c425c724d22669c52

source_gap:
  Dockerfile copies config.example.toml and the release binaries.
  The daemon serves static/ next to the executable (crates/web/src/lib.rs).
  The image does not build or copy web-ui/dist to /usr/local/bin/static.
  packaging/static/assets/app.js is the synthetic sentinel and is not the production UI.
  .github/workflows/ci.yml has no Docker build or startup step.
  The release archive assembler already builds web-ui and rejects the synthetic sentinel.
  The builder image is rust:1-bookworm. rust:stable-bookworm is not a published library tag.

candidate:
  Dockerfile node:22-bookworm stage runs npm ci and npm run build, refuses the synthetic sentinel, and copies dist to /usr/local/bin/static.
  .dockerignore keeps host web-ui/node_modules and web-ui/dist out of that build.
  scripts/qualify_docker_ui.sh is the unrun qualification.

not_run_cases:
  positive: one immutable image, scripts/fixtures/docker-ui-qualify-synthetic.toml (sync_branches = [] selects every branch; file:// SVN; loopback GitHub), health JSON ok/version/git_commit with status omitted, production index naming an asset, that asset containing "Welcome to RepoSync" and not the synthetic sentinel
  negative_missing_config: --config path that load_from_file rejects as FileNotFound. Qualify only docker-wait rc 0, state exited, OOMKilled false, exit 1, and log "configuration file not found:". Timeout, Docker error, OOM, panic, and any other exit do not qualify.
  negative_invalid_config: scripts/fixtures/docker-ui-qualify-invalid.toml. Qualify only the same container outcome plus "daemon.poll_interval_secs" and "poll interval must be > 0". A missing-file diagnostic does not qualify this case.
  negative_missing_static: empty mount over /usr/local/bin/static. Qualify only completed transport, index HTTP 500, and a still-healthy API (HTTP 200, ok true, version, git_commit, status omitted). HTTP 000 and any other index status do not qualify.
  authored_checks: scripts/qualify_docker_ui_authored_checks.sh exercises those decisions, including unrelated nonzero exits, before Docker is used.
  cleanup: only the slot containers and the network this script created. Stop and wait are bounded. A failed stop skips wait. Removal is verified by inspect. Unresolved removal changes a pass into a failure and keeps an earlier failure status. The receipt is printed before the workdir is removed.

limits_in_script: cpus=1 memory=512m memory-swap=512m pids=128 data-tmpfs=64m build-timeout=1800s start-timeout=60s fail-timeout=20s cleanup-stop=20s cleanup-wait=15s cleanup-rm=15s
platform: not executed. aarch64 and Darwin remain unresolved. One Linux run would not accept those platforms.
admission_record: internal/issue-118-successor-freeze-2026-11-10.md records NOT_RUN docker and no docker daemon. No later admission covers this source.
