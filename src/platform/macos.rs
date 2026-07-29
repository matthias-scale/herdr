use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Write;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

pub(crate) fn sample_status_metrics(
    sampler: &mut super::status_metrics::StatusMetricSampler,
) -> super::status_metrics::StatusMetrics {
    let (hostname, username) = status_identity();
    let (date, time) = status_local_date_time();
    let total_bytes = status_sysctl_u64(c"hw.memsize");
    let mem_total_gib = total_bytes.map(|value| value as f32 / 1_073_741_824.0);
    let mem_used_gib = total_bytes
        .and_then(status_used_memory_bytes)
        .map(|value| value as f32 / 1_073_741_824.0);
    let cpu_percent = status_cpu_ticks().and_then(|(idle, total)| sampler.cpu_percent(idle, total));
    let (battery_percent, battery_charging) = status_battery();
    let interfaces = status_interface_ipv4s();
    let (net_down_kib, net_up_kib) = interfaces
        .primary
        .as_deref()
        .zip(interfaces.primary_bytes)
        .and_then(|(interface, (rx, tx))| {
            sampler.bandwidth_kib(interface, rx, tx, std::time::Instant::now())
        })
        .map_or((None, None), |(down, up)| (Some(down), Some(up)));

    super::status_metrics::StatusMetrics {
        cpu_percent,
        mem_used_gib,
        mem_total_gib,
        battery_percent,
        battery_charging,
        local_ip: interfaces.local_ip,
        tailscale_ip: interfaces.tailscale_ip.clone(),
        public_ip: super::status_metrics::compatible_public_ip(),
        net_down_kib,
        net_up_kib,
        net_kind: interfaces
            .primary
            .as_deref()
            .map(status_net_kind)
            .unwrap_or_default(),
        vpn_active: interfaces.vpn_active,
        remote_session: super::status_metrics::remote_session_from_env(),
        hostname,
        username,
        date,
        time,
    }
}

/// `pmset` is the stable system battery surface available without adding an
/// IOKit binding dependency. The metrics worker remains bounded by killing the
/// local command after a short deadline and accepts only a small output.
fn status_battery() -> (Option<u8>, Option<bool>) {
    const PMSET_TIMEOUT: Duration = Duration::from_millis(250);
    const PMSET_OUTPUT_MAX_BYTES: usize = 4096;

    let mut command = crate::noninteractive_process::command("/usr/bin/pmset");
    let mut child = match command
        .args(["-g", "batt"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return (None, None),
    };
    if !wait_for_child_until(&mut child, PMSET_TIMEOUT) {
        return (None, None);
    }

    let Some(stdout) = child.stdout.take() else {
        return (None, None);
    };
    let bytes = match read_limited_reader(stdout, PMSET_OUTPUT_MAX_BYTES) {
        Ok(LimitedRead::Complete(bytes)) => bytes,
        Ok(LimitedRead::Empty | LimitedRead::Oversized) | Err(_) => return (None, None),
    };
    std::str::from_utf8(&bytes)
        .ok()
        .map(parse_pmset_battery)
        .unwrap_or((None, None))
}

trait BoundedChild {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    fn kill(&mut self) -> std::io::Result<()>;
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus>;
}

impl BoundedChild for std::process::Child {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        std::process::Child::try_wait(self)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        std::process::Child::kill(self)
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        std::process::Child::wait(self)
    }
}

