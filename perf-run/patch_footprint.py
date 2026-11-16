from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()

old_fn = """#[cfg(target_os = "macos")]
fn resident_kb() -> u64 {"""
new_fn = """#[cfg(target_os = "macos")]
fn resident_kb() -> u64 {
    // phys_footprint（TASK_VM_INFO），而不是 resident_size：
    // 后者把共享库也算进去（实测同一个进程 RSS 14.7MB / footprint 6.0MB），
    // 用它当阈值会一开始就超，看门狗陷入"每 20 秒重启一次"。
    #[repr(C)]
    struct TaskVmInfo {
        virtual_size: u64,
        region_count: u32,
        page_size: u32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
        min_address: u64,
        max_address: u64,
        ledger_phys_footprint_peak: u64,
        ledger_tag_network_nonvolatile: u64,
        ledger_tag_network_nonvolatile_peak: u64,
        ledger_tag_network_volatile: u64,
        ledger_tag_network_volatile_peak: u64,
        ledger_tag_media_footprint: u64,
        ledger_tag_media_footprint_peak: u64,
        ledger_tag_media_nofootprint: u64,
        ledger_tag_media_nofootprint_peak: u64,
        ledger_tag_graphics_footprint: u64,
        ledger_tag_graphics_footprint_peak: u64,
        ledger_tag_graphics_nofootprint: u64,
        ledger_tag_graphics_nofootprint_peak: u64,
        ledger_tag_neural_footprint: u64,
        ledger_tag_neural_footprint_peak: u64,
        ledger_tag_neural_nofootprint: u64,
        ledger_tag_neural_nofootprint_peak: u64,
        limit_bytes_remaining: u64,
        decompressions: u64,
        compressor_bytes_used: u64,
    }
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
    }
    const TASK_VM_INFO: u32 = 4;
    let mut info: TaskVmInfo = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            TASK_VM_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        )
    };
    if kr == 0 {
        info.phys_footprint / 1024
    } else {
        resident_kb_basic()
    }
}

#[cfg(target_os = "macos")]
fn resident_kb_basic() -> u64 {"""
assert t.count(old_fn) == 1
t = t.replace(old_fn, new_fn)

# 冷却：重启后 5 分钟内不再触发，避免阈值设得过低导致反复重启
old_loop = """            let cur = resident_kb();
            if cur > kb {
                log::plog!(
                    "kernel",
                    warn,
                    "常驻 {cur} KB 超过硬上限 {kb} KB，优雅退出等 supervisor 拉起"
                );"""
new_loop = """            let cur = resident_kb();
            // 冷却 5 分钟：阈值设太低时别变成"重启风暴"（实测一个配错的值能 20 分钟重启 59 次）
            if cur > kb && started.elapsed() > std::time::Duration::from_secs(300) {
                log::plog!(
                    "kernel",
                    warn,
                    "常驻 {cur} KB 超过硬上限 {kb} KB，优雅退出等 supervisor 拉起"
                );"""
assert t.count(old_loop) == 1
t = t.replace(old_loop, new_loop)

# 在循环外记 started
t = t.replace("""        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let cur = resident_kb();
            // 冷却 5 分钟""", """        .spawn(move || {
            let started = std::time::Instant::now();
            loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let cur = resident_kb();
            // 冷却 5 分钟""")
# 闭合多出来的一层大括号：把 watchdog 循环尾部补上
t = t.replace("""                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });""", """                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            }
        });""")
m.write_text(t)
print('ok footprint + cooldown')
