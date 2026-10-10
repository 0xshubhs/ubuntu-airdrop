//! How much room is left, and how much of it one transfer may take.
//!
//! A phone with a full camera roll can fill a disk in one go, and the only
//! sign of it is everything else on the machine starting to fail. omdrop
//! caps a transfer at a share of the free space; this is the same idea, and
//! the share is what the cap is expressed in, because "30%" stays sensible
//! when the disk changes and "8 GB" does not.

use std::path::Path;

/// Bytes free on the filesystem holding `path`.
///
/// `f_bavail`, not `f_bfree`: the reserve root keeps back is not ours to
/// fill. `None` when the path cannot be stat'd, which the callers read as
/// "no idea, let it through" rather than refusing on a technicality.
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // Safe: the pointer is a valid NUL-terminated path, and `st` is ours.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut st) };
    (rc == 0).then(|| (st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// The largest transfer allowed: `pct` percent of `free`.
///
/// 0 means no cap at all, for anyone who would rather Drop just tried.
pub fn budget(free: u64, pct: u8) -> Option<u64> {
    (pct > 0).then(|| free / 100 * u64::from(pct.min(100)))
}

/// The cap for a folder, or `None` when there is no cap or no answer.
pub fn budget_for(dir: &Path, pct: u8) -> Option<u64> {
    budget(free_bytes(dir)?, pct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_share_it_is_given() {
        assert_eq!(budget(1_000, 30), Some(300));
        assert_eq!(budget(1_000, 100), Some(1_000));
        assert_eq!(budget(0, 30), Some(0));
    }

    #[test]
    fn zero_percent_means_no_cap() {
        assert_eq!(budget(1_000, 0), None);
    }

    #[test]
    fn a_share_over_a_hundred_is_still_the_whole_disk() {
        assert_eq!(budget(1_000, 200), Some(1_000));
    }

    /// 30% of a 4 TB disk overflows nothing: divide before multiplying.
    #[test]
    fn survives_a_big_disk() {
        let four_tb = 4u64 * 1024 * 1024 * 1024 * 1024;
        assert_eq!(budget(four_tb, 30), Some(four_tb / 100 * 30));
    }

    #[test]
    fn reads_the_free_space_of_a_real_directory() {
        let free = free_bytes(std::path::Path::new("/")).expect("/ can be stat'd");
        assert!(free > 0, "a working filesystem has some room");
        assert_eq!(free_bytes(std::path::Path::new("/no/such/place")), None);
    }
}