fn kill_and_reap_child(child: &mut impl BoundedChild) {
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_for_child_until(child: &mut impl BoundedChild, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Err(_) => {
                kill_and_reap_child(child);
                return false;
            }
            Ok(None) if Instant::now() >= deadline => {
                kill_and_reap_child(child);
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn parse_pmset_battery(output: &str) -> (Option<u8>, Option<bool>) {
    for line in output.lines() {
        let Some((before_percent, after_percent)) = line.split_once("%;") else {
            continue;
        };
        let percent_text = before_percent
            .trim_end()
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        if percent_text.is_empty() {
            continue;
        }
        let Ok(percent) = percent_text.parse::<u8>() else {
            continue;
        };
        if percent > 100 {
            continue;
        }
        let state = after_percent
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let charging = if state.contains("discharging") {
            Some(false)
        } else if state == "charged"
            || state.contains("charging")
            || state.contains("finishing charge")
        {
            Some(true)
        } else {
            None
        };
        return (Some(percent), charging);
    }
    (None, None)
}

fn status_identity() -> (String, String) {
    let mut hostname = [0u8; 256];
    // SAFETY: `hostname` is writable for the length passed to libc.
    let host = if unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } == 0 {
        let end = hostname
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(hostname.len());
        super::status_metrics::short_hostname(&String::from_utf8_lossy(&hostname[..end]))
    } else {
        "localhost".into()
    };
    let user = std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".into());
    (host, user)
}

fn status_local_date_time() -> (String, String) {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    // SAFETY: `local` is valid writable storage and `now` is a valid time value.
    if unsafe { libc::localtime_r(&now, &mut local) }.is_null() {
        return ("----/--/--".into(), "--:--".into());
    }
    (
        format!(
            "{:04}-{:02}-{:02}",
            local.tm_year + 1900,
            local.tm_mon + 1,
            local.tm_mday
        ),
        format!("{:02}:{:02}", local.tm_hour, local.tm_min),
    )
}

fn status_sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut value = 0u64;
    let mut size = std::mem::size_of::<u64>();
    // SAFETY: name and output pointers reference valid storage.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && size == std::mem::size_of::<u64>()).then_some(value)
}

type StatusMachPortDeallocator =
    unsafe fn(libc::mach_port_t, libc::mach_port_t) -> libc::kern_return_t;

struct StatusMachHostPort {
    port: libc::mach_port_t,
    deallocate: StatusMachPortDeallocator,
}

impl StatusMachHostPort {
    fn acquire() -> Option<Self> {
        // SAFETY: `mach_host_self` returns a send right owned by the caller.
        let port = unsafe { status_mach_host_self() };
        (port != 0).then_some(Self {
            port,
            deallocate: deallocate_status_mach_port,
        })
    }

    fn get(&self) -> libc::mach_port_t {
        self.port
    }
}

impl Drop for StatusMachHostPort {
    fn drop(&mut self) {
        // SAFETY: this guard owns exactly one send right returned by
        // `mach_host_self`; the task-self port is process-owned and borrowed.
        let _ = unsafe { (self.deallocate)(status_mach_task_self(), self.port) };
    }
}

unsafe fn deallocate_status_mach_port(
    task: libc::mach_port_t,
    name: libc::mach_port_t,
) -> libc::kern_return_t {
    // SAFETY: the caller guarantees that `name` is an owned send right in `task`.
    unsafe { status_mach_port_deallocate(task, name) }
}

unsafe fn status_mach_task_self() -> libc::mach_port_t {
    // SAFETY: the kernel owns this process-global task-self port name.
    unsafe { STATUS_MACH_TASK_SELF }
}

fn status_used_memory_bytes(total: u64) -> Option<u64> {
    const HOST_VM_INFO64: libc::c_int = 4;
    #[repr(C)]
    #[derive(Default)]
    struct VmStatistics64 {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }
    let mut stats = VmStatistics64::default();
    let mut count = (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<libc::integer_t>())
        as libc::mach_msg_type_number_t;
    let host = StatusMachHostPort::acquire()?;
    let result = unsafe {
        status_host_statistics64(
            host.get(),
            HOST_VM_INFO64,
            (&raw mut stats).cast(),
            &mut count,
        )
    };
    if result != 0 {
        return None;
    }
    let page_size = status_sysctl_u64(c"hw.pagesize").unwrap_or(4096);
    let pages = u64::from(stats.internal_page_count)
        .saturating_add(u64::from(stats.wire_count))
        .saturating_add(u64::from(stats.compressor_page_count));
    Some(pages.saturating_mul(page_size).min(total))
}

fn status_cpu_ticks() -> Option<(u64, u64)> {
    const HOST_CPU_LOAD_INFO: libc::c_int = 3;
    #[repr(C)]
    struct CpuLoad {
        ticks: [u32; 4],
    }
    let mut load = CpuLoad { ticks: [0; 4] };
    let mut count = 4;
    let host = StatusMachHostPort::acquire()?;
    let result = unsafe {
        status_host_statistics(
            host.get(),
            HOST_CPU_LOAD_INFO,
            (&raw mut load).cast(),
            &mut count,
        )
    };
    if result != 0 {
        return None;
    }
    Some((
        u64::from(load.ticks[2]),
        load.ticks.iter().map(|tick| u64::from(*tick)).sum(),
    ))
}

/// `net/if_media.h` packs its request structure to 4 bytes, so the layout and
/// the derived `SIOCGIFMEDIA` request code must both use that packing.
#[repr(C, packed(4))]
// The trailing fields exist to match the kernel layout; only `ifm_active` is read back.
#[allow(dead_code)]
struct StatusIfMediaRequest {
    ifm_name: [libc::c_char; libc::IFNAMSIZ],
    ifm_current: libc::c_int,
    ifm_mask: libc::c_int,
    ifm_status: libc::c_int,
    ifm_active: libc::c_int,
    ifm_count: libc::c_int,
    ifm_ulist: *mut libc::c_int,
}

const IFM_NMASK: libc::c_int = 0x0000_00e0;
const IFM_ETHER: libc::c_int = 0x0000_0020;
const IFM_IEEE80211: libc::c_int = 0x0000_0080;
const SIOCGIFMEDIA: libc::c_ulong =
    status_iowr(b'i', 56, std::mem::size_of::<StatusIfMediaRequest>());

const fn status_iowr(group: u8, number: u8, size: usize) -> libc::c_ulong {
    const IOC_IN: libc::c_ulong = 0x8000_0000;
    const IOC_OUT: libc::c_ulong = 0x4000_0000;
    const IOCPARM_MASK: libc::c_ulong = 0x1fff;

    IOC_IN
        | IOC_OUT
        | (((size as libc::c_ulong) & IOCPARM_MASK) << 16)
        | ((group as libc::c_ulong) << 8)
        | number as libc::c_ulong
}

/// Resolve the link media type instead of guessing from the interface name:
/// `en0` is Wi-Fi on portables but built-in Ethernet on desktop Macs, and
/// docked links land on arbitrary `enN` names. Anything the kernel does not
/// report as Ethernet or 802.11 stays unknown rather than mislabelled.
fn status_net_kind(interface: &str) -> super::status_metrics::NetKind {
    let name = interface.as_bytes();
    if name.is_empty() || name.len() >= libc::IFNAMSIZ {
        return super::status_metrics::NetKind::Unknown;
    }

    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if socket < 0 {
        return super::status_metrics::NetKind::Unknown;
    }
    let mut ifm_name = [0 as libc::c_char; libc::IFNAMSIZ];
    for (slot, byte) in ifm_name.iter_mut().zip(name) {
        *slot = *byte as libc::c_char;
    }
    let mut request: StatusIfMediaRequest = unsafe { std::mem::zeroed() };
    request.ifm_name = ifm_name;
    let result = unsafe { libc::ioctl(socket, SIOCGIFMEDIA, &raw mut request) };
    let active = request.ifm_active;
    unsafe { libc::close(socket) };
    if result != 0 {
        return super::status_metrics::NetKind::Unknown;
    }

    match active & IFM_NMASK {
        IFM_IEEE80211 => super::status_metrics::NetKind::Wifi,
        IFM_ETHER => super::status_metrics::NetKind::Ethernet,
        _ => super::status_metrics::NetKind::Unknown,
    }
}

struct StatusNetworkInterfaces {
    local_ip: Option<String>,
    tailscale_ip: Option<String>,
    primary: Option<String>,
    primary_bytes: Option<(u64, u64)>,
    vpn_active: bool,
}

struct StatusInterfaceIpv4 {
    name: String,
    address: Ipv4Addr,
    up: bool,
}

struct StatusIfAddrs(NonNull<libc::ifaddrs>);

impl StatusIfAddrs {
    fn acquire() -> Option<Self> {
        let mut interfaces: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: libc initializes the list pointer on success and retains
        // ownership until the paired `freeifaddrs` in this guard's Drop.
        if unsafe { libc::getifaddrs(&mut interfaces) } != 0 {
            return None;
        }
        NonNull::new(interfaces).map(Self)
    }

    fn first(&self) -> *mut libc::ifaddrs {
        self.0.as_ptr()
    }
}

impl Drop for StatusIfAddrs {
    fn drop(&mut self) {
        // SAFETY: `acquire` gives this guard sole ownership of the list.
        unsafe { libc::freeifaddrs(self.0.as_ptr()) };
    }
}

fn status_default_route_interface() -> Option<String> {
    const ROUTE_TIMEOUT: Duration = Duration::from_millis(250);
    const ROUTE_OUTPUT_MAX_BYTES: usize = 4096;

    // `route get` reads the kernel's local routing table; it performs no
    // network I/O. Keep the helper bounded because this runs in the sampler.
    let mut command = crate::noninteractive_process::command("/sbin/route");
    let mut child = command
        .args(["-n", "get", "default"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if !wait_for_child_until(&mut child, ROUTE_TIMEOUT) {
        return None;
    }
    let stdout = child.stdout.take()?;
    let bytes = match read_limited_reader(stdout, ROUTE_OUTPUT_MAX_BYTES).ok()? {
        LimitedRead::Complete(bytes) => bytes,
        LimitedRead::Empty | LimitedRead::Oversized => return None,
    };
    parse_default_route_interface(std::str::from_utf8(&bytes).ok()?)
}

fn parse_default_route_interface(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (label, value) = line.split_once(':')?;
        if label.trim() != "interface" {
            return None;
        }
        let interface = value.split_whitespace().next()?;
        (!interface.is_empty() && interface.len() < libc::IFNAMSIZ).then(|| interface.to_string())
    })
}

fn status_interface_name(interface: &libc::ifaddrs) -> Option<String> {
    if interface.ifa_name.is_null() {
        return None;
    }
    // SAFETY: every node in a live getifaddrs list owns a NUL-terminated name.
    Some(
        unsafe { std::ffi::CStr::from_ptr(interface.ifa_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn status_interface_bytes(interface: &libc::ifaddrs) -> Option<(u64, u64)> {
    if interface.ifa_data.is_null() {
        return None;
    }
    let data = interface.ifa_data.cast::<libc::if_data>();
    // SAFETY: Darwin documents AF_LINK `ifa_data` as `struct if_data`.
    // `if_data` is packed to four bytes, so copy both fields unaligned rather
    // than creating references whose alignment Rust cannot guarantee.
    let rx = unsafe { std::ptr::addr_of!((*data).ifi_ibytes).read_unaligned() };
    let tx = unsafe { std::ptr::addr_of!((*data).ifi_obytes).read_unaligned() };
    Some((u64::from(rx), u64::from(tx)))
}

fn status_interface_ipv4s() -> StatusNetworkInterfaces {
    let Some(interfaces) = StatusIfAddrs::acquire() else {
        return StatusNetworkInterfaces {
            local_ip: None,
            tailscale_ip: None,
            primary: None,
            primary_bytes: None,
            vpn_active: false,
        };
    };
    let mut ipv4s = Vec::new();
    let mut counters = HashMap::new();
    let mut current = interfaces.first();
    while !current.is_null() {
        // SAFETY: `current` belongs to the live list guarded by `interfaces`.
        let interface = unsafe { &*current };
        let family = if interface.ifa_addr.is_null() {
            None
        } else {
            // SAFETY: a non-null ifa_addr points to a sockaddr for this node.
            Some(unsafe { (*interface.ifa_addr).sa_family as i32 })
        };
        if family == Some(libc::AF_INET) {
            // SAFETY: the family check above establishes sockaddr_in layout.
            let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in) };
            let address = Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr));
            if let Some(name) = status_interface_name(interface) {
                ipv4s.push(StatusInterfaceIpv4 {
                    name,
                    address,
                    up: interface.ifa_flags & (libc::IFF_UP as u32) != 0,
                });
            }
        } else if family == Some(libc::AF_LINK) {
            if let (Some(name), Some(bytes)) = (
                status_interface_name(interface),
                status_interface_bytes(interface),
            ) {
                counters.insert(name, bytes);
            }
        }
        current = interface.ifa_next;
    }
    drop(interfaces);

    select_status_network_interfaces(status_default_route_interface().as_deref(), ipv4s, counters)
}

fn select_status_network_interfaces(
    default_interface: Option<&str>,
    ipv4s: Vec<StatusInterfaceIpv4>,
    mut counters: HashMap<String, (u64, u64)>,
) -> StatusNetworkInterfaces {
    let mut tailscale_ip = None;
    let mut vpn_active = false;
    let mut candidates = Vec::new();
    for interface in ipv4s {
        if !interface.up || interface.address.is_loopback() || interface.address.is_unspecified() {
            continue;
        }
        let tunnel = status_tunnel_interface(&interface.name);
        vpn_active |= tunnel;
        let octets = interface.address.octets();
        if tunnel && octets[0] == 100 && (64..=127).contains(&octets[1]) {
            tailscale_ip.get_or_insert_with(|| interface.address.to_string());
        }
        candidates.push(interface);
    }

    let selected = default_interface
        .and_then(|name| {
            candidates
                .iter()
                .position(|interface| interface.name == name)
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|interface| !status_tunnel_interface(&interface.name))
        })
        .and_then(|index| candidates.get(index));

    let Some(selected) = selected else {
        return StatusNetworkInterfaces {
            local_ip: None,
            tailscale_ip,
            primary: None,
            primary_bytes: None,
            vpn_active,
        };
    };
    let primary = selected.name.clone();
    let local_ip = selected.address.to_string();
    let primary_bytes = counters.remove(&primary);
    StatusNetworkInterfaces {
        local_ip: Some(local_ip),
        tailscale_ip,
        primary: Some(primary),
        primary_bytes,
        vpn_active,
    }
}

