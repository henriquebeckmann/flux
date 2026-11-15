//! Reading a process's resource use from `/proc` (Linux).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// Resident memory.
    pub rss_kb: u64,
    /// CPU time used (user + system).
    pub cpu_ms: u64,
}

/// `None` if the process is gone or `/proc` is not readable.
pub fn usage(pid: u32) -> Option<Usage> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses: fields start after the last ')'.
    let after = &stat[stat.rfind(')')? + 2..];
    let mut f = after.split_whitespace();
    let utime: u64 = f.nth(11)?.parse().ok()?; // field 14 overall
    let stime: u64 = f.next()?.parse().ok()?;
    // SAFETY: sysconf with valid names.
    let (page, hz) = unsafe { (libc::sysconf(libc::_SC_PAGESIZE) as u64, libc::sysconf(libc::_SC_CLK_TCK).max(1) as u64) };
    Some(Usage { rss_kb: rss_pages * page / 1024, cpu_ms: (utime + stime) * 1000 / hz })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let u = usage(std::process::id()).expect("/proc is readable");
        assert!(u.rss_kb > 100, "{u:?}");
        assert!(usage(u32::MAX - 1).is_none());
    }
}
