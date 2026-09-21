//! Sweep the networks in the server list for wireless adb devices.
//!
//! The fleet is normally on USB, so a device only shows up over the network
//! after something calls `adb connect` on it. Every entry the user adds is an
//! address they typed — `192.168.101.1`, one machine they know, or the segment
//! written out (`192.168.1.0/24`, `192.168.1.5-192.168.1.40`) — and hitting
//! refresh sweeps the segment that address sits in, one probe per address on the
//! wireless adb port (see [`expand_segment`]).
//!
//! Everything a sweep finds is attached to the local daemon (127.0.0.1:5037):
//! it is the one this app starts and the one the USB devices are on, so a phone
//! it connects is a phone the app can drive straight away. Probing is a plain
//! TCP connect, not an adb call: it needs no daemon, costs one short timeout per
//! address, and filters out everything that is not there before we make the
//! daemon talk to it.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::protocol::AdbClient;
use super::server::AdbServer;
use crate::config::{is_local_server, LOCAL_ADB_PORT, LOCAL_HOST};

/// Wireless adb (`adb tcpip 5555`) listens here.
pub const DEFAULT_ADB_TCP_PORT: u16 = 5555;
/// A device that is up answers in microseconds; this is for the ones that are
/// not there at all, which is nearly every address in a sweep.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const MAX_CONCURRENT_PROBES: usize = 64;
/// A /22 is already 1022 addresses; anything wider is a typo, and sweeping it
/// would take minutes.
const MAX_ADDRESSES: usize = 1024;
const PROGRESS_IPS_PER_EVENT: usize = 16;
const PROGRESS_MIN_GAP: Duration = Duration::from_millis(200);

/// `192.168.1.0/24` → every host address in it, `.0` (network) and `.255`
/// (broadcast) left out since no machine can hold them.
///
/// Also accepts an explicit range (`192.168.1.5-192.168.1.40`), the bare
/// three-octet shorthand `192.168.1`, and one device's own address — with or
/// without the port `adb connect` prints (`192.168.1.5`, `192.168.1.5:5555`),
/// which is read as the segment that device sits in: knowing one phone's IP is
/// knowing the LAN to sweep. A single address on its own is `/32`.
pub fn expand_segment(spec: &str) -> Result<Vec<Ipv4Addr>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(Vec::new());
    }

    // A pasted `ip:5555` is an address, not a new syntax; the port can only be
    // the one the sweep probes, so anything else is a mistake worth naming
    // rather than silently probing 5555 anyway.
    let spec = match spec.rsplit_once(':') {
        Some((addr, port)) => {
            let port: u16 = port
                .trim()
                .parse()
                .map_err(|_| format!("cannot read segment '{spec}'"))?;
            if port != DEFAULT_ADB_TCP_PORT {
                return Err(format!(
                    "'{spec}' asks for port {port}, but the sweep probes {DEFAULT_ADB_TCP_PORT}"
                ));
            }
            addr.trim()
        }
        None => spec,
    };

    if let Some((net, prefix)) = spec.split_once('/') {
        let base = parse_ipv4(net)?;
        let prefix: u32 = prefix
            .trim()
            .parse()
            .map_err(|_| format!("invalid prefix length in '{spec}'"))?;
        if !(16..=32).contains(&prefix) {
            return Err(format!("prefix /{prefix} not supported in '{spec}' (16-32)"));
        }
        let base = u32::from(base) & mask_for(prefix);
        let count = 1u32 << (32 - prefix);
        if count as usize > MAX_ADDRESSES {
            return Err(format!(
                "'{spec}' covers {count} addresses, more than the {MAX_ADDRESSES} limit"
            ));
        }
        // A /31 and /32 have no network/broadcast pair worth excluding.
        let range = if prefix >= 31 {
            base..=base + count - 1
        } else {
            base + 1..=base + count - 2
        };
        return Ok(range.map(Ipv4Addr::from).collect());
    }

    if let Some((start, end)) = spec.split_once('-') {
        let start = parse_ipv4(start)?;
        let end = parse_ipv4(end)?;
        if end < start {
            return Err(format!("range end is before its start in '{spec}'"));
        }
        let count = u32::from(end) - u32::from(start) + 1;
        if count as usize > MAX_ADDRESSES {
            return Err(format!(
                "'{spec}' covers {count} addresses, more than the {MAX_ADDRESSES} limit"
            ));
        }
        return Ok((u32::from(start)..=u32::from(end))
            .map(Ipv4Addr::from)
            .collect());
    }

    // One device's own address — sweep the segment it sits in.
    if let Ok(ip) = spec.parse::<Ipv4Addr>() {
        let base = Ipv4Addr::from(u32::from(ip) & mask_for(24));
        return expand_segment(&format!("{base}/24"));
    }

    // `192.168.1` — the same segment, written the short way.
    if spec.split('.').count() == 3 {
        return expand_segment(&format!("{spec}.0/24"));
    }

    Err(format!(
        "cannot read segment '{spec}' — use 192.168.1.0/24, 192.168.1.5-192.168.1.40 or 192.168.1.5"
    ))
}