fn status_tunnel_interface(name: &str) -> bool {
    name.starts_with("utun") || name.starts_with("tun") || name.contains("tailscale")
}

unsafe extern "C" {
    #[link_name = "mach_task_self_"]
    static STATUS_MACH_TASK_SELF: libc::mach_port_t;
    #[link_name = "mach_host_self"]
    fn status_mach_host_self() -> libc::mach_port_t;
    #[link_name = "mach_port_deallocate"]
    fn status_mach_port_deallocate(
        task: libc::mach_port_t,
        name: libc::mach_port_t,
    ) -> libc::kern_return_t;
    #[link_name = "host_statistics"]
    fn status_host_statistics(
        host: libc::mach_port_t,
        flavor: libc::c_int,
        output: *mut libc::integer_t,
        count: *mut libc::mach_msg_type_number_t,
    ) -> libc::c_int;
    #[link_name = "host_statistics64"]
    fn status_host_statistics64(
        host: libc::mach_port_t,
        flavor: libc::c_int,
        output: *mut libc::integer_t,
        count: *mut libc::mach_msg_type_number_t,
    ) -> libc::c_int;
}

#[cfg(test)]
mod status_metric_tests {
    use std::collections::HashMap;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

    unsafe fn record_deallocation(
        _task: libc::mach_port_t,
        _name: libc::mach_port_t,
    ) -> libc::kern_return_t {
        DEALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        0
    }

    #[test]
    fn platform_metric_sampler_returns_local_macos_snapshot() {
        // AC6: macOS collection stays in macos.rs and uses native libc/Mach APIs.
        let metrics = super::sample_status_metrics(
            &mut crate::platform::status_metrics::StatusMetricSampler::new(),
        );
        assert!(!metrics.hostname.is_empty());
        assert!(!metrics.username.is_empty());
        assert_eq!(metrics.date.len(), 10);
        assert_eq!(metrics.time.len(), 5);
    }

