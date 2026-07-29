use std::{
    collections::{HashSet, VecDeque},
    io::Write,
    os::fd::RawFd,
    path::PathBuf,
    process::{Command, Stdio},
};

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

const WSL_MARKER_ENV_VARS: &[&str] = &["WSL_DISTRO_NAME", "WSL_INTEROP"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcGroupMember {
    pid: u32,
    comm: String,
}

pub(crate) fn sample_status_metrics(
    sampler: &mut super::status_metrics::StatusMetricSampler,
) -> super::status_metrics::StatusMetrics {
    let (hostname, username) = status_identity();
    let (date, time) = status_local_date_time();
    let (mem_used_gib, mem_total_gib) = status_memory().unwrap_or((0.0, 0.0));
    let cpu_percent = status_cpu_ticks().and_then(|(idle, total)| sampler.cpu_percent(idle, total));
    let (battery_percent, battery_charging) = status_battery();
    let interface = status_default_interface();
    let interfaces = status_interface_ipv4s(interface.as_deref());
    let (net_down_kib, net_up_kib) = interface
        .as_deref()
        .and_then(status_interface_bytes)
        .and_then(|(rx, tx)| {
            sampler.bandwidth_kib(interface.as_deref()?, rx, tx, std::time::Instant::now())
        })
        .map(|(down, up)| (Some(down), Some(up)))
        .unwrap_or((None, None));

    super::status_metrics::StatusMetrics {
        cpu_percent,
        mem_used_gib: (mem_total_gib > 0.0).then_some(mem_used_gib),
        mem_total_gib: (mem_total_gib > 0.0).then_some(mem_total_gib),
        battery_percent,
        battery_charging,
        local_ip: interfaces.local_ip,
        tailscale_ip: interfaces.tailscale_ip.clone(),
        public_ip: super::status_metrics::compatible_public_ip(),
        net_down_kib,
        net_up_kib,
        net_kind: status_net_kind(interface.as_deref()),
        vpn_active: interfaces.vpn_active,
        remote_session: super::status_metrics::remote_session_from_env(),
        hostname,
        username,
        date,
        time,
    }
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

fn status_memory() -> Option<(f32, f32)> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_status_memory(&contents)
}

fn parse_status_memory(contents: &str) -> Option<(f32, f32)> {
    let mut total_kib = None;
    let mut available_kib = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total_kib = value.split_whitespace().next()?.parse::<u64>().ok();
        } else if let Some(value) = line.strip_prefix("MemAvailable:") {
            available_kib = value.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    let total = total_kib?;
    let available = available_kib?;
    let used = total.saturating_sub(available);
    Some((used as f32 / 1_048_576.0, total as f32 / 1_048_576.0))
}

fn status_cpu_ticks() -> Option<(u64, u64)> {
    let contents = std::fs::read_to_string("/proc/stat").ok()?;
    let mut fields = contents.lines().next()?.split_whitespace();
    (fields.next()? == "cpu").then_some(())?;
    let values = fields
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (values.len() == 8).then_some(())?;
    let idle = values[3].saturating_add(values[4]);
    Some((idle, values.iter().sum()))
}

fn status_battery() -> (Option<u8>, Option<bool>) {
    let battery = std::fs::read_dir("/sys/class/power_supply")
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("BAT"))
                })
        });
    let Some(battery) = battery else {
        return (None, None);
    };
    let percent = std::fs::read_to_string(battery.join("capacity"))
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok());
    let charging = std::fs::read_to_string(battery.join("status"))
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "charging" | "full"
            )
        });
    (percent, charging)
}

fn status_default_interface() -> Option<String> {
    let contents = std::fs::read_to_string("/proc/net/route").ok()?;
    parse_default_interface(&contents)
}

fn parse_default_interface(contents: &str) -> Option<String> {
    contents
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let interface = *fields.first()?;
            let destination = *fields.get(1)?;
            let flags = u32::from_str_radix(fields.get(3)?, 16).ok()?;
            let metric = fields.get(6)?.parse::<u64>().ok()?;
            let mask = *fields.get(7)?;
            (destination == "00000000" && mask == "00000000" && flags & 0x1 != 0)
                .then_some((interface, metric))
        })
        .min_by_key(|(_, metric)| *metric)
        .map(|(interface, _)| interface.to_owned())
}

