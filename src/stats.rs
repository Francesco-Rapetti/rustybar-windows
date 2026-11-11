//! What the PC is doing, read in-process from Windows itself, without WMI or performance counters:
//! the CPU from `GetSystemTimes`, the memory from `GlobalMemoryStatusEx`, the GPU from the graphics
//! kernel (`D3DKMT*`, what Task Manager reads), the disks from the file system and the processes
//! from one `NtQuerySystemInformation` call. Each reading is a few system calls; the processes,
//! the dearest (a few ms for all of them), are for the open popup only.

use std::collections::HashMap;
use std::time::Instant;

use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_ADAPTER_PERFDATACAPS, D3DKMT_ADAPTERINFO, D3DKMT_ADAPTERREGISTRYINFO, D3DKMT_ADAPTERTYPE,
    D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2, D3DKMT_NODEMETADATA, D3DKMT_QUERYADAPTERINFO, D3DKMT_QUERYSTATISTICS,
    D3DKMT_QUERYSTATISTICS_ADAPTER, D3DKMT_QUERYSTATISTICS_NODE, D3DKMT_QUERYSTATISTICS_SEGMENT, D3DKMT_SEGMENTSIZEINFO,
    D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo, D3DKMTQueryStatistics, DXGK_ENGINE_TYPE_3D,
    KMTQAITYPE_ADAPTERPERFDATA, KMTQAITYPE_ADAPTERPERFDATA_CAPS, KMTQAITYPE_ADAPTERREGISTRYINFO, KMTQAITYPE_ADAPTERTYPE,
    KMTQAITYPE_GETSEGMENTSIZE, KMTQAITYPE_NODEMETADATA, KMTQUERYADAPTERINFOTYPE,
};
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::Foundation::{FILETIME, LUID, STATUS_INFO_LENGTH_MISMATCH};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GlobalMemoryStatusEx, MEMORYSTATUSEX, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetSystemTimes};

use windows::core::PCWSTR;

use crate::log;

/// `GetDriveTypeW`: a disk that stays in the PC (in `Win32_System_WindowsProgramming`, a big part
/// of the `windows` crate for one number).
const DRIVE_FIXED: u32 = 3;

// ---- CPU ---------------------------------------------------------------------------------------

/// The CPU's time since boot, in 100 ns, summed over every logical processor.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTimes {
    pub idle: u64,
    /// Kernel time, without the idle time Windows counts in it.
    pub kernel: u64,
    pub user: u64,
}

impl CpuTimes {
    pub fn read() -> Option<CpuTimes> {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()? };
        let v = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
        let idle = v(idle);
        Some(CpuTimes { idle, kernel: v(kernel).saturating_sub(idle), user: v(user) })
    }
}

/// The share of the time between two readings spent in the kernel, in apps and idle, 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuShare {
    pub kernel: f64,
    pub user: f64,
    pub idle: f64,
}

impl CpuShare {
    pub fn between(old: CpuTimes, new: CpuTimes) -> Option<CpuShare> {
        let idle = new.idle.saturating_sub(old.idle) as f64;
        let kernel = new.kernel.saturating_sub(old.kernel) as f64;
        let user = new.user.saturating_sub(old.user) as f64;
        let total = idle + kernel + user;
        (total > 0.0).then(|| CpuShare { kernel: kernel / total, user: user / total, idle: idle / total })
    }

    pub fn busy(&self) -> f64 {
        1.0 - self.idle
    }
}

/// Logical processors, and physical cores.
pub fn processors() -> (u32, u32) {
    let logical = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1);
    let mut len = 0u32;
    unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len);
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let ptr = buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
    if len == 0 || unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, Some(ptr), &mut len) }.is_err() {
        return (logical, logical);
    }
    // One record per core, each as long as its `Size`
    let (mut offset, mut cores) = (0usize, 0u32);
    while offset + 8 <= len as usize {
        let size = unsafe { (*((buf.as_ptr() as *const u8).add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)).Size } as usize;
        if size == 0 {
            break;
        }
        cores += 1;
        offset += size;
    }
    (logical, cores.max(1))
}