    #[test]
    fn mach_host_port_guard_deallocates_exactly_once() {
        DEALLOCATIONS.store(0, Ordering::SeqCst);
        {
            let _host = super::StatusMachHostPort {
                port: 42,
                deallocate: record_deallocation,
            };
        }
        assert_eq!(DEALLOCATIONS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn route_parser_extracts_only_the_interface_field() {
        let output = "   route to: default\n\
                      destination: default\n\
                      interface: en9\n\
                      flags: <UP,GATEWAY,DONE>\n";
        assert_eq!(
            super::parse_default_route_interface(output).as_deref(),
            Some("en9")
        );
        assert_eq!(super::parse_default_route_interface("gateway: en9\n"), None);
    }

    #[test]
    fn network_selection_joins_default_route_ipv4_and_link_counters_by_name() {
        let ipv4s = vec![
            super::StatusInterfaceIpv4 {
                name: "en0".into(),
                address: Ipv4Addr::new(192, 168, 1, 5),
                up: true,
            },
            super::StatusInterfaceIpv4 {
                name: "en9".into(),
                address: Ipv4Addr::new(10, 0, 0, 8),
                up: true,
            },
            super::StatusInterfaceIpv4 {
                name: "utun3".into(),
                address: Ipv4Addr::new(100, 64, 1, 2),
                up: true,
            },
        ];
        let counters = HashMap::from([
            ("en0".into(), (10, 20)),
            ("en9".into(), (30, 40)),
            ("utun3".into(), (50, 60)),
        ]);

        let selected = super::select_status_network_interfaces(Some("en9"), ipv4s, counters);

        assert_eq!(selected.primary.as_deref(), Some("en9"));
        assert_eq!(selected.local_ip.as_deref(), Some("10.0.0.8"));
        assert_eq!(selected.primary_bytes, Some((30, 40)));
        assert_eq!(selected.tailscale_ip.as_deref(), Some("100.64.1.2"));
        assert!(selected.vpn_active);
    }

    #[test]
    fn link_counter_reader_copies_darwin_if_data_fields() {
        let mut data = unsafe { std::mem::zeroed::<libc::if_data>() };
        data.ifi_ibytes = 123;
        data.ifi_obytes = 456;
        let mut interface = unsafe { std::mem::zeroed::<libc::ifaddrs>() };
        interface.ifa_data = (&raw mut data).cast();

        assert_eq!(super::status_interface_bytes(&interface), Some((123, 456)));
    }

    #[test]
    fn network_selection_falls_back_coherently_when_default_route_is_unavailable() {
        let ipv4s = vec![
            super::StatusInterfaceIpv4 {
                name: "utun3".into(),
                address: Ipv4Addr::new(100, 100, 1, 2),
                up: true,
            },
            super::StatusInterfaceIpv4 {
                name: "en7".into(),
                address: Ipv4Addr::new(172, 16, 0, 9),
                up: true,
            },
        ];
        let counters = HashMap::from([("en7".into(), (70, 80))]);

        let selected = super::select_status_network_interfaces(Some("missing0"), ipv4s, counters);

        assert_eq!(selected.primary.as_deref(), Some("en7"));
        assert_eq!(selected.local_ip.as_deref(), Some("172.16.0.9"));
        assert_eq!(selected.primary_bytes, Some((70, 80)));
        assert_eq!(selected.tailscale_ip.as_deref(), Some("100.100.1.2"));
        assert!(selected.vpn_active);
    }

    #[test]
    fn generic_vpn_requires_an_up_tunnel_with_a_usable_address() {
        let ipv4s = vec![
            super::StatusInterfaceIpv4 {
                name: "utun7".into(),
                address: Ipv4Addr::new(10, 50, 0, 1),
                up: false,
            },
            super::StatusInterfaceIpv4 {
                name: "en0".into(),
                address: Ipv4Addr::new(192, 168, 1, 5),
                up: true,
            },
        ];
        let selected = super::select_status_network_interfaces(Some("en0"), ipv4s, HashMap::new());
        assert!(!selected.vpn_active);

        let ipv4s = vec![
            super::StatusInterfaceIpv4 {
                name: "utun7".into(),
                address: Ipv4Addr::new(10, 50, 0, 1),
                up: true,
            },
            super::StatusInterfaceIpv4 {
                name: "en0".into(),
                address: Ipv4Addr::new(192, 168, 1, 5),
                up: true,
            },
        ];
        let selected = super::select_status_network_interfaces(Some("en0"), ipv4s, HashMap::new());
        assert!(selected.vpn_active);
        assert_eq!(selected.tailscale_ip, None);
    }
}

const PROC_PGRP_ONLY: u32 = 2;
const SERVER_NOFILE_LIMIT_TARGET: libc::rlim_t = 8192;

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    false
}

fn raw_command_argv(command: &str, flag: &str) -> Vec<std::ffi::OsString> {
    vec!["/bin/sh".into(), flag.into(), command.into()]
}

pub(crate) fn detached_custom_command_process_platform(command: &str) -> std::process::Command {
    let argv = raw_command_argv(command, "-lc");
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

pub(crate) fn pane_custom_command_pty_builder_platform(
    command: &str,
) -> portable_pty::CommandBuilder {
    portable_pty::CommandBuilder::from_argv(raw_command_argv(command, "-c"))
}

pub(crate) fn scrollback_editor_argv(path: &Path) -> std::io::Result<Vec<String>> {
    let quoted_path = shell_quote(&path.display().to_string());
    let command = format!(
        r#"scrollback_file={quoted_path}; eval "${{EDITOR:-vi}} \"\$scrollback_file\""; status=$?; rm -f "$scrollback_file"; exit $status"#
    );
    Ok(vec!["/bin/sh".to_string(), "-c".to_string(), command])
}

pub(crate) fn interactive_shell_command(argv: &[String], shell_name: &str) -> Option<String> {
    super::interactive_unix_shell_command(argv, shell_name, shell_quote)
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[repr(C)]
struct TisInputSource {
    _private: [u8; 0],
}

type TisInputSourceRef = *const TisInputSource;
type CfTypeRef = *const libc::c_void;
type CfStringRef = *const libc::c_void;
type OsStatus = libc::c_int;
type Boolean = libc::c_uchar;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    #[link_name = "kTISPropertyInputSourceID"]
    static TIS_PROPERTY_INPUT_SOURCE_ID: CfStringRef;

    #[link_name = "TISCopyCurrentKeyboardInputSource"]
    fn tis_copy_current_keyboard_input_source() -> TisInputSourceRef;

    #[link_name = "TISCopyCurrentASCIICapableKeyboardLayoutInputSource"]
    fn tis_copy_current_ascii_capable_keyboard_layout_input_source() -> TisInputSourceRef;

    #[link_name = "TISGetInputSourceProperty"]
    fn tis_get_input_source_property(
        input_source: TisInputSourceRef,
        property_key: CfStringRef,
    ) -> CfTypeRef;

    #[link_name = "TISSelectInputSource"]
    fn tis_select_input_source(input_source: TisInputSourceRef) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    #[link_name = "CFRelease"]
    fn cf_release(value: CfTypeRef);

    #[link_name = "CFEqual"]
    fn cf_equal(left: CfTypeRef, right: CfTypeRef) -> Boolean;

    #[link_name = "kCFRunLoopDefaultMode"]
    static CF_RUN_LOOP_DEFAULT_MODE: CfStringRef;

    #[link_name = "CFRunLoopRunInMode"]
    fn cf_run_loop_run_in_mode(
        mode: CfStringRef,
        seconds: f64,
        return_after_source_handled: Boolean,
    ) -> libc::c_int;
}

/// Pump the main thread's run loop once (non-blocking) so the process receives the
/// `kTISNotifySelectedKeyboardInputSourceChanged` notification and refreshes the per-process cache
/// that `TISCopyCurrentKeyboardInputSource` reads. That notification arrives only via the main
/// thread's run loop, so a process that never runs a CFRunLoop (the headless server) reads a stale
/// source. Must run on the main thread.
pub(crate) fn pump_input_source_runloop() {
    debug_assert!(
        // SAFETY: `pthread_main_np` is always safe to call.
        unsafe { libc::pthread_main_np() } != 0,
        "pump_input_source_runloop must run on the main thread"
    );
    // SAFETY: `CFRunLoopRunInMode` is thread-safe; a 0-second call drains the ready sources and
    // returns immediately (no blocking). `CF_RUN_LOOP_DEFAULT_MODE` is a framework-owned constant.
    unsafe {
        let _ = cf_run_loop_run_in_mode(CF_RUN_LOOP_DEFAULT_MODE, 0.0, 0);
    }
}

#[derive(Debug)]
struct RetainedInputSource(NonNull<TisInputSource>);

impl RetainedInputSource {
    /// Takes ownership of a retained reference returned by a TIS `Copy` function.
    unsafe fn from_copy(raw: TisInputSourceRef) -> Option<Self> {
        NonNull::new(raw as *mut TisInputSource).map(Self)
    }

    fn select(&self) -> OsStatus {
        // SAFETY: this wrapper keeps the retained input source alive for the call.
        unsafe { tis_select_input_source(self.0.as_ptr()) }
    }

    fn has_same_id(&self, other: &Self) -> bool {
        // SAFETY: TIS property values stay valid while their input sources are alive;
        // both wrappers outlive this comparison.
        unsafe {
            let left = tis_get_input_source_property(self.0.as_ptr(), TIS_PROPERTY_INPUT_SOURCE_ID);
            let right =
                tis_get_input_source_property(other.0.as_ptr(), TIS_PROPERTY_INPUT_SOURCE_ID);
            !left.is_null() && !right.is_null() && cf_equal(left, right) != 0
        }
    }
}

impl Drop for RetainedInputSource {
    fn drop(&mut self) {
        // SAFETY: `from_copy` gives this wrapper ownership of one retain.
        unsafe { cf_release(self.0.as_ptr().cast()) }
    }
}

#[derive(Debug)]
pub(crate) struct InputSourceRestore {
    previous: RetainedInputSource,
}

impl Drop for InputSourceRestore {
    fn drop(&mut self) {
        let status = self.previous.select();
        if status != 0 {
            tracing::debug!(
                status,
                "failed to restore host input source after prefix mode"
            );
        }
    }
}

pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    // SAFETY: both Carbon `Copy` functions transfer one retain to the caller.
    let current =
        unsafe { RetainedInputSource::from_copy(tis_copy_current_keyboard_input_source())? };
    let ascii = unsafe {
        RetainedInputSource::from_copy(
            tis_copy_current_ascii_capable_keyboard_layout_input_source(),
        )?
    };

    if current.has_same_id(&ascii) {
        return None;
    }

    let status = ascii.select();
    if status != 0 {
        tracing::debug!(status, "failed to switch host input source for prefix mode");
        return None;
    }

    Some(InputSourceRestore { previous: current })
}

pub fn raise_server_nofile_limit() {
    match raise_nofile_limit(SERVER_NOFILE_LIMIT_TARGET) {
        Ok(None) => {}
        Ok(Some((previous, target))) => {
            tracing::info!(previous, target, "raised server file descriptor soft limit")
        }
        Err(err) => tracing::warn!(err = %err, "failed to raise server file descriptor limit"),
    }
}

fn raise_nofile_limit(
    target: libc::rlim_t,
) -> std::io::Result<Option<(libc::rlim_t, libc::rlim_t)>> {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut limit = unsafe { limit.assume_init() };
    let Some(target) = target_nofile_soft_limit(limit.rlim_cur, limit.rlim_max, target) else {
        return Ok(None);
    };

    let previous = limit.rlim_cur;
    limit.rlim_cur = target;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(Some((previous, target)))
}

fn target_nofile_soft_limit(
    current: libc::rlim_t,
    hard: libc::rlim_t,
    target: libc::rlim_t,
) -> Option<libc::rlim_t> {
    let target = if hard == libc::RLIM_INFINITY {
        target
    } else {
        target.min(hard)
    };

    (current < target).then_some(target)
}

pub(crate) fn available_pane_shell(child_pid: u32) -> Option<String> {
    super::available_pane_shell_from_job(child_pid, foreground_job(child_pid)?)
}

/// Collect the foreground terminal job for a given child PID.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    if child_pid == 0 {
        return None;
    }

    let fg_pgid = foreground_process_group_id(child_pid)?;
    let mut processes = Vec::new();

    for pid in process_group_pids(fg_pgid) {
        let Some(info) = process_bsdinfo(pid) else {
            continue;
        };
        if info.pbi_pgid != fg_pgid {
            continue;
        }

        let Some(name) = comm_from_bsdinfo(&info) else {
            continue;
        };
        let argv = process_argv(pid);
        processes.push(ForegroundProcess {
            pid,
            name,
            argv0: process_argv0_name(pid),
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        });
    }

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: fg_pgid,
        processes,
    })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let info = process_bsdinfo(process_group_id)?;
    if info.pbi_pgid != process_group_id {
        return None;
    }

    let name = comm_from_bsdinfo(&info)?;
    let argv = process_argv(process_group_id);
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: process_argv0_name(process_group_id),
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

