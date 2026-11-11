// task_vm_info 的前 20 个字段，逐字照 macOS SDK 的 mach/task_info.h（rev1）。
// 用 count = 本结构大小/4 调 task_info，内核只填到这里，phys_footprint 在末尾。
#[repr(C)]
#[derive(Default)]
struct TaskVmInfoRev1 {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
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
}

extern "C" {
    fn task_info(
        target_task: u32,
        flavor: i32,
        task_info_out: *mut i32,
        task_info_out_cnt: *mut u32,
    ) -> i32;
}

const TASK_VM_INFO: i32 = 22;

#[allow(unsafe_code)]
pub fn footprint_mb() -> f64 {
    unsafe {
        let mut info = TaskVmInfoRev1::default();
        let mut count = (std::mem::size_of::<TaskVmInfoRev1>() / 4) as u32;
        let kr = task_info(
            mach2::traps::current_task(),
            TASK_VM_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        );
        assert_eq!(kr, 0, "task_info failed: {kr}");
        info.phys_footprint as f64 / (1024.0 * 1024.0)
    }
}