from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()

start = t.index('#[cfg(target_os = "macos")]\nfn resident_kb() -> u64 {')
end = t.index('#[cfg(target_os = "macos")]\nfn resident_kb_basic() -> u64 {')
new_impl = '''#[cfg(target_os = "macos")]
fn resident_kb() -> u64 {
    // 用 proc_pid_rusage(RUSAGE_INFO_V4).ri_phys_footprint —— 和 footprint(1)/我们的
    // 采样器完全同一个口径。为什么不用 task_info：TASK_VM_INFO 的结构体随 SDK 增长，
    // 字段偏移一旦对不上就读到 0（实测看门狗一次都没触发，就是因为这个）。
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u8) -> i32;
        fn getpid() -> i32;
    }
    const RUSAGE_INFO_V4: i32 = 4;
    #[repr(C)]
    struct RusageInfoV4 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
    }
    let mut buf: RusageInfoV4 = unsafe { std::mem::zeroed() };
    let kr = unsafe {
        proc_pid_rusage(
            getpid(),
            RUSAGE_INFO_V4,
            &mut buf as *mut _ as *mut u8,
        )
    };
    if kr == 0 {
        buf.ri_phys_footprint / 1024
    } else {
        resident_kb_basic()
    }
}

'''
t = t[:start] + new_impl + t[end:]

# 线程启动时打一条自检，方便确认口径
t = t.replace("""        .spawn(move || {
            let started = std::time::Instant::now();""",
"""        .spawn(move || {
            let started = std::time::Instant::now();
            log::plog!("kernel", info, "内存看门狗自检：当前 footprint {} KB，阈值 {} KB", resident_kb(), kb);""")
m.write_text(t)
print('ok proc_pid_rusage')
