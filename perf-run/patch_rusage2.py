from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
start = t.index('#[cfg(target_os = "macos")]\nfn resident_kb() -> u64 {')
end = t.index('#[cfg(target_os = "macos")]\nfn resident_kb_basic() -> u64 {')
new_impl = '''#[cfg(target_os = "macos")]
fn resident_kb() -> u64 {
    // proc_pid_rusage(RUSAGE_INFO_V4).ri_phys_footprint —— 和 footprint(1) 同口径。
    // ⚠ 缓冲区必须**明显大于**结构体：内核按自己的版本往里写，给个刚好够的
    //   Rust 结构体会把栈写花（实测：面板启动 1 秒后直接死掉，日志只留下一行 boot）。
    //   用 2KB 缓冲 + 固定偏移读取：uuid(16) + 7×u64 → phys_footprint 在 72。
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u8) -> i32;
        fn getpid() -> i32;
    }
    const RUSAGE_INFO_V4: i32 = 4;
    const OFF_PHYS_FOOTPRINT: usize = 72;
    let mut buf = [0u8; 2048];
    let kr = unsafe { proc_pid_rusage(getpid(), RUSAGE_INFO_V4, buf.as_mut_ptr()) };
    if kr == 0 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&buf[OFF_PHYS_FOOTPRINT..OFF_PHYS_FOOTPRINT + 8]);
        u64::from_ne_bytes(b) / 1024
    } else {
        resident_kb_basic()
    }
}

'''
t = t[:start] + new_impl + t[end:]
m.write_text(t)
print('ok rusage buffer')
