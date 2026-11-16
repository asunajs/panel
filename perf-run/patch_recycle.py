from pathlib import Path
BT = chr(96)

e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()

anchor = """fn maintain_plugins(
    rt: &mut Option<Runtime>,"""
add = """/// 内存到阈值了，把运行时都卸掉的请求位（由 main.rs 的回收线程置位）。
///
/// 为什么用请求位而不是直接卸：运行时不是 Send，只能在本线程碰。
/// 排班/任务定义都在调度器和 DB 里，卸掉运行时不会丢任务；下一个触发点前
/// 惰性加载会把它装回来（代价是一次加载，几十毫秒）。
pub static RECYCLE_RUNTIMES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 请求回收：下一次 maintain（约每秒）执行。
pub fn request_runtime_recycle() {
    RECYCLE_RUNTIMES.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn maintain_plugins(
    rt: &mut Option<Runtime>,"""
if 'RECYCLE_RUNTIMES' not in s:
    assert s.count(anchor) == 1
    s = s.replace(anchor, add)
    print('ok engine hook')
else:
    print('engine hook already')

anchor2 = '    // 返回"这一趟有没有改过状态"：调用点据此刷新 ' + BT + 'registry' + BT + ' 快照。'
add2 = """    // 内存到阈值了：把运行时全卸掉（任务、排班、路由声明都保留）。
    if RECYCLE_RUNTIMES.swap(false, std::sync::atomic::Ordering::Relaxed) {
        let mut n = 0;
        for p in plugins.iter_mut() {
            if p.runtime.is_some() {
                evict_runtime(p, host);
                n += 1;
            }
        }
        if n > 0 {
            hostapi::push_log(
                logs,
                "kernel",
                "info",
                &format!("内存达到回收阈值，已卸下 {n} 个插件运行时（排班保留，下次触发前装回）"),
            );
        }
    }

""" + anchor2
if 'RECYCLE_RUNTIMES.swap' not in s:
    assert s.count(anchor2) == 1, s.count(anchor2)
    s = s.replace(anchor2, add2)
    print('ok engine maintain')
else:
    print('engine maintain already')
e.write_text(s)

m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
if 'spawn_mem_recycle' not in t:
    anchor3 = "    spawn_mem_trim();"
    assert t.count(anchor3) == 1
    t = t.replace(anchor3, anchor3 + """
    // 内存**上限**兜底：常驻超过阈值就把所有插件运行时卸掉（排班保留，下次触发前装回）。
    // 这条不试图让分配器别涨，而是给曲线一个上界 —— 实测还页/换分配器都不灵。
    spawn_mem_recycle();""")
    funcs = """

/// 周期回收线程：常驻内存超过 PLUGIN_RECYCLE_KB 就请求卸掉所有插件运行时。
///
/// 默认关（PLUGIN_RECYCLE_KB=0）。建议值：稳态的 1.5 倍，或设备内存的 1/32。
/// 检查间隔 PLUGIN_RECYCLE_MS（默认 30 秒）。
fn spawn_mem_recycle() {
    let kb = std::env::var("PLUGIN_RECYCLE_KB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if kb == 0 {
        return;
    }
    let every = std::env::var("PLUGIN_RECYCLE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1000)
        .unwrap_or(30_000);
    let _ = std::thread::Builder::new()
        .name("mem-recycle".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let cur = resident_kb();
            if cur > kb {
                log::plog!(
                    "kernel",
                    info,
                    "常驻 {cur} KB 超过阈值 {kb} KB，请求回收插件运行时"
                );
                crate::engine::request_runtime_recycle();
            }
        });
}

#[cfg(target_os = "macos")]
fn resident_kb() -> u64 {
    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
    }
    const MACH_TASK_BASIC_INFO: u32 = 20;
    let mut info = MachTaskBasicInfo {
        virtual_size: 0,
        resident_size: 0,
        resident_size_max: 0,
        user_time: [0; 2],
        system_time: [0; 2],
        policy: 0,
        suspend_count: 0,
    };
    let mut count = (std::mem::size_of::<MachTaskBasicInfo>() / 4) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        )
    };
    if kr == 0 {
        info.resident_size / 1024
    } else {
        0
    }
}

#[cfg(target_os = "linux")]
fn resident_kb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
        if let Some(pages) = s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
            return pages * 4;
        }
    }
    0
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_kb() -> u64 {
    0
}
"""
    t = t.rstrip() + funcs + "\n"
    print('ok main recycle')
else:
    print('main recycle already')
m.write_text(t)