fn status_interface_bytes(interface: &str) -> Option<(u64, u64)> {
    let root = std::path::Path::new("/sys/class/net")
        .join(interface)
        .join("statistics");
    let rx = std::fs::read_to_string(root.join("rx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let tx = std::fs::read_to_string(root.join("tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((rx, tx))
}

fn status_net_kind(interface: Option<&str>) -> super::status_metrics::NetKind {
    let Some(interface) = interface else {
        return super::status_metrics::NetKind::Unknown;
    };
    if std::path::Path::new("/sys/class/net")
        .join(interface)
        .join("wireless")
        .exists()
        || interface.starts_with("wl")
    {
        super::status_metrics::NetKind::Wifi
    } else {
        super::status_metrics::NetKind::Ethernet
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusInterfaceIpv4 {
    name: String,
    address: std::net::Ipv4Addr,
    up: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StatusInterfaceSelection {
    local_ip: Option<String>,
    tailscale_ip: Option<String>,
    vpn_active: bool,
}

fn status_tunnel_interface(name: &str) -> bool {
    ["tailscale", "tun", "wg", "ppp", "ipsec"]
        .iter()
        .any(|needle| name.contains(needle))
}

fn status_interface_ipv4s(default_interface: Option<&str>) -> StatusInterfaceSelection {
    let mut ipv4s = Vec::new();
    let mut interfaces: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: libc owns the linked list until the paired `freeifaddrs`.
    if unsafe { libc::getifaddrs(&mut interfaces) } != 0 || interfaces.is_null() {
        return StatusInterfaceSelection::default();
    }
    let mut current = interfaces;
    while !current.is_null() {
        // SAFETY: current belongs to the valid getifaddrs list.
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null()
            && unsafe { (*interface.ifa_addr).sa_family as i32 } == libc::AF_INET
        {
            let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in) };
            let ip = std::net::Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr));
            let name = if interface.ifa_name.is_null() {
                String::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(interface.ifa_name) }
                    .to_string_lossy()
                    .into_owned()
            };
            ipv4s.push(StatusInterfaceIpv4 {
                name,
                address: ip,
                up: interface.ifa_flags & (libc::IFF_UP as u32) != 0,
            });
        }
        current = interface.ifa_next;
    }
    unsafe { libc::freeifaddrs(interfaces) };
    select_status_interface_ipv4s(default_interface, ipv4s)
}

fn select_status_interface_ipv4s(
    default_interface: Option<&str>,
    ipv4s: Vec<StatusInterfaceIpv4>,
) -> StatusInterfaceSelection {
    let mut tailscale_ip = None;
    let mut vpn_active = false;
    let mut local_candidates = Vec::new();
    for interface in ipv4s {
        if !interface.up || interface.address.is_loopback() || interface.address.is_unspecified() {
            continue;
        }
        if status_tunnel_interface(&interface.name) {
            vpn_active = true;
            let octets = interface.address.octets();
            if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                tailscale_ip.get_or_insert_with(|| interface.address.to_string());
            }
        } else {
            local_candidates.push(interface);
        }
    }
    let selected = match default_interface {
        Some(name) => local_candidates
            .iter()
            .find(|interface| interface.name == name),
        None => local_candidates.first(),
    };
    let local_ip = selected.map(|interface| interface.address.to_string());
    StatusInterfaceSelection {
        local_ip,
        tailscale_ip,
        vpn_active,
    }
}

#[cfg(test)]
mod status_metric_tests {
    use std::net::Ipv4Addr;

    use super::StatusInterfaceIpv4;

    #[test]
    fn platform_metric_sampler_returns_local_linux_snapshot() {
        // AC6: Linux collection stays in linux.rs and uses /proc, /sys, libc.
        let metrics = super::sample_status_metrics(
            &mut crate::platform::status_metrics::StatusMetricSampler::new(),
        );
        assert!(!metrics.hostname.is_empty());
        assert!(!metrics.username.is_empty());
        assert_eq!(metrics.date.len(), 10);
        assert_eq!(metrics.time.len(), 5);
    }

