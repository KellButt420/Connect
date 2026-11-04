//! This process's CPU and memory use, for the session stats.

use std::time::Instant;

use serde::{Deserialize, Serialize};

/// CPU and memory use of the process, over the time since the previous sample.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessLoad {
    /// CPU time used per wall-clock time, in percent of one core: 100 is one core fully busy,
    /// and the most is 100 × `cores`.
    pub cpu_percent: f32,
    /// Logical processors on this PC.
    pub cores: u16,
    /// Physical memory in use: the working set on Windows, the resident set (VmRSS) on Linux.
    /// Includes shared pages such as the graphics driver's.
    pub memory_bytes: u64,
    /// Memory only this process uses: committed private memory on Windows (whether resident or
    /// paged out), resident anonymous memory (RssAnon) on Linux.
    pub private_bytes: u64,
}

/// Samples [`ProcessLoad`]. Each sample's CPU figure covers the time since the previous one (or
/// since [`ProcessMonitor::new`]), so take them at a steady interval, around a second.
pub struct ProcessMonitor {
    last: Option<(Instant, u64)>,
    cores: u16,
}

impl ProcessMonitor {
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get().min(u16::MAX as usize) as u16);
        Self { last: imp::cpu_time_ns().map(|cpu| (Instant::now(), cpu)), cores }
    }

    /// The load since the last sample; `None` if this system can't report it.
    pub fn sample(&mut self) -> Option<ProcessLoad> {
        let now = Instant::now();
        let cpu = imp::cpu_time_ns()?;
        let (memory_bytes, private_bytes) = imp::memory()?;
        let cpu_percent = match self.last.replace((now, cpu)) {
            Some((then, before)) => {
                let wall = now.duration_since(then).as_nanos() as f64;
                if wall > 0.0 { (cpu.saturating_sub(before) as f64 / wall * 100.0) as f32 } else { 0.0 }
            }
            None => 0.0,
        };
        Some(ProcessLoad { cpu_percent, cores: self.cores, memory_bytes, private_bytes })
    }
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };

    /// User plus kernel time of every thread, in ns.
    pub fn cpu_time_ns() -> Option<u64> {
        let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
        // SAFETY: the pseudo-handle for this process, and four valid FILETIMEs to fill.
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        // FILETIME counts 100 ns.
        Some((ticks(kernel) + ticks(user)) * 100)
    }

    pub fn memory() -> Option<(u64, u64)> {
        let mut counters =
            PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        // SAFETY: `counters` is the extended struct, and `cb` says so.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        ok.as_bool().then_some((counters.WorkingSetSize as u64, counters.PrivateUsage as u64))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// Clock ticks per second in /proc: `USER_HZ`, which is 100 on every Linux architecture
    /// Connect builds for.
    const USER_HZ: u64 = 100;

    /// User plus system time of every thread, in ns, at the kernel's tick resolution (10 ms).
    pub fn cpu_time_ns() -> Option<u64> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        super::parse_stat_ticks(&stat).map(|ticks| ticks * (1_000_000_000 / USER_HZ))
    }

    pub fn memory() -> Option<(u64, u64)> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        super::parse_status_memory(&status)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn cpu_time_ns() -> Option<u64> {
        None
    }

    pub fn memory() -> Option<(u64, u64)> {
        None
    }
}

/// utime + stime from `/proc/<pid>/stat`, in clock ticks. The command name (field 2) is in
/// parentheses and may hold spaces or parentheses itself, so fields are counted from the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat_ticks(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    // After the name: state is field 3, utime field 14, stime field 15.
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// VmRSS and RssAnon from `/proc/<pid>/status`, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_status_memory(status: &str) -> Option<(u64, u64)> {
    let kb = |name: &str| {
        status.lines().find_map(|line| {
            let value = line.strip_prefix(name)?.strip_prefix(':')?;
            value.trim().trim_end_matches("kB").trim().parse::<u64>().ok().map(|kb| kb * 1024)
        })
    };
    Some((kb("VmRSS")?, kb("RssAnon").unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat_even_with_odd_command_names() {
        let stat = "4242 (my (odd) prog) S 1 4242 4242 0 -1 4194304 500 0 0 0 1234 56 0 0 20 0 9 0 100 0 0";
        assert_eq!(parse_stat_ticks(stat), Some(1234 + 56));
        assert_eq!(parse_stat_ticks("garbage"), None);
    }

    #[test]
    fn parses_proc_status_memory() {
        let status = "Name:\tconnect\nVmPeak:\t  900000 kB\nVmRSS:\t  153600 kB\nRssAnon:\t   81920 kB\n";
        assert_eq!(parse_status_memory(status), Some((153_600 * 1024, 81_920 * 1024)));
        assert_eq!(parse_status_memory("Name:\tconnect\n"), None);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn measures_this_process() {
        let mut monitor = ProcessMonitor::new();
        // Burn some CPU so the figure has something to show at a 10 ms tick.
        let start = Instant::now();
        let mut x = 0u64;
        while start.elapsed().as_millis() < 60 {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        let load = monitor.sample().expect("this OS reports process load");
        assert!(load.memory_bytes > 1 << 20, "{load:?}");
        assert!(load.cores >= 1);
        assert!(load.cpu_percent >= 0.0 && load.cpu_percent <= 100.0 * f32::from(load.cores) + 50.0, "{load:?}");
    }
}