// ---- memory ------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    /// What the processes have committed, and the most they can (the RAM plus the page file).
    pub committed: u64,
    pub commit_limit: u64,
}

impl Memory {
    pub fn read() -> Option<Memory> {
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        unsafe { GlobalMemoryStatusEx(&mut m).ok()? };
        Some(Memory {
            total: m.ullTotalPhys,
            available: m.ullAvailPhys,
            committed: m.ullTotalPageFile.saturating_sub(m.ullAvailPageFile),
            commit_limit: m.ullTotalPageFile,
        })
    }

    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn percent(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.used() as f64 / self.total as f64 * 100.0 }
    }
}

// ---- GPU ---------------------------------------------------------------------------------------

/// The GPU, through the graphics kernel: the hardware adapter with the most video memory (the
/// discrete one, on a PC that also has one in the CPU). Its use is that of its 3D engines, the
/// ones games draw with: every engine read is a call into the driver (~0.2 ms each on a Radeon,
/// and it has 15), so the others (video, copies, compute) are left out.
pub struct Gpu {
    luid: LUID,
    adapter: u32,
    pub name: String,
    /// Dedicated video memory, in bytes.
    pub dedicated: u64,
    segments: u32,
    nodes: Vec<u32>,
    /// The engines' running time at the last reading, in 100 ns, and when.
    last: Option<(Vec<i64>, Instant)>,
    /// The temperature the driver says is high, in °C, if it says.
    pub hot: Option<f32>,
}

fn query<T>(adapter: u32, kind: KMTQUERYADAPTERINFOTYPE, data: &mut T) -> bool {
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter,
        Type: kind,
        pPrivateDriverData: data as *mut T as *mut _,
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok()
}

fn close_adapter(adapter: u32) {
    let close = D3DKMT_CLOSEADAPTER { hAdapter: adapter };
    unsafe {
        let _ = D3DKMTCloseAdapter(&close);
    }
}

impl Gpu {
    pub fn open() -> Option<Gpu> {
        let mut list = D3DKMT_ENUMADAPTERS2::default();
        unsafe {
            let _ = D3DKMTEnumAdapters2(&mut list);
        }
        let mut adapters = vec![D3DKMT_ADAPTERINFO::default(); list.NumAdapters as usize];
        list.pAdapters = adapters.as_mut_ptr();
        if adapters.is_empty() || unsafe { D3DKMTEnumAdapters2(&mut list) }.is_err() {
            return None;
        }
        adapters.truncate(list.NumAdapters as usize);
        // The adapter handles come open: keep the chosen one, close the others
        let mut best: Option<(D3DKMT_ADAPTERINFO, u64)> = None;
        for a in &adapters {
            let mut kind = D3DKMT_ADAPTERTYPE::default();
            let software = !query(a.hAdapter, KMTQAITYPE_ADAPTERTYPE, &mut kind) || unsafe { kind.Anonymous.Value } & 0x4 != 0;
            let mut size = D3DKMT_SEGMENTSIZEINFO::default();
            let dedicated = if query(a.hAdapter, KMTQAITYPE_GETSEGMENTSIZE, &mut size) { size.DedicatedVideoMemorySize } else { 0 };
            if !software && best.is_none_or(|(_, d)| dedicated > d) {
                if let Some((old, _)) = best {
                    close_adapter(old.hAdapter);
                }
                best = Some((*a, dedicated));
            } else {
                close_adapter(a.hAdapter);
            }
        }
        let (a, dedicated) = best?;
        let mut registry = D3DKMT_ADAPTERREGISTRYINFO { AdapterString: [0; 260], BiosString: [0; 260], DacType: [0; 260], ChipType: [0; 260] };
        let name = if query(a.hAdapter, KMTQAITYPE_ADAPTERREGISTRYINFO, &mut registry) { wide(&registry.AdapterString) } else { String::new() };
        let stats = D3DKMT_QUERYSTATISTICS { Type: D3DKMT_QUERYSTATISTICS_ADAPTER, AdapterLuid: a.AdapterLuid, ..Default::default() };
        if unsafe { D3DKMTQueryStatistics(&stats) }.is_err() {
            close_adapter(a.hAdapter);
            return None;
        }
        let info = unsafe { stats.QueryResult.AdapterInformation };
        let mut nodes: Vec<u32> = (0..info.NodeCount)
            .filter(|&n| {
                let mut meta = D3DKMT_NODEMETADATA { NodeOrdinalAndAdapterIndex: n, ..Default::default() };
                query(a.hAdapter, KMTQAITYPE_NODEMETADATA, &mut meta) && { meta.NodeData.EngineType } == DXGK_ENGINE_TYPE_3D
            })
            .collect();
        // A driver that doesn't describe its engines: the first is the 3D one
        if nodes.is_empty() && info.NodeCount > 0 {
            nodes.push(0);
        }
        let mut caps = D3DKMT_ADAPTER_PERFDATACAPS::default();
        let hot = (query(a.hAdapter, KMTQAITYPE_ADAPTERPERFDATA_CAPS, &mut caps) && caps.TemperatureWarning > 0)
            .then(|| caps.TemperatureWarning as f32 / 10.0);
        log!("GPU: {name}, {} MB, 3D engines {nodes:?} of {}", dedicated >> 20, info.NodeCount);
        Some(Gpu { luid: a.AdapterLuid, adapter: a.hAdapter, name, dedicated, segments: info.NbSegments, nodes, last: None, hot })
    }

