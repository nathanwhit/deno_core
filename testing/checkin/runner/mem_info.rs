use deno_core::serde;
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(crate = "serde")]
pub struct MemInfo {
  pub total: u64,
  pub free: u64,
  pub available: u64,
  pub buffers: u64,
  pub cached: u64,
  pub swap_total: u64,
  pub swap_free: u64,
}

pub fn mem_info() -> Option<MemInfo> {
  let mut mem_info = MemInfo {
    total: 0,
    free: 0,
    available: 0,
    buffers: 0,
    cached: 0,
    swap_total: 0,
    swap_free: 0,
  };
  #[cfg(any(target_os = "android", target_os = "linux"))]
  {
    let mut info = std::mem::MaybeUninit::uninit();
    // SAFETY: `info` is a valid pointer to a `libc::sysinfo` struct.
    let res = unsafe { libc::sysinfo(info.as_mut_ptr()) };
    if res == 0 {
      // SAFETY: `sysinfo` initializes the struct.
      let info = unsafe { info.assume_init() };
      let mem_unit = info.mem_unit as u64;
      mem_info.swap_total = info.totalswap * mem_unit;
      mem_info.swap_free = info.freeswap * mem_unit;
      mem_info.total = info.totalram * mem_unit;
      mem_info.free = info.freeram * mem_unit;
      mem_info.available = mem_info.free;
      mem_info.buffers = info.bufferram * mem_unit;
    }

    // Gets the available memory from /proc/meminfo in linux for compatibility
    #[allow(clippy::disallowed_methods)]
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
      let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"));
      if let Some(line) = line {
        let mem = line.split_whitespace().nth(1);
        let mem = mem.and_then(|v| v.parse::<u64>().ok());
        mem_info.available = mem.unwrap_or(0) * 1024;
      }
    }
  }
  #[cfg(target_vendor = "apple")]
  {
    let mut mib: [i32; 2] = [0, 0];
    mib[0] = libc::CTL_HW;
    mib[1] = libc::HW_MEMSIZE;
    // SAFETY:
    //  - We assume that `mach_host_self` always returns a valid value.
    //  - sysconf returns a system constant.
    unsafe {
      let mut size = std::mem::size_of::<u64>();
      libc::sysctl(
        mib.as_mut_ptr(),
        mib.len() as _,
        &mut mem_info.total as *mut _ as *mut libc::c_void,
        &mut size,
        std::ptr::null_mut(),
        0,
      );

      let mut xs: libc::xsw_usage = std::mem::zeroed::<libc::xsw_usage>();
      mib[0] = libc::CTL_VM;
      mib[1] = libc::VM_SWAPUSAGE;

      let mut size = std::mem::size_of::<libc::xsw_usage>();
      libc::sysctl(
        mib.as_mut_ptr(),
        mib.len() as _,
        &mut xs as *mut _ as *mut libc::c_void,
        &mut size,
        std::ptr::null_mut(),
        0,
      );

      mem_info.swap_total = xs.xsu_total;
      mem_info.swap_free = xs.xsu_avail;

      unsafe extern "C" {
        fn mach_host_self() -> std::ffi::c_uint;
      }

      let mut count: u32 = libc::HOST_VM_INFO64_COUNT as _;
      let mut stat = std::mem::zeroed::<libc::vm_statistics64>();
      if libc::host_statistics64(
        // TODO(@littledivy): Put this in a once_cell.
        mach_host_self(),
        libc::HOST_VM_INFO64,
        &mut stat as *mut libc::vm_statistics64 as *mut _,
        &mut count,
      ) == libc::KERN_SUCCESS
      {
        // TODO(@littledivy): Put this in a once_cell
        let page_size = libc::sysconf(libc::_SC_PAGESIZE) as u64;
        mem_info.available =
          (stat.free_count as u64 + stat.inactive_count as u64) * page_size;
        mem_info.free =
          (stat.free_count as u64 - stat.speculative_count as u64) * page_size;
      }
    }
  }
  #[cfg(target_family = "windows")]
  // SAFETY:
  //   - `mem_status` is a valid pointer to a `libc::MEMORYSTATUSEX` struct.
  //   - `dwLength` is set to the size of the struct.
  unsafe {
    use std::mem;

    use winapi::shared::minwindef;
    use winapi::um::psapi::GetPerformanceInfo;
    use winapi::um::psapi::PERFORMANCE_INFORMATION;
    use winapi::um::sysinfoapi;

    let mut mem_status =
      mem::MaybeUninit::<sysinfoapi::MEMORYSTATUSEX>::uninit();
    let length =
      mem::size_of::<sysinfoapi::MEMORYSTATUSEX>() as minwindef::DWORD;
    (*mem_status.as_mut_ptr()).dwLength = length;

    let result = sysinfoapi::GlobalMemoryStatusEx(mem_status.as_mut_ptr());
    if result != 0 {
      let stat = mem_status.assume_init();
      mem_info.total = stat.ullTotalPhys;
      mem_info.available = 0;
      mem_info.free = stat.ullAvailPhys;
      mem_info.cached = 0;
      mem_info.buffers = 0;

      // `stat.ullTotalPageFile` is reliable only from GetPerformanceInfo()
      //
      // See https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/ns-sysinfoapi-memorystatusex
      // and https://github.com/GuillaumeGomez/sysinfo/issues/534

      let mut perf_info = mem::MaybeUninit::<PERFORMANCE_INFORMATION>::uninit();
      let result = GetPerformanceInfo(
        perf_info.as_mut_ptr(),
        mem::size_of::<PERFORMANCE_INFORMATION>() as minwindef::DWORD,
      );
      if result == minwindef::TRUE {
        let perf_info = perf_info.assume_init();
        let swap_total = perf_info.PageSize
          * perf_info
            .CommitLimit
            .saturating_sub(perf_info.PhysicalTotal);
        let swap_free = perf_info.PageSize
          * perf_info
            .CommitLimit
            .saturating_sub(perf_info.PhysicalTotal)
            .saturating_sub(perf_info.PhysicalAvailable);
        mem_info.swap_total = (swap_total / 1000) as u64;
        mem_info.swap_free = (swap_free / 1000) as u64;
      }
    }
  }

  Some(mem_info)
}