fn process_group_pids(process_group_id: u32) -> Vec<u32> {
    let mut capacity = 16usize;

    for _ in 0..8 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let buffer_bytes = pids.len() * std::mem::size_of::<libc::pid_t>();
        let returned_bytes = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                process_group_id,
                pids.as_mut_ptr() as *mut libc::c_void,
                buffer_bytes as libc::c_int,
            )
        };
        if returned_bytes <= 0 {
            return Vec::new();
        }

        let returned_bytes = returned_bytes as usize;
        let count = returned_bytes / std::mem::size_of::<libc::pid_t>();
        if returned_bytes < buffer_bytes {
            return collect_positive_pids(pids, count);
        }
        capacity = capacity.saturating_mul(2);
    }

    Vec::new()
}

/// Read `e_tpgid` (foreground process group of the controlling terminal)
/// for the given PID.
pub fn foreground_process_group_id(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret != size {
        return None;
    }

    let fg = info.e_tpgid;
    if fg > 0 {
        #[allow(clippy::unnecessary_cast)] // info.e_tpgid (pid_t) type is platform-dependent
        Some(fg as u32)
    } else {
        None
    }
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

/// Get the effective process name from `argv[0]` via `sysctl(KERN_PROCARGS2)`.
///
/// This is the macOS equivalent of reading `/proc/{pid}/cmdline` on Linux.
/// It reflects runtime title changes like Node.js `process.title = "pi"`.
fn process_argv0_name(pid: u32) -> Option<String> {
    let buf = kern_procargs2(pid)?;

    // Layout: [argc: i32] [exec_path\0] [padding\0...] [argv[0]\0] [argv[1]\0] ...
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    // Skip past exec_path and null padding to reach argv[0]
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    if pos >= rest.len() {
        return None;
    }

    // Read argv[0]
    let argv0_end = rest[pos..]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(rest.len() - pos);
    let argv0 = std::str::from_utf8(&rest[pos..pos + argv0_end]).ok()?;

    if argv0.is_empty() {
        return None;
    }

    // Return basename (argv[0] may be a full path like "/usr/bin/node")
    let basename = Path::new(argv0).file_name()?.to_str()?;

    // Strip leading dash (login shells show as "-zsh")
    let name = basename.strip_prefix('-').unwrap_or(basename);
    if name.is_empty() {
        return None;
    }

    Some(name.to_string())
}

/// Raw `sysctl(KERN_PROCARGS2)` call. Returns the full buffer.
fn kern_procargs2(pid: u32) -> Option<Vec<u8>> {
    unsafe {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];

        // First call: query required buffer size
        let mut size: libc::size_t = 0;
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 || size == 0 {
            return None;
        }

        // Second call: read data
        let mut buf = vec![0u8; size];
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return None;
        }
        buf.truncate(size);
        Some(buf)
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    run_clipboard_command(
        &ClipboardCommand {
            program: "pbcopy",
            args: &[],
        },
        bytes,
    )
}