    #[test]
    fn default_route_prefers_lowest_metric_up_route() {
        let routes = "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
                      eth0 00000000 0100000A 0003 0 0 200 00000000 0 0 0\n\
                      wlan0 00000000 0100000A 0003 0 0 50 00000000 0 0 0\n\
                      down0 00000000 0100000A 0002 0 0 1 00000000 0 0 0\n";
        assert_eq!(
            super::parse_default_interface(routes).as_deref(),
            Some("wlan0")
        );
    }

    #[test]
    fn memory_requires_valid_total_and_available_values() {
        assert_eq!(
            super::parse_status_memory(
                "MemTotal:       16777216 kB\nMemAvailable:    8388608 kB\n"
            ),
            Some((8.0, 16.0))
        );
        assert_eq!(
            super::parse_status_memory("MemTotal:       16777216 kB\n"),
            None
        );
        assert_eq!(
            super::parse_status_memory(
                "MemTotal:       16777216 kB\nMemAvailable:    unavailable kB\n"
            ),
            None
        );
    }

    #[test]
    fn network_selection_keeps_local_ip_on_default_route_interface() {
        let interfaces = vec![
            StatusInterfaceIpv4 {
                name: "eth0".into(),
                address: Ipv4Addr::new(192, 168, 1, 20),
                up: true,
            },
            StatusInterfaceIpv4 {
                name: "wlan0".into(),
                address: Ipv4Addr::new(10, 0, 0, 9),
                up: true,
            },
            StatusInterfaceIpv4 {
                name: "tailscale0".into(),
                address: Ipv4Addr::new(100, 100, 20, 30),
                up: true,
            },
        ];

        let selected = super::select_status_interface_ipv4s(Some("wlan0"), interfaces);
        assert_eq!(selected.local_ip.as_deref(), Some("10.0.0.9"));
        assert_eq!(selected.tailscale_ip.as_deref(), Some("100.100.20.30"));
        assert!(selected.vpn_active);
    }

    #[test]
    fn network_selection_does_not_mix_missing_default_route_with_another_ip() {
        let interfaces = vec![StatusInterfaceIpv4 {
            name: "eth0".into(),
            address: Ipv4Addr::new(192, 168, 1, 20),
            up: true,
        }];

        let selected = super::select_status_interface_ipv4s(Some("wlan0"), interfaces);
        assert_eq!(selected.local_ip, None);
    }

    #[test]
    fn vpn_selection_requires_an_up_tunnel_with_a_usable_address() {
        let interfaces = vec![
            StatusInterfaceIpv4 {
                name: "wg0".into(),
                address: Ipv4Addr::new(10, 10, 0, 1),
                up: false,
            },
            StatusInterfaceIpv4 {
                name: "eth0".into(),
                address: Ipv4Addr::new(192, 168, 1, 20),
                up: true,
            },
        ];
        let selected = super::select_status_interface_ipv4s(Some("eth0"), interfaces);
        assert!(!selected.vpn_active);

        let interfaces = vec![
            StatusInterfaceIpv4 {
                name: "tun0".into(),
                address: Ipv4Addr::new(10, 20, 0, 1),
                up: true,
            },
            StatusInterfaceIpv4 {
                name: "eth0".into(),
                address: Ipv4Addr::new(192, 168, 1, 20),
                up: true,
            },
        ];
        let selected = super::select_status_interface_ipv4s(Some("eth0"), interfaces);
        assert!(selected.vpn_active);
    }
}

pub fn raise_server_nofile_limit() {}

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    running_inside_wsl()
}

fn running_inside_wsl() -> bool {
    proc_file_indicates_wsl("/proc/sys/kernel/osrelease")
        || proc_file_indicates_wsl("/proc/version")
        || WSL_MARKER_ENV_VARS
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        || std::path::Path::new("/run/WSL").exists()
}

fn proc_file_indicates_wsl(path: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text_indicates_wsl(&text))
        .unwrap_or(false)
}

