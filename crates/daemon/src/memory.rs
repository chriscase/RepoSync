//! Process memory monitoring.
//!
//! Reads the daemon's RSS (Resident Set Size) from `/proc/self/status`
//! on Linux. Returns 0 on non-Linux platforms (safe fallback — the
//! memory limit never triggers).

/// Read the current process RSS in bytes.
///
/// On Linux, parses `/proc/self/status` for the `VmRSS` line.
/// Returns 0 on error or non-Linux platforms.
pub fn process_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        read_proc_rss().unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cfg(target_os = "linux")]
fn read_proc_rss() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // Format: "VmRSS:    123456 kB"
            let kb: u64 = rest
                .trim()
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            return Some(kb * 1024); // convert kB to bytes
        }
    }
    None
}