pub fn read_clipboard_text() -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new("pbpaste")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let read = match read_limited_reader(stdout, MAX_CLIPBOARD_TEXT_BYTES) {
        Ok(LimitedRead::Oversized) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }
    match read {
        LimitedRead::Complete(bytes) => String::from_utf8(bytes).ok(),
        LimitedRead::Empty => None,
        LimitedRead::Oversized => unreachable!("oversized clipboard text is handled before wait"),
    }
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    let path = std::env::temp_dir().join(format!(
        "herdr-clipboard-image-{}-{}.png",
        std::process::id(),
        unique_timestamp_nanos()
    ));
    let script = format!(
        "set png_data to (the clipboard as «class PNGf»)\nset fp to open for access POSIX file \"{}\" with write permission\nwrite png_data to fp\nclose access fp",
        path.display()
    );

    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;

    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }

    let bytes = match std::fs::File::open(&path).ok().and_then(|file| {
        read_limited_reader(file, crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()
    }) {
        Some(LimitedRead::Complete(bytes)) => bytes,
        Some(LimitedRead::Empty | LimitedRead::Oversized) | None => {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    };
    let _ = std::fs::remove_file(&path);
    Some(ClipboardImage {
        bytes,
        extension: "png",
    })
}

fn unique_timestamp_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// Show a native macOS notification.
///
/// Prefer `terminal-notifier` when it is installed because it can activate the
/// hosting terminal on click. Fall back to built-in AppleScript notifications
/// when it is not available.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if show_terminal_notifier_notification(title, body, &mut command).unwrap_or(false) {
        return Ok(true);
    }

    show_osascript_notification(title, body, &mut command)
}

fn show_terminal_notifier_notification(
    title: &str,
    body: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let activate_bundle_id = verified_terminal_bundle_identifier(command);
    show_terminal_notifier_notification_with_options(
        title,
        body,
        activate_bundle_id.as_deref(),
        command,
    )
}

fn show_terminal_notifier_notification_with_options(
    title: &str,
    body: Option<&str>,
    activate_bundle_id: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let mut cmd = command("terminal-notifier");
    build_terminal_notifier_command(&mut cmd, title, body, activate_bundle_id);
    run_notification_command(cmd)
}

fn build_terminal_notifier_command(
    cmd: &mut Command,
    title: &str,
    body: Option<&str>,
    activate_bundle_id: Option<&str>,
) {
    cmd.arg("-title").arg(title);
    cmd.arg("-message").arg(body.unwrap_or_default());
    if let Some(bundle_id) = activate_bundle_id {
        cmd.arg("-activate").arg(bundle_id);
    }
}

fn show_osascript_notification(
    title: &str,
    body: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let mut cmd = command("/usr/bin/osascript");
    cmd.arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("display notification (item 2 of argv) with title (item 1 of argv)")
        .arg("-e")
        .arg("end run")
        .arg(title)
        .arg(body.unwrap_or_default());
    run_notification_command(cmd)
}

fn verified_terminal_bundle_identifier(
    command: &mut impl FnMut(&str) -> Command,
) -> Option<String> {
    static BUNDLE_ID: OnceLock<Option<String>> = OnceLock::new();
    BUNDLE_ID
        .get_or_init(|| {
            let bundle_id = detected_terminal_bundle_identifier()?;
            bundle_identifier_available(bundle_id, command).then(|| bundle_id.to_owned())
        })
        .clone()
}

fn bundle_identifier_available(bundle_id: &str, command: &mut impl FnMut(&str) -> Command) -> bool {
    let query = format!("kMDItemCFBundleIdentifier == '{bundle_id}'");
    let output = command("mdfind")
        .arg(query)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match output {
        Ok(output) if output.status.success() => !output.stdout.is_empty(),
        _ => false,
    }
}