fn text_indicates_wsl(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("microsoft") || text.contains("wsl")
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

pub(crate) fn scrollback_editor_argv(path: &std::path::Path) -> std::io::Result<Vec<String>> {
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

/// Collect the foreground terminal job for a given child PID.
pub(crate) fn available_pane_shell(child_pid: u32) -> Option<String> {
    super::available_pane_shell_from_job(child_pid, foreground_job(child_pid)?)
}

pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    let tpgid = foreground_process_group_id(child_pid)?;
    let members = foreground_process_group_members(child_pid, tpgid)?;
    let processes = members
        .into_iter()
        .map(|member| {
            let argv = process_argv(member.pid);
            ForegroundProcess {
                pid: member.pid,
                name: member.comm,
                argv0: None,
                cmdline: argv.as_ref().map(|parts| parts.join(" ")),
                argv,
            }
        })
        .collect::<Vec<_>>();

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: tpgid,
        processes,
    })
}

fn foreground_process_group_members(
    child_pid: u32,
    process_group_id: u32,
) -> Option<Vec<ProcGroupMember>> {
    foreground_process_group_members_with(
        child_pid,
        process_group_id,
        process_task_ids,
        process_task_children,
        live_process_group_member,
    )
}

fn foreground_process_group_members_with(
    child_pid: u32,
    process_group_id: u32,
    task_ids: impl FnMut(u32) -> Vec<u32>,
    task_children: impl FnMut(u32, u32) -> Vec<u32>,
    mut live_member: impl FnMut(u32, u32) -> Option<ProcGroupMember>,
) -> Option<Vec<ProcGroupMember>> {
    let mut members = process_tree_pids([child_pid, process_group_id], task_ids, task_children)
        .into_iter()
        .filter_map(|pid| live_member(process_group_id, pid))
        .collect::<Vec<_>>();
    members.sort_unstable_by_key(|member| member.pid);
    (!members.is_empty()).then_some(members)
}

fn process_tree_pids(
    roots: impl IntoIterator<Item = u32>,
    mut task_ids: impl FnMut(u32) -> Vec<u32>,
    mut task_children: impl FnMut(u32, u32) -> Vec<u32>,
) -> Vec<u32> {
    let mut pending = VecDeque::new();
    let mut visited = HashSet::new();
    for pid in roots {
        if pid > 0 && visited.insert(pid) {
            pending.push_back(pid);
        }
    }

    let mut pids = Vec::new();
    while let Some(pid) = pending.pop_front() {
        pids.push(pid);
        for tid in task_ids(pid) {
            for child_pid in task_children(pid, tid) {
                if child_pid > 0 && visited.insert(child_pid) {
                    pending.push_back(child_pid);
                }
            }
        }
    }
    pids
}

fn process_task_ids(pid: u32) -> Vec<u32> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| numeric_file_name(&entry))
        .collect()
}

fn process_task_children(pid: u32, tid: u32) -> Vec<u32> {
    let Some(children) = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/children")).ok()
    else {
        return Vec::new();
    };
    children
        .split_whitespace()
        .filter_map(|child| child.parse::<u32>().ok())
        .collect()
}