fn parse_ipv4(text: &str) -> Result<Ipv4Addr, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("'{text}' is not an IPv4 address"))
}

fn mask_for(prefix: u32) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

/// Does anything answer on this address? A plain TCP connect: the adb daemon
/// is not involved, so this works before anything is connected.
fn probe(ip: Ipv4Addr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((ip, port)), PROBE_TIMEOUT).is_ok()
}

/// One sweep's state, emitted as the `scan-progress` event.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    /// The daemon the sweep attaches through — the local one, always.
    pub daemon_host: String,
    pub daemon_port: u16,
    pub segment: String,
    pub scanned: u32,
    pub total: u32,
    /// Addresses that answered, as `ip:port` — what the user cares about.
    pub found: Vec<String>,
    pub connected: u32,
    pub done: bool,
    /// Why a sweep could not run or could not attach everything it found.
    pub error: Option<String>,
}

fn emit(app: &AppHandle, progress: &ScanProgress) {
    let _ = app.emit("scan-progress", progress);
}

/// The segment an entry asks to sweep: its own address.
///
/// An entry the user added is where they typed the network they care about —
/// one address (`192.168.101.1`, meaning the /24 it sits in), a range, or a
/// `/24` written out. The local daemon is the exception: `127.0.0.0/24` holds
/// nothing worth probing, so it is never a segment.
fn segment_of(srv: &AdbServer) -> Option<String> {
    let spec = srv.host.trim();
    if spec.is_empty() || is_local_server(&srv.host, srv.port) {
        return None;
    }
    Some(spec.to_string())
}

/// What a sweep attaches through: the daemon this app runs, which holds the USB
/// devices. An entry names a network, not a daemon to attach through, so this is
/// the same for every address a sweep finds.
fn attach_daemon(servers: &[AdbServer]) -> (String, u16) {
    servers
        .iter()
        .find(|s| is_local_server(&s.host, s.port))
        .map(|s| (s.host.clone(), s.port))
        .unwrap_or_else(|| (LOCAL_HOST.to_string(), LOCAL_ADB_PORT))
}