fn detected_terminal_bundle_identifier() -> Option<&'static str> {
    terminal_bundle_identifier_from_env(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        std::env::var_os("ALACRITTY_WINDOW_ID").is_some(),
    )
}

fn terminal_bundle_identifier_from_env(
    term_program: Option<&str>,
    term: Option<&str>,
    has_kitty_window_id: bool,
    has_alacritty_window_id: bool,
) -> Option<&'static str> {
    match term_program {
        Some("ghostty") => return Some("com.mitchellh.ghostty"),
        Some("iTerm.app") => return Some("com.googlecode.iterm2"),
        Some("WezTerm") => return Some("com.github.wez.wezterm"),
        Some("Apple_Terminal") => return Some("com.apple.Terminal"),
        _ => {}
    }

    if has_kitty_window_id || term == Some("xterm-kitty") {
        return Some("net.kovidgoyal.kitty");
    }
    if has_alacritty_window_id {
        return Some("org.alacritty");
    }

    None
}

fn run_notification_command(mut command: Command) -> std::io::Result<bool> {
    let status = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    Ok(status.success())
}

fn run_clipboard_command(command: &ClipboardCommand, bytes: &[u8]) -> bool {
    let mut child = match Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    if stdin.write_all(bytes).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    drop(stdin);

    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn process_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    (ret == size).then_some(info)
}

fn comm_from_bsdinfo(info: &libc::proc_bsdinfo) -> Option<String> {
    let end = info
        .pbi_comm
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(info.pbi_comm.len());
    if end == 0 {
        return None;
    }

    let bytes: Vec<u8> = info.pbi_comm[..end].iter().map(|&b| b as u8).collect();
    String::from_utf8(bytes).ok()
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let buf = kern_procargs2(pid)?;
    procargs2_argv(&buf)
}

/// Read a Herdr agent identity hint from a process environment.
pub fn process_agent_hint(pid: u32) -> Option<crate::detect::Agent> {
    if pid == 0 {
        return None;
    }
    let buf = kern_procargs2(pid)?;
    super::parse_agent_env_hint(procargs2_env(&buf)?)
}

fn procargs2_argv_start(rest: &[u8]) -> Option<usize> {
    let exec_end = rest.iter().position(|&byte| byte == 0)?;
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    (pos < rest.len()).then_some(pos)
}

fn skip_nul_strings(bytes: &[u8], start: usize, count: usize) -> Option<usize> {
    let mut current = start;
    for _ in 0..count {
        let end = bytes.get(current..)?.iter().position(|&byte| byte == 0)?;
        current = current.checked_add(end)?.checked_add(1)?;
    }
    Some(current)
}

fn procargs2_argv(buf: &[u8]) -> Option<Vec<String>> {
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    // Layout: [argc: i32] [exec_path\0] [padding\0...] [argv[0]\0] ... [env\0] ...
    let rest = &buf[4..];
    let mut current = procargs2_argv_start(rest)?;
    let mut argv = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        if current >= rest.len() {
            return None;
        }
        let end = rest[current..]
            .iter()
            .position(|&b| b == 0)
            .map(|offset| current + offset)
            .unwrap_or(rest.len());
        if end == current {
            return None;
        }
        argv.push(String::from_utf8_lossy(&rest[current..end]).into_owned());
        current = end + 1;
    }

    Some(argv)
}

fn procargs2_env(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    let rest = &buf[4..];
    let argv_start = procargs2_argv_start(rest)?;
    let env_start = skip_nul_strings(rest, argv_start, argc as usize)?;
    rest.get(env_start..)
}

/// Get the current working directory of a process.
///
/// Uses `proc_pidinfo(PROC_PIDVNODEPATHINFO)` to read `pvi_cdir.vip_path`.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }

    let mut pathinfo: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut pathinfo as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret != size {
        return None;
    }

    // vip_path is [[c_char; 32]; 32] in libc (workaround for old Rust const generics).
    // Reinterpret as flat bytes (total MAXPATHLEN = 1024).
    let vip_path = unsafe {
        std::slice::from_raw_parts(
            pathinfo.pvi_cdir.vip_path.as_ptr() as *const u8,
            libc::MAXPATHLEN as usize,
        )
    };

    let nul = vip_path.iter().position(|&b| b == 0)?;
    if nul == 0 {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&vip_path[..nul])))
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    if child_pid == 0 {
        return Vec::new();
    }

    let target_session = unsafe { libc::getsid(child_pid as libc::c_int) };
    if target_session <= 0 {
        return Vec::new();
    }

    all_pids()
        .into_iter()
        .filter(|pid| unsafe { libc::getsid(*pid as libc::pid_t) } == target_session)
        .collect()
}

fn all_pids() -> Vec<u32> {
    let initial_count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut capacity = if initial_count > 0 {
        initial_count as usize + 128
    } else {
        4096
    };

    for _ in 0..8 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr() as *mut libc::c_void,
                (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if count <= 0 {
            return Vec::new();
        }

        let count = count as usize;
        if count < capacity {
            return collect_positive_pids(pids, count);
        }
        capacity = capacity.saturating_mul(2);
    }

    Vec::new()
}