fn numeric_file_name(entry: &std::fs::DirEntry) -> Option<u32> {
    let file_name = entry.file_name();
    let value = file_name.to_str()?;
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn live_process_group_member(process_group_id: u32, pid: u32) -> Option<ProcGroupMember> {
    let (pgrp, comm) = process_pgrp_and_comm(pid)?;
    (pgrp > 0 && pgrp as u32 == process_group_id).then_some(ProcGroupMember { pid, comm })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let (pgrp, name) = process_pgrp_and_comm(process_group_id)?;
    if pgrp as u32 != process_group_id {
        return None;
    }

    let argv = process_argv(process_group_id);
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: None,
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    // /proc/<pid>/stat format: "pid (comm) state ppid pgrp session tty_nr tpgid ..."
    // The (comm) field can contain spaces and parens, so we find the last ')' first.
    let stat = std::fs::read_to_string(format!("/proc/{child_pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After (comm): state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    let tpgid: i32 = fields.get(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid as u32)
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

fn process_pgrp_and_comm(pid: u32) -> Option<(i32, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    process_pgrp_and_comm_from_stat(&stat)
}

fn process_pgrp_and_comm_from_stat(stat: &str) -> Option<(i32, String)> {
    let close = stat.rfind(')')?;
    let comm = stat.get(1 + stat.find('(')?..close)?.to_string();
    let rest = stat.get(close + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let pgrp: i32 = fields.get(2)?.parse().ok()?;
    Some((pgrp, comm))
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let parts: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// Get the current working directory of a process.
/// Uses /proc/<pid>/cwd symlink.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Read a Herdr agent identity hint from a process environment.
pub fn process_agent_hint(pid: u32) -> Option<crate::detect::Agent> {
    if pid == 0 {
        return None;
    }
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    super::parse_agent_env_hint(&environ)
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    let Some(session_id) = process_session_id(child_pid) else {
        return Vec::new();
    };

    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if process_session_id(pid) == Some(session_id) {
            pids.push(pid);
        }
    }
    pids
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
            libc::kill(pid as i32, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    for command in clipboard_commands() {
        if run_clipboard_command(&command, bytes) {
            return true;
        }
    }
    false
}

pub fn read_clipboard_text() -> Option<String> {
    for command in read_clipboard_text_commands() {
        if let Some(text) = read_clipboard_text_with_command(&command) {
            return Some(text);
        }
    }
    None
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    for (mime, extension) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/jpg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/bmp", "bmp"),
    ] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Some(image) =
                read_validated_clipboard_image("wl-paste", &["--type", mime], extension)
            {
                return Some(image);
            }
        }

        if std::env::var_os("DISPLAY").is_some() {
            if let Some(image) = read_validated_clipboard_image(
                "xclip",
                &["-selection", "clipboard", "-t", mime, "-o"],
                extension,
            ) {
                return Some(image);
            }
        }
    }

    None
}

fn read_validated_clipboard_image(
    program: &str,
    args: &[&str],
    extension: &'static str,
) -> Option<ClipboardImage> {
    let bytes = read_clipboard_image_with_command(program, args)?;
    if !bytes_match_image_signature(extension, &bytes) {
        return None;
    }
    Some(ClipboardImage { bytes, extension })
}

fn bytes_match_image_signature(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP",
        "bmp" => {
            if bytes.len() < 26 || !bytes.starts_with(b"BM") {
                return false;
            }
            let offset = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
            (26..=bytes.len()).contains(&offset)
        }
        _ => false,
    }
}

/// Show a native desktop notification through libnotify's command-line helper.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok(false);
    }

    let mut cmd = command("notify-send");
    cmd.arg("--").arg(title);
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        cmd.arg(body);
    }
    run_notification_command(cmd)
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

fn read_clipboard_image_with_command(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command.args(args);
    read_clipboard_image_with_spawned_command(command)
}

fn read_clipboard_image_with_spawned_command(command: Command) -> Option<Vec<u8>> {
    read_clipboard_image_with_spawned_command_max(
        command,
        crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD,
    )
}

fn read_clipboard_image_with_spawned_command_max(
    mut command: Command,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;

    let read = match read_limited_reader(stdout, max_bytes) {
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    if read == LimitedRead::Oversized {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }

    match read {
        LimitedRead::Complete(bytes) => Some(bytes),
        LimitedRead::Empty | LimitedRead::Oversized => None,
    }
}

fn clipboard_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-copy",
            args: &["--type", "text/plain;charset=utf-8"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-in"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--input"],
        });
    }

    commands
}

fn read_clipboard_text_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain;charset=utf-8"],
        });
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-out"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--output"],
        });
    }

    commands
}

fn read_clipboard_text_with_command(command: &ClipboardCommand) -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new(command.program)
        .args(command.args)
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