/// Sweep every enabled entry's network, then attach what answers.
///
/// Progress is reported while each segment is swept, then one final event
/// covers the whole sweep: the refresh button stays disabled off that event,
/// and the line under it describes everything this refresh did rather than
/// just the last segment. With a single entry — the usual case — the two are
/// the same.
pub async fn scan_segments(servers: Vec<AdbServer>, app: AppHandle) {
    let (daemon_host, daemon_port) = attach_daemon(&servers);
    let mut claimed: HashSet<Ipv4Addr> = HashSet::new();
    let mut plans: Vec<(String, Vec<Ipv4Addr>)> = Vec::new();
    // Every segment the user asked for, in order, for the final event; and
    // whatever went wrong, held back until the sweep is over so one bad entry
    // does not end the run early in the UI.
    let mut segments: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for srv in servers.iter().filter(|s| s.enabled) {
        let Some(spec) = segment_of(srv) else { continue };
        if !segments.contains(&spec) {
            segments.push(spec.clone());
        }
        match expand_segment(&spec) {
            Ok(ips) => {
                // Overlapping segments are common (two entries on one LAN):
                // every address is probed once, by the entry that claimed it.
                let fresh: Vec<Ipv4Addr> = ips
                    .into_iter()
                    .filter(|ip| claimed.insert(*ip))
                    .collect();
                if !fresh.is_empty() {
                    plans.push((spec, fresh));
                }
            }
            Err(e) => {
                println!("[SCAN] segment '{spec}' invalid: {e}");
                errors.push(format!("{spec}: {e}"));
            }
        }
    }

    if plans.is_empty() {
        // Nothing to sweep. A segment that could not be read is the one thing
        // worth saying, so the entry shows why it did nothing.
        if !errors.is_empty() {
            emit(
                &app,
                &ScanProgress {
                    segment: segments.join(", "),
                    done: true,
                    error: Some(errors.join("; ")),
                    ..Default::default()
                },
            );
        }
        return;
    }

    let started = Instant::now();
    let mut total_addresses = 0u32;
    let mut found: Vec<String> = Vec::new();
    let mut connected = 0u32;

    for (spec, ips) in plans {
        println!(
            "[SCAN] sweeping {} ({} addresses) via {}:{}",
            spec,
            ips.len(),
            daemon_host,
            daemon_port
        );

        let total = ips.len() as u32;
        total_addresses += total;
        // Sent before the first probe: the sweep is running from the frontend's
        // point of view from here on, so the refresh button stays disabled
        // while the first addresses are still timing out.
        emit(
            &app,
            &ScanProgress {
                daemon_host: daemon_host.clone(),
                daemon_port,
                segment: spec.clone(),
                scanned: 0,
                total,
                ..Default::default()
            },
        );
        // Probing is blocking, and a sweep is mostly timeouts — run a bounded
        // number at a time on the blocking pool so a /24 finishes in seconds
        // instead of minutes.
        let mut probes = stream::iter(ips)
            .map(|ip| tokio::task::spawn_blocking(move || (ip, probe(ip, DEFAULT_ADB_TCP_PORT))))
            .buffer_unordered(MAX_CONCURRENT_PROBES);

        let mut found_ips: Vec<Ipv4Addr> = Vec::new();
        let mut scanned: u32 = 0;
        let mut last_emit = Instant::now();
        while let Some(result) = probes.next().await {
            scanned += 1;
            if let Ok((ip, true)) = result {
                found_ips.push(ip);
            }
            // One event per address would flood the webview during a sweep.
            if scanned as usize % PROGRESS_IPS_PER_EVENT == 0
                || last_emit.elapsed() >= PROGRESS_MIN_GAP
            {
                last_emit = Instant::now();
                emit(
                    &app,
                    &ScanProgress {
                        daemon_host: daemon_host.clone(),
                        daemon_port,
                        segment: spec.clone(),
                        scanned,
                        total,
                        // Everything that has answered so far this refresh,
                        // including the segments swept before this one.
                        found: found
                            .iter()
                            .cloned()
                            .chain(
                                found_ips
                                    .iter()
                                    .map(|ip| format!("{ip}:{DEFAULT_ADB_TCP_PORT}")),
                            )
                            .collect(),
                        ..Default::default()
                    },
                );
            }
        }
        found_ips.sort();
        if found_ips.is_empty() {
            println!("[SCAN] nothing answered on {spec}");
        } else {
            println!(
                "[SCAN] {} answered on {} ({})",
                found_ips.len(),
                spec,
                found_ips
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        if !found_ips.is_empty() {
            let host = daemon_host.clone();
            let port = daemon_port;
            let addresses: Vec<Ipv4Addr> = found_ips.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let mut connected = 0u32;
                let mut failures: Vec<String> = Vec::new();
                for ip in addresses {
                    let addr = format!("{ip}:{DEFAULT_ADB_TCP_PORT}");
                    let result = AdbClient::connect(&host, port).and_then(|mut client| {
                        client.connect_device(&ip.to_string(), DEFAULT_ADB_TCP_PORT)
                    });
                    match result {
                        Ok(message) => {
                            connected += 1;
                            println!("[SCAN] attached {addr} via {host}:{port} ({message})");
                        }
                        Err(e) => {
                            println!("[SCAN] attach {addr} via {host}:{port} failed: {e}");
                            failures.push(format!("{addr}: {e}"));
                        }
                    }
                }
                (connected, failures)
            })
            .await
            .unwrap_or_else(|e| (0, vec![format!("attach task failed: {e}")]));

            connected += outcome.0;
            if !outcome.1.is_empty() {
                errors.extend(outcome.1);
            }
        }

        found.extend(
            found_ips
                .iter()
                .map(|ip| format!("{ip}:{DEFAULT_ADB_TCP_PORT}")),
        );
    }

    println!(
        "[SCAN] {}/{} answered, attached {} in {:?}",
        found.len(),
        total_addresses,
        connected,
        started.elapsed()
    );
    emit(
        &app,
        &ScanProgress {
            daemon_host,
            daemon_port,
            segment: segments.join(", "),
            scanned: total_addresses,
            total: total_addresses,
            found,
            connected,
            done: true,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, port: u16) -> AdbServer {
        AdbServer::new(host.to_string(), port)
    }

    fn ips(spec: &str) -> Vec<String> {
        expand_segment(spec)
            .unwrap()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect()
    }

    #[test]
    fn expands_a_slash_24_without_network_and_broadcast() {
        let list = ips("192.168.1.0/24");

        assert_eq!(list.len(), 254);
        assert_eq!(list[0], "192.168.1.1");
        assert_eq!(list[list.len() - 1], "192.168.1.254");
    }

    #[test]
    fn expands_the_three_octet_shorthand_as_a_slash_24() {
        assert_eq!(ips("192.168.1"), ips("192.168.1.0/24"));
    }

    #[test]
    fn expands_an_explicit_range_inclusive_of_both_ends() {
        assert_eq!(
            ips("192.168.1.5-192.168.1.8"),
            ["192.168.1.5", "192.168.1.6", "192.168.1.7", "192.168.1.8"]
        );
    }

    #[test]
    fn a_single_host_prefix_keeps_that_host() {
        assert_eq!(ips("192.168.1.7/32"), ["192.168.1.7"]);
    }

    #[test]
    fn a_two_host_prefix_keeps_both_addresses() {
        assert_eq!(ips("192.168.1.32/31"), ["192.168.1.32", "192.168.1.33"]);
    }

    /// Someone who knows one device's address knows the segment it is in.
    #[test]
    fn one_devices_address_means_the_segment_it_is_in() {
        assert_eq!(ips("192.168.101.5"), ips("192.168.101.0/24"));
        assert_eq!(ips("192.168.101.0"), ips("192.168.101.0/24"));
    }

    /// The port `adb connect` prints travels with the address people paste.
    #[test]
    fn an_address_with_the_adb_port_means_the_same_segment() {
        assert_eq!(ips("192.168.101.5:5555"), ips("192.168.101.0/24"));
    }

    /// ... but a port the sweep does not probe must not be quietly ignored.
    #[test]
    fn rejects_a_port_the_sweep_does_not_probe() {
        let err = expand_segment("192.168.101.5:5556").unwrap_err();

        assert!(err.contains("probes 5555"), "{err}");
    }

    #[test]
    fn an_empty_spec_scans_nothing() {
        assert!(expand_segment("").unwrap().is_empty());
        assert!(expand_segment("   ").unwrap().is_empty());
    }

    #[test]
    fn rejects_a_segment_wider_than_the_limit() {
        let err = expand_segment("10.0.0.0/16").unwrap_err();

        assert!(err.contains("more than the 1024 limit"), "{err}");
    }

    #[test]
    fn rejects_ranges_and_prefixes_that_make_no_sense() {
        assert!(expand_segment("192.168.1.9-192.168.1.5").is_err());
        assert!(expand_segment("192.168.1.0/8").is_err());
        assert!(expand_segment("not-an-address").is_err());
        assert!(expand_segment("192.168.1.0/x").is_err());
    }

    /// The probe is the part that decides what gets attached, so it has to be
    /// right about both answers.
    #[test]
    fn probe_reports_open_and_closed_ports() {        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap();
        // Nothing is listening here: bind, note the port, hand it back.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap();
            drop(l);
            addr
        };

        assert!(probe(Ipv4Addr::LOCALHOST, open.port()));
        assert!(!probe(Ipv4Addr::LOCALHOST, closed.port()));
    }

    /// An entry the user adds names the network to sweep — its own address,
    /// however it is written. The local daemon's address is not a network.
    #[test]
    fn an_entry_sweeps_the_network_it_names() {
        assert_eq!(
            segment_of(&entry("192.168.101.1", 5037)).unwrap(),
            "192.168.101.1"
        );
        assert_eq!(
            segment_of(&entry("192.168.1.0/24", 5037)).unwrap(),
            "192.168.1.0/24"
        );
        assert!(segment_of(&entry("127.0.0.1", 5037)).is_none());
        assert!(segment_of(&entry("   ", 5037)).is_none());
    }

    /// A phone a sweep finds goes onto the local daemon, whichever entry's
    /// network it turned up on.
    #[test]
    fn a_sweep_attaches_through_the_local_daemon() {
        assert_eq!(
            attach_daemon(&[entry("192.168.101.1", 5037)]),
            (LOCAL_HOST.to_string(), LOCAL_ADB_PORT)
        );
        assert_eq!(
            attach_daemon(&[entry("192.168.101.1", 5037), entry("localhost", 5037)]),
            ("localhost".to_string(), LOCAL_ADB_PORT)
        );
    }
}