fn collect_positive_pids(pids: Vec<libc::pid_t>, count: usize) -> Vec<u32> {
    pids.into_iter()
        .take(count)
        .filter(|pid| *pid > 0)
        .map(|pid| pid as u32)
        .collect()
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    let sig = match signal {
        Signal::Hangup => libc::SIGHUP,
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };

    for &pid in pids {
        if pid == 0 {
            continue;
        }
        unsafe {
            libc::kill(pid as libc::c_int, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::c_int, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TryWaitErrorChild {
        kill_calls: usize,
        wait_calls: usize,
    }

    impl BoundedChild for TryWaitErrorChild {
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            Err(std::io::Error::other("injected try_wait failure"))
        }

        fn kill(&mut self) -> std::io::Result<()> {
            self.kill_calls += 1;
            Err(std::io::Error::other("injected kill failure"))
        }

        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            self.wait_calls += 1;
            Err(std::io::Error::other("injected wait failure"))
        }
    }

    #[test]
    fn pmset_battery_wait_cleans_up_after_try_wait_error() {
        let mut child = TryWaitErrorChild::default();

        assert!(!wait_for_child_until(&mut child, Duration::from_secs(1)));
        assert_eq!(child.kill_calls, 1);
        assert_eq!(child.wait_calls, 1);
    }

    #[test]
    fn pmset_battery_parser_handles_present_discharging_battery() {
        let output =
            "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t73%; discharging; 4:12 remaining present: true\n";
        assert_eq!(parse_pmset_battery(output), (Some(73), Some(false)));
    }

    #[test]
    fn pmset_battery_parser_handles_charging_battery() {
        let output =
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t42%; charging; 1:11 remaining present: true\n";
        assert_eq!(parse_pmset_battery(output), (Some(42), Some(true)));
    }

    #[test]
    fn pmset_battery_parser_handles_unavailable_battery() {
        assert_eq!(
            parse_pmset_battery("Now drawing from 'AC Power'\n"),
            (None, None)
        );
    }

    #[test]
    fn pmset_battery_parser_rejects_malformed_values() {
        assert_eq!(
            parse_pmset_battery("-InternalBattery-0\tunknown%; charging;\n"),
            (None, None)
        );
        assert_eq!(
            parse_pmset_battery("-InternalBattery-0\t101%; charging;\n"),
            (None, None)
        );
    }

    #[test]
    fn pmset_battery_wait_kills_a_child_blocked_on_full_stdout_pipe() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "while :; do printf 'battery-output'; done"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn blocking fixture");
        let started = Instant::now();

        assert!(!wait_for_child_until(&mut child, Duration::from_millis(50)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(child.try_wait().expect("query child").is_some());
    }

    #[test]
    fn nofile_target_raises_low_soft_limit_to_cap_when_hard_is_unlimited() {
        assert_eq!(
            target_nofile_soft_limit(256, libc::RLIM_INFINITY, 8192),
            Some(8192)
        );
    }

    #[test]
    fn nofile_target_respects_finite_hard_limit() {
        assert_eq!(target_nofile_soft_limit(256, 4096, 8192), Some(4096));
    }

    #[test]
    fn nofile_target_does_not_lower_existing_soft_limit() {
        assert_eq!(
            target_nofile_soft_limit(16_384, libc::RLIM_INFINITY, 8192),
            None
        );
    }

    fn build_procargs2(exec_path: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(argv.len() as i32).to_ne_bytes());
        buf.extend_from_slice(exec_path.as_bytes());
        buf.push(0);
        buf.push(0);
        for arg in argv {
            buf.extend_from_slice(arg.as_bytes());
            buf.push(0);
        }
        for entry in env {
            buf.extend_from_slice(entry.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn procargs2_argv_excludes_environment_entries() {
        let buf = build_procargs2(
            "/usr/bin/node",
            &["node", "/Users/can/.local/bin/pi"],
            &[
                "PATH=/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin",
                "TERM=tmux-256color",
            ],
        );

        let argv = procargs2_argv(&buf).expect("expected argv");
        assert_eq!(argv, vec!["node", "/Users/can/.local/bin/pi"]);
        assert_eq!(argv.join(" "), "node /Users/can/.local/bin/pi");
        assert!(!argv.join(" ").contains("codex.system"));
    }

    #[test]
    fn procargs2_env_reads_agent_hint_after_argv() {
        let buf = build_procargs2(
            "/opt/homebrew/bin/nono",
            &["nono", "run", "HERDR_AGENT=codex", "--", "claude"],
            &["PATH=/usr/bin", "HERDR_AGENT=claude", "TERM=xterm-256color"],
        );

        let env = procargs2_env(&buf).expect("expected env block");
        assert_eq!(
            crate::platform::parse_agent_env_hint(env),
            Some(crate::detect::Agent::Claude)
        );
    }

    #[test]
    fn procargs2_env_does_not_treat_argv_as_environment() {
        let buf = build_procargs2(
            "/opt/homebrew/bin/nono",
            &["nono", "run", "HERDR_AGENT=claude"],
            &["PATH=/usr/bin"],
        );

        let env = procargs2_env(&buf).expect("expected env block");
        assert_eq!(crate::platform::parse_agent_env_hint(env), None);
    }

    #[test]
    fn terminal_bundle_identifier_maps_known_terminal_env() {
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("ghostty"), None, false, false),
            Some("com.mitchellh.ghostty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("iTerm.app"), None, false, false),
            Some("com.googlecode.iterm2")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("WezTerm"), None, false, false),
            Some("com.github.wez.wezterm")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("Apple_Terminal"), None, false, false),
            Some("com.apple.Terminal")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, Some("xterm-kitty"), false, false),
            Some("net.kovidgoyal.kitty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, true, false),
            Some("net.kovidgoyal.kitty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, false, true),
            Some("org.alacritty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, false, false),
            None
        );
    }

    #[test]
    fn terminal_notifier_command_includes_icon_and_activation() {
        let mut cmd = Command::new("terminal-notifier");
        build_terminal_notifier_command(
            &mut cmd,
            "pi finished",
            Some("workspace 1"),
            Some("com.mitchellh.ghostty"),
        );
        let args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-title",
                "pi finished",
                "-message",
                "workspace 1",
                "-activate",
                "com.mitchellh.ghostty"
            ]
        );
    }

    #[test]
    fn terminal_notifier_success_skips_osascript() {
        let path = std::env::temp_dir().join(format!(
            "herdr-terminal-notifier-args-{}",
            std::process::id()
        ));
        let script = "printf '%s:%s\\n' \"$0\" \"$*\" >> \"$HERDR_NOTIFY_ARGS\"";
        let mut command = |program: &str| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg(program)
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        };

        let shown = show_terminal_notifier_notification_with_options(
            "title",
            Some("body"),
            Some("com.mitchellh.ghostty"),
            &mut command,
        )
        .expect("terminal-notifier command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert!(args.starts_with("terminal-notifier:"), "{args}");
        assert!(args.contains("-activate com.mitchellh.ghostty"), "{args}");
        assert!(!args.contains("osascript"), "{args}");
    }

    #[test]
    fn desktop_notification_falls_back_to_osascript_when_terminal_notifier_fails() {
        let path =
            std::env::temp_dir().join(format!("herdr-osascript-args-{}", std::process::id()));
        let script = r#"
if [ "$0" = "terminal-notifier" ]; then
  exit 1
fi
printf '%s\n' "$@" > "$HERDR_NOTIFY_ARGS"
"#;
        let mut command = |program: &str| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg(program)
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        };
        let shown = show_desktop_notification_with_command("title", Some("body"), &mut command)
            .expect("osascript fallback should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            args,
            "-e\non run argv\n-e\ndisplay notification (item 2 of argv) with title (item 1 of argv)\n-e\nend run\ntitle\nbody\n"
        );
    }

    #[test]
    fn scrollback_editor_argv_preserves_unix_editor_shell_semantics() {
        let path = std::path::Path::new("/tmp/herdr scrollback.txt");
        let argv = scrollback_editor_argv(path).unwrap();

        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].contains("EDITOR:-vi"));
        assert!(argv[2].contains("/tmp/herdr scrollback.txt"));
    }
}
