//! fork: how much of the CPU is used (the measurement log's per-second readings; see FORK.md): this
//! app's own process, and the whole system (the Xbox player's video runs in processes of its own, on
//! the same cores). Windows: GetProcessTimes / GetSystemTimes; Linux: /proc; elsewhere: unknown.

/// CPU time used so far, in ms: (this process, all processes busy, all processes total) — the last two
/// summed over every core, so busy / total is the share of the whole machine. None where unknown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Times {
    pub process_ms: f64,
    pub busy_ms: f64,
    pub total_ms: f64,
}

/// The CPU used between two readings, in % of the whole machine (every core): (this process, all).
pub fn share(before: &Times, after: &Times) -> Option<(f64, f64)> {
    let total = after.total_ms - before.total_ms;
    if total <= 0.0 {
        return None;
    }
    let round1 = |v: f64| (v.clamp(0.0, 100.0) * 10.0).round() / 10.0;
    Some((round1((after.process_ms - before.process_ms) / total * 100.0), round1((after.busy_ms - before.busy_ms) / total * 100.0)))
}

/// The CPU times now.
pub fn now() -> Option<Times> {
    imp::now()
}

/// This process's CPU time so far (ms, every thread).
pub fn process_ms() -> Option<f64> {
    imp::now().map(|t| t.process_ms)
}

/// The calling thread's own CPU time so far (ms): the reading thread's, for the log's per-second readings.
pub fn thread_ms() -> Option<f64> {
    imp::thread_ms()
}

#[cfg(windows)]
mod imp {
    // (kernel32 itself: no new dependency; FILETIME is two 32-bit halves, low first: a u64)
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(process: isize, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
        fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
        fn GetCurrentThread() -> isize;
        fn GetThreadTimes(thread: isize, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    }

    pub fn thread_ms() -> Option<f64> {
        let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
        // SAFETY: plain out-parameters of the documented sizes; the thread handle is the pseudo-handle
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) != 0 };
        ok.then(|| (k + u) as f64 / 10_000.0)
    }

    pub fn now() -> Option<super::Times> {
        let (mut c, mut e, mut pk, mut pu) = (0u64, 0u64, 0u64, 0u64);
        let (mut idle, mut sk, mut su) = (0u64, 0u64, 0u64);
        // SAFETY: plain out-parameters of the documented sizes; the process handle is the pseudo-handle
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut pk, &mut pu) != 0 && GetSystemTimes(&mut idle, &mut sk, &mut su) != 0 };
        if !ok {
            return None;
        }
        // (100 ns units; the system's kernel time includes its idle time)
        let ms = |v: u64| v as f64 / 10_000.0;
        Some(super::Times { process_ms: ms(pk + pu), busy_ms: ms((sk + su).saturating_sub(idle)), total_ms: ms(sk + su) })
    }
}

#[cfg(all(target_os = "linux", not(windows)))]
mod imp {
    /// (the kernel's clock ticks: 100 a second on every common Linux)
    const TICK_MS: f64 = 10.0;

    pub fn now() -> Option<super::Times> {
        // this process: utime and stime, the 14th and 15th fields (after the name in brackets)
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let rest = stat.rsplit_once(')')?.1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        let (ut, st): (f64, f64) = (f.get(11)?.parse().ok()?, f.get(12)?.parse().ok()?);
        // all: the first line of /proc/stat, "cpu user nice system idle iowait irq softirq steal …"
        let all = std::fs::read_to_string("/proc/stat").ok()?;
        let v: Vec<f64> = all.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
        let total: f64 = v.iter().take(8).sum();
        let idle = v.get(3)? + v.get(4).copied().unwrap_or(0.0);
        Some(super::Times { process_ms: (ut + st) * TICK_MS, busy_ms: (total - idle) * TICK_MS, total_ms: total * TICK_MS })
    }

    pub fn thread_ms() -> Option<f64> {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
        let rest = stat.rsplit_once(')')?.1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        let (ut, st): (f64, f64) = (f.get(11)?.parse().ok()?, f.get(12)?.parse().ok()?);
        Some((ut + st) * TICK_MS)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn now() -> Option<super::Times> {
        None
    }
    pub fn thread_ms() -> Option<f64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_cpu_share_is_a_share_of_the_whole_machine() {
        let a = Times { process_ms: 100.0, busy_ms: 1000.0, total_ms: 4000.0 };
        let b = Times { process_ms: 300.0, busy_ms: 2000.0, total_ms: 8000.0 };
        assert_eq!(share(&a, &b), Some((5.0, 25.0)));
        assert_eq!(share(&b, &b), None);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn fork_cpu_times_move_while_working() {
        let a = now().expect("CPU times");
        // (some work of this thread's own)
        let mut x = 0u64;
        let until = std::time::Instant::now() + std::time::Duration::from_millis(150);
        while std::time::Instant::now() < until {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        }
        std::hint::black_box(x);
        let b = now().expect("CPU times");
        assert!(b.process_ms > a.process_ms, "{a:?} {b:?}");
        assert!(thread_ms().is_some_and(|t| t > 0.0), "this thread's own CPU time");
        assert!(b.total_ms > a.total_ms && b.busy_ms >= a.busy_ms, "{a:?} {b:?}");
    }
}