    /// How busy the 3D engines were since the last reading, 0 to 100: the busiest one. None the
    /// first time.
    pub fn usage(&mut self) -> Option<f64> {
        let now = Instant::now();
        let times: Vec<i64> = self
            .nodes
            .iter()
            .map(|&node| {
                let mut stats = D3DKMT_QUERYSTATISTICS { Type: D3DKMT_QUERYSTATISTICS_NODE, AdapterLuid: self.luid, ..Default::default() };
                stats.Anonymous.QueryNode.NodeId = node;
                if unsafe { D3DKMTQueryStatistics(&stats) }.is_err() {
                    return 0;
                }
                unsafe { stats.QueryResult.NodeInformation.GlobalInformation.RunningTime }
            })
            .collect();
        let usage = self.last.as_ref().and_then(|(before, at)| {
            let wall = now.duration_since(*at).as_nanos() as f64 / 100.0;
            (wall > 0.0).then(|| times.iter().zip(before).map(|(t, b)| (t - b).max(0) as f64 / wall * 100.0).fold(0.0, f64::max).min(100.0))
        });
        self.last = Some((times, now));
        usage
    }

    /// The temperature, in °C, if the driver tells it (as it does to Task Manager).
    pub fn temperature(&self) -> Option<f32> {
        let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
        (query(self.adapter, KMTQAITYPE_ADAPTERPERFDATA, &mut perf) && perf.Temperature > 0).then(|| perf.Temperature as f32 / 10.0)
    }

    /// The dedicated video memory in use, in bytes: what's resident in the segments that aren't
    /// system memory seen through an aperture.
    pub fn memory_used(&self) -> u64 {
        (0..self.segments)
            .filter_map(|segment| {
                let mut stats = D3DKMT_QUERYSTATISTICS { Type: D3DKMT_QUERYSTATISTICS_SEGMENT, AdapterLuid: self.luid, ..Default::default() };
                stats.Anonymous.QuerySegment.SegmentId = segment;
                unsafe { D3DKMTQueryStatistics(&stats) }.ok().ok()?;
                let info = unsafe { stats.QueryResult.SegmentInformation };
                (info.Aperture == 0).then_some(info.BytesResident)
            })
            .sum()
    }

