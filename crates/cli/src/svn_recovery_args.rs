use std::{ffi::OsStr, path::PathBuf};

use clap::{builder::TypedValueParser, error::ErrorKind, Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum RecoveryAction {
    /// Create a private local review key for authenticated historical installations.
    CreateReviewKey {
        /// New file in a private, operator-owned directory outside RepoSync data.
        #[arg(long)]
        output: PathBuf,
    },
    /// Read the evidence and print a revision-bound token for review.
    Preview(RecoveryInputs),
    /// Recheck the evidence and record the result while the pair stays paused.
    Apply {
        #[command(flatten)]
        inputs: RecoveryInputs,
        /// Token copied from the reviewed preview, including its SVN revision and SHA-256 digest.
        #[arg(long, value_parser = RecoveryText::Digest)]
        reviewed_digest: String,
        /// Confirm the old service and other writers acting for this bridge are stopped.
        #[arg(long, required = true)]
        confirm_stopped_writers: bool,
        /// Attest that this is the known-good old installation before replacement.
        #[arg(
            long,
            required_unless_present = "attest_trusted_backup",
            conflicts_with = "svn_backup"
        )]
        attest_known_good_install: bool,
        /// Attest that the restored backup is independently attributable to the old service.
        #[arg(
            long,
            requires = "svn_backup",
            conflicts_with = "attest_known_good_install"
        )]
        attest_trusted_backup: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct RecoveryInputs {
    /// Team reposync.db from the stopped old installation, or the migrated database in backup mode.
    #[arg(long)]
    pub database: PathBuf,
    /// Registration UUID reported by upgrade-preflight.
    #[arg(long, value_parser = RecoveryText::Pair)]
    pub pair: String,
    /// Complete local bare Git mirror containing the configured branch and all imported commits.
    #[arg(long)]
    pub git_mirror: PathBuf,
    /// Private local review key used for both preview and apply when the old site has no credential review key.
    #[arg(long)]
    pub review_key_file: Option<PathBuf>,
    /// Local SVN repository independently restored from a trusted pre-upgrade backup.
    #[arg(long, requires = "historical_root_url")]
    pub svn_backup: Option<PathBuf>,
    /// Original SVN service root URL from independent records, without credentials.
    #[arg(long, requires = "svn_backup", value_parser = RecoveryText::RootUrl)]
    pub historical_root_url: Option<String>,
}

