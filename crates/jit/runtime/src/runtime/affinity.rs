//! Last-level cache (L3) placement of the backend thread.

use std::{fs, mem};

/// Returns the CPUs the calling thread may run on outside the L3 domain of its current CPU.
///
/// Returns `None` if the domain cannot be read, or if every allowed CPU shares it.
pub(super) fn cpus_outside_current_llc() -> Option<libc::cpu_set_t> {
    // SAFETY: `sched_getcpu` has no preconditions, and an all-zero `cpu_set_t` is empty.
    let (cpu, mut cpus) = unsafe { (libc::sched_getcpu(), mem::zeroed::<libc::cpu_set_t>()) };
    let cpu = usize::try_from(cpu).ok()?;
    // SAFETY: `cpus` is a valid `cpu_set_t` of the given size, and pid 0 is the calling thread.
    if unsafe { libc::sched_getaffinity(0, mem::size_of::<libc::cpu_set_t>(), &raw mut cpus) } != 0
    {
        return None;
    }
    for shared in llc_cpus(cpu)? {
        if shared < libc::CPU_SETSIZE as usize {
            // SAFETY: `shared` is below `CPU_SETSIZE`.
            unsafe { libc::CPU_CLR(shared, &mut cpus) };
        }
    }
    // SAFETY: `cpus` is a valid `cpu_set_t`.
    (unsafe { libc::CPU_COUNT(&cpus) } > 0).then_some(cpus)
}

/// Restricts the calling thread to `cpus`, logging failures.
pub(super) fn set_current_thread(cpus: &libc::cpu_set_t) {
    // SAFETY: `cpus` is a valid `cpu_set_t` of the given size, and pid 0 is the calling thread.
    if unsafe { libc::sched_setaffinity(0, mem::size_of::<libc::cpu_set_t>(), cpus) } != 0 {
        warn!(error = %std::io::Error::last_os_error(), "failed to set JIT backend CPU affinity");
    }
}

/// Returns the CPUs that share `cpu`'s level 3 cache, from sysfs.
fn llc_cpus(cpu: usize) -> Option<Vec<usize>> {
    let dir = format!("/sys/devices/system/cpu/cpu{cpu}/cache");
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if fs::read_to_string(path.join("level")).is_ok_and(|level| level.trim() == "3") {
            return parse_cpu_list(fs::read_to_string(path.join("shared_cpu_list")).ok()?.trim());
        }
    }
    None
}

/// Parses a sysfs CPU list such as `0-5,24-29`.
fn parse_cpu_list(list: &str) -> Option<Vec<usize>> {
    let mut cpus = Vec::new();
    for range in list.split(',').filter(|range| !range.is_empty()) {
        let (start, end) = range.split_once('-').unwrap_or((range, range));
        cpus.extend(start.parse::<usize>().ok()?..=end.parse().ok()?);
    }
    Some(cpus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("0-2,24-25,7"), Some(vec![0, 1, 2, 24, 25, 7]));
        assert_eq!(parse_cpu_list("3"), Some(vec![3]));
        assert_eq!(parse_cpu_list("x"), None);
    }

    #[test]
    fn outside_llc_excludes_current_cpu() {
        if let Some(cpus) = cpus_outside_current_llc() {
            // SAFETY: `sched_getcpu` has no preconditions.
            let cpu = unsafe { libc::sched_getcpu() } as usize;
            // SAFETY: `cpus` is a valid `cpu_set_t`.
            assert!(!unsafe { libc::CPU_ISSET(cpu, &cpus) });
        }
    }
}