fn process_session_id(pid: u32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::{cell::RefCell, collections::HashMap};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn wsl_marker_detection_matches_kernel_release_text() {
        assert!(text_indicates_wsl("5.15.167.4-microsoft-standard-WSL2"));
        assert!(text_indicates_wsl("4.4.0-19041-Microsoft"));
        assert!(!text_indicates_wsl("6.8.0-64-generic"));
        assert!(!text_indicates_wsl(""));
    }

    #[test]
    fn foreground_members_follow_the_pane_tree_and_filter_by_process_group() {
        let tasks = HashMap::from([
            (100, vec![100, 101]),
            (200, vec![200]),
            (201, vec![201]),
            (210, vec![210]),
            (220, vec![220]),
            (221, vec![221]),
            (300, vec![300]),
        ]);
        let children = HashMap::from([
            ((100, 100), vec![200, 201, 300]),
            ((100, 101), vec![210]),
            ((200, 200), vec![220]),
            ((220, 220), vec![221]),
        ]);
        let processes = HashMap::from([
            (100, (100, "shell")),
            (200, (200, "leader")),
            (201, (200, "pipeline")),
            (210, (200, "thread-child")),
            (220, (220, "intermediate")),
            (221, (200, "nested-agent")),
            (300, (300, "background")),
            (9999, (200, "unrelated-host-process")),
        ]);
        let task_reads = RefCell::new(Vec::new());
        let child_reads = RefCell::new(Vec::new());
        let member_reads = RefCell::new(Vec::new());

        let members = foreground_process_group_members_with(
            100,
            200,
            |pid| {
                task_reads.borrow_mut().push(pid);
                tasks.get(&pid).cloned().unwrap_or_default()
            },
            |pid, tid| {
                child_reads.borrow_mut().push((pid, tid));
                children.get(&(pid, tid)).cloned().unwrap_or_default()
            },
            |process_group_id, pid| {
                member_reads.borrow_mut().push(pid);
                let (pgrp, comm) = processes.get(&pid)?;
                (*pgrp == process_group_id).then(|| ProcGroupMember {
                    pid,
                    comm: (*comm).to_string(),
                })
            },
        )
        .unwrap();

        assert_eq!(
            members
                .into_iter()
                .map(|member| (member.pid, member.comm))
                .collect::<Vec<_>>(),
            vec![
                (200, "leader".to_string()),
                (201, "pipeline".to_string()),
                (210, "thread-child".to_string()),
                (221, "nested-agent".to_string()),
            ]
        );
        assert!(child_reads.borrow().contains(&(100, 101)));
        assert!(task_reads.borrow().contains(&220));
        assert!(!task_reads.borrow().contains(&9999));
        assert!(!member_reads.borrow().contains(&9999));
    }

    #[test]
    fn foreground_members_degrade_to_the_direct_group_leader() {
        let members = foreground_process_group_members_with(
            100,
            200,
            |_| Vec::new(),
            |_, _| Vec::new(),
            |process_group_id, pid| {
                (pid == process_group_id).then(|| ProcGroupMember {
                    pid,
                    comm: "leader".to_string(),
                })
            },
        )
        .unwrap();

        assert_eq!(
            members,
            vec![ProcGroupMember {
                pid: 200,
                comm: "leader".to_string()
            }]
        );
    }

    #[test]
    fn foreground_members_observe_new_children_without_a_snapshot_cache() {
        let children = RefCell::new(HashMap::from([((100, 100), vec![200])]));
        let discover = || {
            foreground_process_group_members_with(
                100,
                200,
                |pid| vec![pid],
                |pid, tid| {
                    children
                        .borrow()
                        .get(&(pid, tid))
                        .cloned()
                        .unwrap_or_default()
                },
                |process_group_id, pid| {
                    [200, 201]
                        .contains(&pid)
                        .then(|| ProcGroupMember {
                            pid,
                            comm: format!("member-{pid}"),
                        })
                        .filter(|_| process_group_id == 200)
                },
            )
            .unwrap()
            .into_iter()
            .map(|member| member.pid)
            .collect::<Vec<_>>()
        };

        assert_eq!(discover(), vec![200]);
        children.borrow_mut().insert((100, 100), vec![200, 201]);
        assert_eq!(discover(), vec![200, 201]);
    }

    #[test]
    fn proc_stat_parsing_keeps_group_leader_inputs_live() {
        assert_eq!(
            process_pgrp_and_comm_from_stat("123 (name with ) paren) S 1 456 789 0 456"),
            Some((456, "name with ) paren".to_string()))
        );
    }

    #[test]
    fn clipboard_commands_prefer_wayland_when_available() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::remove_var("DISPLAY");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "wl-copy");
    }

    #[test]
    fn clipboard_commands_include_x11_fallbacks() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program, "xclip");
        assert_eq!(commands[1].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_commands_include_session_backends() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
        }

        let commands = read_clipboard_text_commands();
        assert_eq!(commands[0].program, "wl-paste");
        assert_eq!(commands[1].program, "wl-paste");
        assert_eq!(commands[2].program, "xclip");
        assert_eq!(commands[3].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_with_command_reads_utf8() {
        let command = ClipboardCommand {
            program: "printf",
            args: &["feature/linear-302"],
        };

        assert_eq!(
            read_clipboard_text_with_command(&command).as_deref(),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn read_clipboard_text_with_command_rejects_oversized_output() {
        let command = ClipboardCommand {
            program: "sh",
            args: &["-c", "yes x | head -c 1048578"],
        };

        assert_eq!(read_clipboard_text_with_command(&command), None);
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_reads_under_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf image");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 16),
            Some(b"image".to_vec())
        );
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_rejects_over_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf oversized");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 4),
            None
        );
    }

    #[test]
    fn read_clipboard_image_rejects_xclip_text_served_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("herdr-fake-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&fake_xclip)
                .expect("fake xclip metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&fake_xclip, permissions)
                .expect("fake xclip should be executable");
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_clipboard_image_rejects_wayland_xclip_fallback_text_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("herdr-fake-wayland-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_wl_paste = temp_dir.join("wl-paste");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_wl_paste, "#!/bin/sh\nexit 1\n")
            .expect("fake wl-paste should be written");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            for command in [&fake_wl_paste, &fake_xclip] {
                let mut permissions = std::fs::metadata(command)
                    .expect("fake clipboard command metadata")
                    .permissions();
                permissions.set_mode(0o700);
                std::fs::set_permissions(command, permissions)
                    .expect("fake clipboard command should be executable");
            }
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_wl_paste);
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_validated_clipboard_image_accepts_real_png_payload() {
        assert_eq!(
            read_validated_clipboard_image(
                "sh",
                &["-c", "printf '\\211PNG\\r\\n\\032\\nrest-of-image'"],
                "png"
            ),
            Some(ClipboardImage {
                bytes: b"\x89PNG\r\n\x1a\nrest-of-image".to_vec(),
                extension: "png",
            })
        );
    }

    #[test]
    fn image_signatures_match_only_their_format() {
        assert!(bytes_match_image_signature("png", b"\x89PNG\r\n\x1a\n..."));
        assert!(bytes_match_image_signature(
            "jpg",
            &[0xFF, 0xD8, 0xFF, 0xE0]
        ));
        assert!(bytes_match_image_signature("gif", b"GIF87a..."));
        assert!(bytes_match_image_signature("gif", b"GIF89a..."));
        assert!(bytes_match_image_signature(
            "webp",
            b"RIFF\x10\x00\x00\x00WEBPVP8 "
        ));

        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10] = 26;
        assert!(bytes_match_image_signature("bmp", &bmp));

        assert!(!bytes_match_image_signature("png", b"# Tasks"));
        assert!(!bytes_match_image_signature("jpg", b"plain clipboard text"));
        assert!(!bytes_match_image_signature("gif", b""));
        assert!(!bytes_match_image_signature("webp", b"RIFF but not webp"));
        assert!(!bytes_match_image_signature("bmp", b"\x89PNG\r\n\x1a\n"));
        assert!(!bytes_match_image_signature(
            "bmp",
            b"BM text is not a bitmap"
        ));
        assert!(!bytes_match_image_signature("svg", b"<svg></svg>"));
    }

    #[test]
    fn desktop_notification_separates_option_like_titles() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }

        let path =
            std::env::temp_dir().join(format!("herdr-notify-send-args-{}", std::process::id()));
        let script = "printf '%s\\n' \"$@\" > \"$HERDR_NOTIFY_ARGS\"";
        let shown = show_desktop_notification_with_command("-danger", Some("body"), |_| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg("notify-send")
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        })
        .expect("notification command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(args, "--\n-danger\nbody\n");
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