    /// The readings start again (after a sleep): the next one has nothing to compare with.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        close_adapter(self.adapter);
    }
}

fn wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

// ---- disks -------------------------------------------------------------------------------------

/// A fixed disk, in bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Disk {
    /// `C:\`
    pub root: String,
    /// The volume's label, if it has one.
    pub label: String,
    pub total: u64,
    pub free: u64,
}

impl Disk {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn percent(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.used() as f64 / self.total as f64 * 100.0 }
    }
}

/// The fixed disks with a drive letter. Network shares and removable drives are left out: a share
/// that doesn't answer could hold the read for seconds.
pub fn disks() -> Vec<Disk> {
    let mut buf = [0u16; 512];
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return Vec::new();
    }
    buf[..n]
        .split(|c| *c == 0)
        .filter(|root| !root.is_empty())
        .filter_map(|root| {
            let mut z: Vec<u16> = root.to_vec();
            z.push(0);
            let path = PCWSTR(z.as_ptr());
            if unsafe { GetDriveTypeW(path) } != DRIVE_FIXED {
                return None;
            }
            let (mut free, mut total) = (0u64, 0u64);
            unsafe { GetDiskFreeSpaceExW(path, Some(&mut free), Some(&mut total), None).ok()? };
            let mut label = [0u16; 64];
            let label = if unsafe { GetVolumeInformationW(path, Some(&mut label), None, None, None, None) }.is_ok() { wide(&label) } else { String::new() };
            Some(Disk { root: String::from_utf16_lossy(root), label, total, free })
        })
        .collect()
}

// ---- processes ---------------------------------------------------------------------------------

/// The start of `SYSTEM_PROCESS_INFORMATION`, as it has been since Windows Vista; the documented
/// struct hides the times and the private working set in reserved fields.
#[repr(C)]
struct ProcessRecord {
    next: u32,
    threads: u32,
    private_working_set: i64,
    hard_faults: u32,
    threads_high: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    name_length: u16,
    name_capacity: u16,
    name: *const u16,
    base_priority: i32,
    pid: usize,
}

/// An app in the lists: every process of one executable together, as Task Manager groups them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct App {
    /// The executable's file name, lowercase.
    pub exe: String,
    /// One of its processes, to find the executable's path (and its name).
    pub pid: u32,
    /// Share of the whole CPU since the last reading, 0 to 100.
    pub cpu: f64,
    /// Private working set, in bytes (Task Manager's memory column).
    pub memory: u64,
}

/// The CPU time of each process (kernel and user, in 100 ns), by its id and when it started (an id
/// can be used again).
type Times = HashMap<(usize, i64), i64>;

/// Reads the processes, again and again while a popup is open: the CPU of each is the difference
/// from the last reading.
pub struct Processes {
    buf: Vec<u64>,
    last: Option<(Times, Instant)>,
    logical: f64,
}

impl Processes {
    pub fn new(logical: u32) -> Processes {
        Processes { buf: Vec::new(), last: None, logical: logical as f64 }
    }

    /// Every app, with its CPU since the last call (0 the first time) and its memory.
    pub fn read(&mut self) -> Option<Vec<App>> {
        let len = self.fill()?;
        let now = Instant::now();
        let elapsed = self.last.as_ref().map(|(_, at)| now.duration_since(*at).as_nanos() as f64 / 100.0);
        let mut times = HashMap::new();
        let mut apps: HashMap<String, App> = HashMap::new();
        let base = self.buf.as_ptr() as *const u8;
        let mut offset = 0usize;
        loop {
            if offset + size_of::<ProcessRecord>() > len {
                break;
            }
            let p = unsafe { std::ptr::read_unaligned(base.add(offset) as *const ProcessRecord) };
            // The idle process has no name; its time is the CPU's idle time
            if p.pid != 0 && p.name_length > 0 && !p.name.is_null() {
                let name = unsafe { std::slice::from_raw_parts(p.name, p.name_length as usize / 2) };
                let exe = String::from_utf16_lossy(name).to_lowercase();
                let time = p.user_time + p.kernel_time;
                times.insert((p.pid, p.create_time), time);
                let cpu = match (&self.last, elapsed) {
                    (Some((before, _)), Some(wall)) if wall > 0.0 => {
                        before.get(&(p.pid, p.create_time)).map_or(0.0, |b| (time - b).max(0) as f64 / wall / self.logical * 100.0)
                    }
                    _ => 0.0,
                };
                let app = apps.entry(exe.clone()).or_insert_with(|| App { exe, pid: p.pid as u32, ..Default::default() });
                app.cpu += cpu;
                app.memory += p.private_working_set.max(0) as u64;
            }
            if p.next == 0 {
                break;
            }
            offset += p.next as usize;
        }
        self.last = Some((times, now));
        Some(apps.into_values().collect())
    }