pub(crate) fn parse_review_token(value: &str) -> Option<(i64, &str)> {
    let (number, hash) = value.strip_prefix('r')?.split_once('.')?;
    let floor = number.parse::<i64>().ok()?;
    (floor > 0
        && number == floor.to_string()
        && hash.len() == 64
        && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then_some((floor, hash))
}

// Clap's ordinary parse errors repeat the input. These arguments may accidentally
// contain a private URL or credential, so only render the expected format.
#[derive(Clone)]
enum RecoveryText {
    Pair,
    Digest,
    RootUrl,
}

impl TypedValueParser for RecoveryText {
    type Value = String;

    fn parse_ref(
        &self,
        _command: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<String, clap::Error> {
        let value = value.to_str().unwrap_or_default();
        let message = match self {
            Self::Pair => {
                if value.len() == 36
                    && value.bytes().enumerate().all(|(index, byte)| {
                        if matches!(index, 8 | 13 | 18 | 23) {
                            byte == b'-'
                        } else {
                            byte.is_ascii_hexdigit()
                        }
                    })
                {
                    return Ok(value.to_ascii_lowercase());
                }
                "--pair requires a hyphenated registration UUID"
            }
            Self::Digest => {
                if parse_review_token(value).is_some() {
                    return Ok(value.to_owned());
                }
                "--reviewed-digest requires the full r<revision>.<sha256> token from the preview"
            }
            Self::RootUrl => {
                if reposync_core::svn::operand::checked_url(value)
                    .is_ok_and(|url| url.username().is_empty())
                {
                    return Ok(value.to_owned());
                }
                "--historical-root-url requires an SVN service root URL without credentials, query or fragment"
            }
        };
        Err(clap::Error::raw(ErrorKind::ValueValidation, message))
    }
}

#[cfg(test)]
mod tests {
    use super::RecoveryAction;
    use crate::{Cli, Commands};
    use clap::Parser;

    const PAIR: &str = "7D156383-EC0A-459D-A7C3-B1D12C7320CE";
    const DIGEST: &str = "r23.b38f42d62640f3201f3a442c60795f9e808825999f6a98dfd55aa371229c6021";
    const BACKUP: [&str; 4] = [
        "--svn-backup",
        "/backup/restored",
        "--historical-root-url",
        "file:///srv/svn/team",
    ];
    const APPLY: [&str; 3] = ["--reviewed-digest", DIGEST, "--confirm-stopped-writers"];

    fn parse(action: &str, extra: &[&str]) -> Result<RecoveryAction, clap::Error> {
        let args = [
            "reposync",
            "recover-svn-lineage",
            action,
            "--database",
            "reposync.db",
            "--pair",
            PAIR,
            "--git-mirror",
            "/mirror/team",
        ];
        let cli = Cli::try_parse_from(args.into_iter().chain(extra.iter().copied()))?;
        let Commands::RecoverSvnLineage { action } = cli.command else {
            unreachable!()
        };
        Ok(action)
    }

    #[test]
    fn token_requires_one_positive_canonical_revision_and_exact_lowercase_digest() {
        let (_, digest) = super::parse_review_token(DIGEST).unwrap();
        for invalid in [
            digest.to_owned(),
            format!("r0.{digest}"),
            format!("r01.{digest}"),
            format!("r+1.{digest}"),
            format!("r1.{}", digest.to_ascii_uppercase()),
        ] {
            assert!(super::parse_review_token(&invalid).is_none());
        }
    }

    #[test]
    fn preview_accepts_both_evidence_sources_and_rejects_apply_options() {
        let RecoveryAction::Preview(inputs) = parse("preview", &[]).unwrap() else {
            unreachable!()
        };
        assert_eq!(inputs.pair, PAIR.to_ascii_lowercase());
        assert!(inputs.svn_backup.is_none());
        let RecoveryAction::Preview(inputs) = parse("preview", &BACKUP).unwrap() else {
            unreachable!()
        };
        assert_eq!(inputs.historical_root_url.as_deref(), Some(BACKUP[3]));
        for extra in [
            &BACKUP[..2],
            &BACKUP[2..],
            &["--reviewed-digest", DIGEST],
            &["--confirm-stopped-writers"],
        ] {
            assert!(parse("preview", extra).is_err());
        }
    }

    #[test]
    fn apply_requires_digest_stopped_writers_and_matching_attestation() {
        let planned = [APPLY.as_slice(), &["--attest-known-good-install"]].concat();
        let RecoveryAction::Apply {
            reviewed_digest,
            confirm_stopped_writers,
            attest_known_good_install,
            ..
        } = parse("apply", &planned).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(reviewed_digest, DIGEST);
        assert!(confirm_stopped_writers && attest_known_good_install);

        let restored = [APPLY.as_slice(), &BACKUP, &["--attest-trusted-backup"]].concat();
        let RecoveryAction::Apply {
            attest_trusted_backup,
            ..
        } = parse("apply", &restored).unwrap()
        else {
            unreachable!()
        };
        assert!(attest_trusted_backup);
        for incomplete in [
            APPLY.to_vec(),
            [&APPLY[..2], &["--attest-known-good-install"]].concat(),
            [&APPLY[2..], &["--attest-known-good-install"]].concat(),
            [APPLY.as_slice(), &["--attest-trusted-backup"]].concat(),
            [planned.as_slice(), &BACKUP].concat(),
            [restored.as_slice(), &["--attest-known-good-install"]].concat(),
        ] {
            assert!(parse("apply", &incomplete).is_err());
        }
    }

    #[test]
    fn recovery_preserves_global_options_and_rejects_malformed_evidence_without_echoing_values() {
        const PRIVATE: &str = "EXAMPLE_private_value";
        let leading = [
            "reposync",
            "--config",
            PRIVATE,
            "recover-svn-lineage",
            "preview",
            "--database",
            "team.db",
            "--git-mirror",
            "mirror",
            "--pair",
            PAIR,
        ];
        assert!(Cli::try_parse_from(leading).is_ok());
        assert!(parse("preview", &["--config", PRIVATE]).is_ok());
        let invalid_url = format!("https://reader:{PRIVATE}@example.test/team");
        for extra in [
            vec!["--password", PRIVATE],
            vec!["--token=EXAMPLE_private_value"],
            vec![
                "--svn-backup",
                "/old",
                "--historical-root-url",
                invalid_url.as_str(),
            ],
        ] {
            let err = parse("preview", &extra).unwrap_err().to_string();
            assert!(!err.contains(PRIVATE));
        }
        for (action, args) in [
            ("preview", vec!["--pair", PRIVATE]),
            (
                "apply",
                vec![
                    "--pair",
                    PAIR,
                    "--attest-known-good-install",
                    "--confirm-stopped-writers",
                    "--reviewed-digest",
                    PRIVATE,
                ],
            ),
            (
                "apply",
                vec![
                    "--pair",
                    PAIR,
                    "--attest-known-good-install",
                    "--confirm-stopped-writers",
                    "--reviewed-digest",
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                ],
            ),
        ] {
            let err = Cli::try_parse_from(
                [
                    "reposync",
                    "recover-svn-lineage",
                    action,
                    "--database",
                    "team.db",
                    "--git-mirror",
                    "local",
                ]
                .into_iter()
                .chain(args),
            )
            .unwrap_err()
            .to_string();
            assert!(!err.contains(PRIVATE));
        }
    }
}
