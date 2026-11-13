//! Three-way merge engine.
//!
//! Uses the `diffy` crate to perform line-based three-way merges between a
//! base, "ours" (SVN), and "theirs" (Git) versions of a file.

use tracing::{debug, info};

use crate::errors::ConflictError;

/// The result of a three-way merge attempt.
#[derive(Debug, Clone)]
pub struct MergeResult {
    /// The merged content (may contain conflict markers if `has_conflicts` is true).
    pub merged_content: String,
    /// Whether the merge completed without conflicts.
    pub has_conflicts: bool,
    /// Locations of conflict markers within the merged content.
    pub conflict_markers: Vec<ConflictMarker>,
}

/// A single conflict region within merged output.
#[derive(Debug, Clone)]
pub struct ConflictMarker {
    /// Starting line number (1-indexed) of the conflict marker block.
    pub start_line: usize,
    /// Ending line number (1-indexed) of the conflict marker block.
    pub end_line: usize,
}

/// Stateless three-way merge engine.
pub struct Merger;

impl Merger {
    /// Attempt a three-way merge of `base`, `ours`, and `theirs`.
    ///
    /// Returns a [`MergeResult`] that always contains merged content. If the
    /// merge is clean, `has_conflicts` will be `false`. If there are
    /// conflicts, standard `<<<<<<<` / `=======` / `>>>>>>>` markers are
    /// inserted and `has_conflicts` is `true`.
    pub fn three_way_merge(
        base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<MergeResult, ConflictError> {
        info!("performing three-way merge");

        // Fast path: if either side is identical to base, the other side wins cleanly.
        if ours == base {
            debug!("ours == base, theirs wins cleanly");
            return Ok(MergeResult {
                merged_content: theirs.to_string(),
                has_conflicts: false,
                conflict_markers: Vec::new(),
            });
        }
        if theirs == base {
            debug!("theirs == base, ours wins cleanly");
            return Ok(MergeResult {
                merged_content: ours.to_string(),
                has_conflicts: false,
                conflict_markers: Vec::new(),
            });
        }

        // Fast path: if both sides made the exact same change, no conflict.
        if ours == theirs {
            debug!("ours == theirs, identical changes");
            return Ok(MergeResult {
                merged_content: ours.to_string(),
                has_conflicts: false,
                conflict_markers: Vec::new(),
            });
        }

        // Use diffy to create patches from base to each side.
        let patch_ours = diffy::create_patch(base, ours);
        let patch_theirs = diffy::create_patch(base, theirs);

        // Try applying "theirs" patch to "ours" content.
        // If this succeeds cleanly, we have an automatic merge.
        let theirs_applied = diffy::apply(ours, &patch_theirs);

        if let Ok(merged) = theirs_applied {
            debug!("clean merge via applying theirs-patch to ours");
            return Ok(MergeResult {
                merged_content: merged,
                has_conflicts: false,
                conflict_markers: Vec::new(),
            });
        }

        // Try the reverse: apply "ours" patch to "theirs" content.
        let ours_applied = diffy::apply(theirs, &patch_ours);

        if let Ok(merged) = ours_applied {
            debug!("clean merge via applying ours-patch to theirs");
            return Ok(MergeResult {
                merged_content: merged,
                has_conflicts: false,
                conflict_markers: Vec::new(),
            });
        }

        // Both patch applications failed -- produce conflicted output with markers.
        debug!("automatic merge failed, generating conflict markers");
        let (merged, markers) = generate_conflict_output(base, ours, theirs);

        Ok(MergeResult {
            merged_content: merged,
            has_conflicts: true,
            conflict_markers: markers,
        })
    }

    /// Quick check: can these three versions be auto-merged without conflicts?
    pub fn can_auto_merge(base: &str, ours: &str, theirs: &str) -> bool {
        // If either side is identical to base, no conflict is possible.
        if ours == base || theirs == base {
            return true;
        }
        // If both sides made the same change, also fine.
        if ours == theirs {
            return true;
        }

        // Try both patch directions.
        let patch_theirs = diffy::create_patch(base, theirs);
        if diffy::apply(ours, &patch_theirs).is_ok() {
            return true;
        }

        let patch_ours = diffy::create_patch(base, ours);
        diffy::apply(theirs, &patch_ours).is_ok()
    }
}

/// Generate conflict-marker output for a failed three-way merge.
///
/// Uses a simple line-by-line comparison to produce standard Git-style
/// conflict markers.
fn generate_conflict_output(base: &str, ours: &str, theirs: &str) -> (String, Vec<ConflictMarker>) {
    let base_lines: Vec<&str> = base.lines().collect();
    let ours_lines: Vec<&str> = ours.lines().collect();
    let theirs_lines: Vec<&str> = theirs.lines().collect();

    let mut output = Vec::new();
    let mut markers = Vec::new();

    let max_len = ours_lines
        .len()
        .max(theirs_lines.len())
        .max(base_lines.len());

    let mut i = 0;
    while i < max_len {
        let ours_line = ours_lines.get(i).copied();
        let theirs_line = theirs_lines.get(i).copied();
        let base_line = base_lines.get(i).copied();

        match (ours_line, theirs_line) {
            (Some(o), Some(t)) if o == t => {
                // Same on both sides -- no conflict.
                output.push(o.to_string());
                i += 1;
            }
            (Some(o), Some(t)) => {
                // Different -- find the extent of the conflicting region.
                let start_line = output.len() + 1;

                // Collect contiguous differing lines.
                let mut ours_block = vec![o.to_string()];
                let mut theirs_block = vec![t.to_string()];
                let mut j = i + 1;
                while j < max_len {
                    let ol = ours_lines.get(j).copied();
                    let tl = theirs_lines.get(j).copied();
                    if ol == tl {
                        break;
                    }
                    if let Some(o2) = ol {
                        ours_block.push(o2.to_string());
                    }
                    if let Some(t2) = tl {
                        theirs_block.push(t2.to_string());
                    }
                    j += 1;
                }

                output.push("<<<<<<< ours (SVN)".to_string());
                output.extend(ours_block);
                if let Some(bl) = base_line {
                    output.push("||||||| base".to_string());
                    // Include base lines for the same range.
                    for k in i..j {
                        if let Some(b) = base_lines.get(k) {
                            output.push(b.to_string());
                        }
                    }
                    let _ = bl; // suppress unused warning
                }
                output.push("=======".to_string());
                output.extend(theirs_block);
                output.push(">>>>>>> theirs (Git)".to_string());

                let end_line = output.len();
                markers.push(ConflictMarker {
                    start_line,
                    end_line,
                });

                i = j;
            }
            (Some(o), None) => {
                output.push(o.to_string());
                i += 1;
            }
            (None, Some(t)) => {
                output.push(t.to_string());
                i += 1;
            }
            (None, None) => {
                i += 1;
            }
        }
    }

    (output.join("\n"), markers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_files() {
        let base = "line1\nline2\nline3\n";
        let ours = base;
        let theirs = base;
        let result = Merger::three_way_merge(base, ours, theirs).unwrap();
        assert!(!result.has_conflicts);
        assert!(result.conflict_markers.is_empty());
    }

    #[test]
    fn test_only_ours_changed() {
        let base = "line1\nline2\nline3\n";
        let ours = "line1\nmodified\nline3\n";
        let theirs = base;
        let result = Merger::three_way_merge(base, ours, theirs).unwrap();
        assert!(!result.has_conflicts);
        assert!(result.merged_content.contains("modified"));
    }

    #[test]
    fn test_only_theirs_changed() {
        let base = "line1\nline2\nline3\n";
        let ours = base;
        let theirs = "line1\nline2\nmodified\n";
        let result = Merger::three_way_merge(base, ours, theirs).unwrap();
        assert!(!result.has_conflicts);
        assert!(result.merged_content.contains("modified"));
    }

    #[test]
    fn test_non_overlapping_changes() {
        let base = "aaa\nbbb\nccc\nddd\neee\n";
        let ours = "AAA\nbbb\nccc\nddd\neee\n";
        let theirs = "aaa\nbbb\nccc\nddd\nEEE\n";
        let result = Merger::three_way_merge(base, ours, theirs).unwrap();
        assert!(!result.has_conflicts);
        assert!(result.merged_content.contains("AAA"));
        assert!(result.merged_content.contains("EEE"));
    }

    #[test]
    fn test_conflicting_changes() {
        let base = "line1\noriginal\nline3\n";
        let ours = "line1\nours_version\nline3\n";
        let theirs = "line1\ntheirs_version\nline3\n";
        let result = Merger::three_way_merge(base, ours, theirs).unwrap();
        assert!(result.has_conflicts);
        assert!(result.merged_content.contains("<<<<<<<"));
        assert!(result.merged_content.contains("======="));
        assert!(result.merged_content.contains(">>>>>>>"));
        assert!(!result.conflict_markers.is_empty());
    }

    #[test]
    fn test_can_auto_merge() {
        let base = "aaa\nbbb\nccc\n";
        assert!(Merger::can_auto_merge(base, base, base));
        assert!(Merger::can_auto_merge(base, "AAA\nbbb\nccc\n", base));
        // Both sides unchanged = trivially auto-mergeable
        assert!(Merger::can_auto_merge(base, base, "aaa\nbbb\nCCC\n"));
        // Same change on both sides
        assert!(Merger::can_auto_merge(
            base,
            "XXX\nbbb\nccc\n",
            "XXX\nbbb\nccc\n"
        ));

        // Non-overlapping changes on a larger file with enough context for diffy.
        let base_large = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n";
        let ours_large = "LINE1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n";
        let theirs_large = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nLINE8\n";
        assert!(Merger::can_auto_merge(base_large, ours_large, theirs_large));
    }

    #[test]
    fn test_cannot_auto_merge() {
        let base = "line1\noriginal\nline3\n";
        assert!(!Merger::can_auto_merge(
            base,
            "line1\nours\nline3\n",
            "line1\ntheirs\nline3\n"
        ));
    }

    #[test]
    fn test_same_change_both_sides() {
        let base = "old\n";
        let ours = "new\n";
        let theirs = "new\n";
        let result = Merger::three_way_merge(base, ours, theirs).unwrap();
        assert!(!result.has_conflicts);
    }
}