    /// Asks for every process into the buffer, growing it as needed; its length in bytes.
    fn fill(&mut self) -> Option<usize> {
        if self.buf.is_empty() {
            self.buf = vec![0u64; 128 * 1024];
        }
        for _ in 0..4 {
            let mut needed = 0u32;
            let size = (self.buf.len() * 8) as u32;
            let status = unsafe { NtQuerySystemInformation(SystemProcessInformation, self.buf.as_mut_ptr() as *mut _, size, &mut needed) };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                // Room for the processes that may start before the next try
                self.buf = vec![0u64; (needed as usize + 64 * 1024).div_ceil(8)];
                continue;
            }
            return status.is_ok().then_some(needed as usize);
        }
        None
    }
}

/// The apps that use the most of what `key` gives, at most `n`, above `floor`.
pub fn top<K: PartialOrd + Copy>(apps: &[App], n: usize, floor: K, key: impl Fn(&App) -> K) -> Vec<App> {
    let mut list: Vec<&App> = apps.iter().filter(|a| key(a) >= floor).collect();
    list.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
    list.into_iter().take(n).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_share_between_readings() {
        let a = CpuTimes { idle: 1000, kernel: 100, user: 200 };
        let b = CpuTimes { idle: 1600, kernel: 200, user: 500 };
        let s = CpuShare::between(a, b).unwrap();
        assert!((s.idle - 0.6).abs() < 1e-9);
        assert!((s.busy() - 0.4).abs() < 1e-9);
        assert!((s.user - 0.3).abs() < 1e-9);
        assert_eq!(CpuShare::between(a, a), None);
    }

    #[test]
    fn the_record_matches_windows() {
        // The offsets of SYSTEM_PROCESS_INFORMATION on 64-bit Windows
        assert_eq!(std::mem::offset_of!(ProcessRecord, create_time), 32);
        assert_eq!(std::mem::offset_of!(ProcessRecord, name_length), 56);
        assert_eq!(std::mem::offset_of!(ProcessRecord, name), 64);
        assert_eq!(std::mem::offset_of!(ProcessRecord, pid), 80);
    }

    #[test]
    fn top_apps() {
        let app = |exe: &str, cpu: f64| App { exe: exe.into(), cpu, ..Default::default() };
        let apps = vec![app("a", 1.0), app("b", 5.0), app("c", 0.01), app("d", 3.0)];
        let top = top(&apps, 2, 0.1, |a| a.cpu);
        assert_eq!(top.iter().map(|a| a.exe.as_str()).collect::<Vec<_>>(), ["b", "d"]);
    }

    #[test]
    fn the_processes_can_be_read() {
        let (logical, cores) = processors();
        assert!(logical >= cores && cores >= 1);
        let mut p = Processes::new(logical);
        let apps = p.read().expect("processes");
        assert!(apps.iter().any(|a| a.exe == "system"));
        assert!(CpuTimes::read().is_some());
        assert!(Memory::read().is_some_and(|m| m.total > 0 && m.used() <= m.total));
        assert!(disks().iter().all(|d| d.free <= d.total));
    }
}
