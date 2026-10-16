//! Process memory in bytes: PrivateUsage (commit charge) on Windows, RSS elsewhere.

#[cfg(windows)]
pub fn private_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain out-struct call on the current process handle.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            c.cb,
        );
        c.PrivateUsage as u64
    }
}

#[cfg(not(windows))]
pub fn private_bytes() -> u64 {
    // ps works on macOS and Linux; KiB -> bytes.
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok()?.trim().parse::<u64>().ok())
        .map_or(0, |k| k * 1024)
}
