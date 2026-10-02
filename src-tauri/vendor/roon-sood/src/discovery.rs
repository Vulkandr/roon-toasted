use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use crate::{ROON_CORE_SERVICE_ID, SOOD_MULTICAST_IP, SOOD_PORT, SoodType, parse, serialize_query};

/// Information about a discovered Roon Core on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCore {
    /// Unique identifier for this Roon Core instance.
    pub core_id: String,
    /// IP address to connect to (localhost-corrected if applicable).
    pub host: IpAddr,
    /// TCP port for the MOO/WebSocket API endpoint.
    pub http_port: u16,
    /// Human-readable core name broadcast in the SOOD response (e.g., "home"),
    /// if present. Roon Core always includes this in practice but the SOOD
    /// spec marks it as optional.
    pub name: Option<String>,
    /// Display version of the core (e.g., "2.65 (build 1648) earlyaccess"),
    /// if present in the SOOD response.
    pub display_version: Option<String>,
}

/// What discovery has done so far, for a diagnostics display.
///
/// Roon: Toasted addition (see VENDORED.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryDiagnostics {
    /// One entry per network adapter address the query is sent from.
    pub adapters: Vec<AdapterDiagnostics>,
    /// Replies that arrived on the sockets that aren't tied to one adapter
    /// (the OS-chosen send socket and the multicast listener).
    pub other_replies: u64,
    /// Times the query has gone out (periodic, on adapter changes, and on request).
    pub searches: u64,
    /// When the last search went out (seconds since 1970).
    pub last_search_unix: Option<u64>,
    /// When a Core last answered (seconds since 1970), and from where.
    pub last_reply_unix: Option<u64>,
    pub last_reply_from: Option<IpAddr>,
    /// Addresses queried directly (unicast) on every search, in addition to
    /// the multicast and broadcast queries.
    pub known_hosts: Vec<Ipv4Addr>,
}

/// Counters for one network adapter address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterDiagnostics {
    pub ip: Ipv4Addr,
    pub queries_sent: u64,
    pub replies: u64,
}

type Diag = Arc<Mutex<DiscoveryDiagnostics>>;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

enum Command {
    SearchNow,
    SetKnownHosts(Vec<Ipv4Addr>),
}

/// A cheap, cloneable handle for steering a running discovery.
#[derive(Clone)]
pub struct SoodControl {
    cmd_tx: mpsc::UnboundedSender<Command>,
    diag: Diag,
}

impl SoodControl {
    /// Send the query again right now, whether or not a Core is paired.
    pub fn search_now(&self) {
        let _ = self.cmd_tx.send(Command::SearchNow);
    }

    /// Also query these addresses directly on every search. A direct query
    /// doesn't depend on multicast or broadcast reaching the Core. Replaces
    /// the previous list, and searches immediately.
    pub fn set_known_hosts(&self, hosts: Vec<Ipv4Addr>) {
        let _ = self.cmd_tx.send(Command::SetKnownHosts(hosts));
    }

    pub fn diagnostics(&self) -> DiscoveryDiagnostics {
        self.diag.lock().unwrap().clone()
    }
}

/// SOOD network discovery for Roon Cores.
pub struct SoodDiscovery {
    control: SoodControl,
    cancel_tx: watch::Sender<bool>,
    paired_tx: watch::Sender<bool>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl SoodDiscovery {
    /// Start SOOD discovery.
    pub async fn start() -> Result<(Self, broadcast::Receiver<DiscoveredCore>), crate::SoodError> {
        let (core_tx, core_rx) = broadcast::channel::<DiscoveredCore>(16);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (paired_tx, paired_rx) = watch::channel(false);

        let recv_socket = bind_recv_socket().await?;
        let send_socket = bind_send_socket().await?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let diag: Diag = Arc::new(Mutex::new(DiscoveryDiagnostics::default()));

        let task_handle = tokio::spawn(discovery_loop(
            recv_socket,
            send_socket,
            core_tx,
            cancel_rx,
            paired_rx,
            cmd_rx,
            diag.clone(),
        ));

        Ok((
            SoodDiscovery {
                control: SoodControl { cmd_tx, diag },
                cancel_tx,
                paired_tx,
                task_handle,
            },
            core_rx,
        ))
    }

    /// A handle for searching again, adding direct queries and reading diagnostics.
    pub fn control(&self) -> SoodControl {
        self.control.clone()
    }

    /// Suppress periodic queries when paired (saves network traffic).
    pub fn set_paired(&self, paired: bool) {
        let _ = self.paired_tx.send(paired);
    }

    /// Stop discovery and release network resources.
    pub async fn stop(self) {
        let _ = self.cancel_tx.send(true);
        let _ = self.task_handle.await;
    }
}

async fn bind_recv_socket() -> Result<UdpSocket, crate::SoodError> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    socket
        .set_reuse_address(true)
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    socket
        .set_nonblocking(true)
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SOOD_PORT);
    socket
        .bind(&addr.into())
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    let multicast_addr: Ipv4Addr = SOOD_MULTICAST_IP
        .parse()
        .expect("hardcoded multicast IP is valid");
    socket
        .join_multicast_v4(&multicast_addr, &Ipv4Addr::UNSPECIFIED)
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket).map_err(|e| crate::SoodError::Io(e.to_string()))
}

async fn bind_send_socket() -> Result<UdpSocket, crate::SoodError> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    socket
        .set_broadcast(true)
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    socket
        .set_multicast_ttl_v4(1)
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    socket
        .set_nonblocking(true)
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
    socket
        .bind(&addr.into())
        .map_err(|e| crate::SoodError::Io(e.to_string()))?;

    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket).map_err(|e| crate::SoodError::Io(e.to_string()))
}

/// One IPv4 address on one network adapter, with its subnet broadcast address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct LocalIface {
    ip: Ipv4Addr,
    /// Subnet-directed broadcast (e.g. 192.168.1.255), if the subnet has one.
    broadcast: Option<Ipv4Addr>,
}

/// Subnet-directed broadcast address for an address and netmask. A /31 or /32
/// has no broadcast address.
fn directed_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let mask = u32::from(netmask);
    if mask >= 0xFFFF_FFFE {
        return None;
    }
    Some(Ipv4Addr::from(u32::from(ip) | !mask))
}

/// Every usable IPv4 address on this machine (all adapters, loopback excluded),
/// sorted so two calls can be compared.
///
/// Roon: Toasted change (see VENDORED.md): used to send discovery queries out
/// of every adapter instead of letting the OS pick one.
fn list_ipv4_interfaces() -> Vec<LocalIface> {
    let mut out = Vec::new();
    match if_addrs::get_if_addrs() {
        Ok(ifaces) => {
            for iface in ifaces {
                if let if_addrs::IfAddr::V4(v4) = iface.addr {
                    if v4.ip.is_loopback() || v4.ip.is_unspecified() {
                        continue;
                    }
                    out.push(LocalIface {
                        ip: v4.ip,
                        broadcast: directed_broadcast(v4.ip, v4.netmask),
                    });
                }
            }
        }
        Err(e) => tracing::warn!("Could not list network adapters: {}", e),
    }
    out.sort();
    out.dedup();
    out
}

/// A send socket bound to one adapter's address, plus the task that reads the
/// Core's replies on it (replies come back to the address and port the query
/// was sent from). Dropping this stops the task and closes the socket.
struct IfaceSender {
    iface: LocalIface,
    socket: Arc<UdpSocket>,
    reader: JoinHandle<()>,
}

impl Drop for IfaceSender {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn bind_iface_socket(ip: Ipv4Addr) -> Result<UdpSocket, crate::SoodError> {
    let io = |e: std::io::Error| crate::SoodError::Io(e.to_string());
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(io)?;
    socket.set_broadcast(true).map_err(io)?;
    socket.set_multicast_ttl_v4(1).map_err(io)?;
    // Send multicast out of this adapter, not whichever one the OS prefers.
    socket.set_multicast_if_v4(&ip).map_err(io)?;
    socket.set_nonblocking(true).map_err(io)?;
    socket
        .bind(&SocketAddrV4::new(ip, 0).into())
        .map_err(io)?;
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket).map_err(io)
}

/// Make `senders` match the adapters that exist right now: open a socket on each
/// new one, drop the ones that went away. Returns true if anything changed.
fn sync_senders(
    senders: &mut Vec<IfaceSender>,
    recv_socket: &UdpSocket,
    core_tx: &broadcast::Sender<DiscoveredCore>,
    local_addrs: &Arc<Mutex<HashSet<IpAddr>>>,
    diag: &Diag,
) -> bool {
    let current = list_ipv4_interfaces();
    let before = senders.len();
    senders.retain(|s| current.contains(&s.iface));
    let mut changed = senders.len() != before;

    let multicast_addr: Ipv4Addr = SOOD_MULTICAST_IP
        .parse()
        .expect("hardcoded multicast IP is valid");

    for iface in current {
        if senders.iter().any(|s| s.iface == iface) {
            continue;
        }
        let socket = match bind_iface_socket(iface.ip) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::debug!("SOOD: skipping adapter {}: {}", iface.ip, e);
                continue;
            }
        };
        // Also listen for multicast on this adapter (the receive socket only
        // joined on the OS-default one).
        if let Err(e) = recv_socket.join_multicast_v4(multicast_addr, iface.ip) {
            tracing::debug!("SOOD: multicast join on {} failed: {}", iface.ip, e);
        }
        let reader = tokio::spawn(read_replies(
            socket.clone(),
            Some(iface.ip),
            core_tx.clone(),
            local_addrs.clone(),
            diag.clone(),
        ));
        tracing::info!("SOOD: searching on adapter {}", iface.ip);
        senders.push(IfaceSender {
            iface,
            socket,
            reader,
        });
        changed = true;
    }

    if changed {
        // Keep the counters of adapters that are still there; add the new ones.
        let mut d = diag.lock().unwrap();
        let old = std::mem::take(&mut d.adapters);
        d.adapters = senders
            .iter()
            .map(|s| {
                old.iter()
                    .find(|a| a.ip == s.iface.ip)
                    .cloned()
                    .unwrap_or(AdapterDiagnostics {
                        ip: s.iface.ip,
                        queries_sent: 0,
                        replies: 0,
                    })
            })
            .collect();
    }
    changed
}

/// Count a reply from a Core, on one adapter's socket (`adapter`) or on the
/// shared ones (None).
fn record_reply(diag: &Diag, adapter: Option<Ipv4Addr>, from: SocketAddr) {
    let mut d = diag.lock().unwrap();
    d.last_reply_unix = Some(now_unix());
    d.last_reply_from = Some(from.ip());
    match adapter.and_then(|ip| d.adapters.iter_mut().find(|a| a.ip == ip)) {
        Some(a) => a.replies += 1,
        None => d.other_replies += 1,
    }
}

async fn read_replies(
    socket: Arc<UdpSocket>,
    adapter: Option<Ipv4Addr>,
    core_tx: broadcast::Sender<DiscoveredCore>,
    local_addrs: Arc<Mutex<HashSet<IpAddr>>>,
    diag: Diag,
) {
    let mut buf = vec![0u8; 65535];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, from)) => {
                let core = {
                    let addrs = local_addrs.lock().unwrap();
                    process_response(&buf[..len], from, &addrs)
                };
                if let Some(core) = core {
                    record_reply(&diag, adapter, from);
                    let _ = core_tx.send(core);
                }
            }
            Err(e) => {
                // Usually the adapter went away; the next interface poll
                // drops this socket. Wait so a dead socket can't spin.
                tracing::debug!("SOOD recv error (adapter socket): {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

fn build_query_packet() -> Vec<u8> {
    let mut props = HashMap::new();
    props.insert(
        "query_service_id".to_string(),
        Some(ROON_CORE_SERVICE_ID.to_string()),
    );
    props.insert("_tid".to_string(), Some(uuid::Uuid::new_v4().to_string()));
    serialize_query(&props)
}

/// Get all local IPv4 addresses for localhost detection.
fn get_local_ipv4_addrs() -> HashSet<IpAddr> {
    let mut addrs = HashSet::new();
    addrs.insert(IpAddr::V4(Ipv4Addr::LOCALHOST));

    // Roon: Toasted change (see VENDORED.md): not on Windows. Its hostname.exe
    // has no -I (it only prints an error, so this found nothing there anyway),
    // and starting it from a windowed app flashes a console window, every
    // 5 seconds while discovery runs.
    #[cfg(not(windows))]
    {
        // Read from /proc/net/fib_trie or fall back to parsing ip addr
        if let Ok(output) = std::process::Command::new("hostname").arg("-I").output()
            && let Ok(stdout) = std::str::from_utf8(&output.stdout)
        {
            for part in stdout.split_whitespace() {
                if let Ok(ip) = part.parse::<IpAddr>() {
                    addrs.insert(ip);
                }
            }
        }
    }

    addrs
}

async fn discovery_loop(
    recv_socket: UdpSocket,
    send_socket: UdpSocket,
    core_tx: broadcast::Sender<DiscoveredCore>,
    mut cancel_rx: watch::Receiver<bool>,
    paired_rx: watch::Receiver<bool>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    diag: Diag,
) {
    let multicast_target: SocketAddr = SocketAddr::V4(SocketAddrV4::new(
        SOOD_MULTICAST_IP.parse().unwrap(),
        SOOD_PORT,
    ));
    let broadcast_target: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, SOOD_PORT));

    // Cache local addresses, refresh periodically
    let local_addrs = Arc::new(Mutex::new(get_local_ipv4_addrs()));

    // One socket per network adapter, so the query goes out of all of them.
    // `send_socket` (OS-chosen adapter) stays as well, as a safety net.
    let mut senders: Vec<IfaceSender> = Vec::new();
    let mut known_hosts: Vec<Ipv4Addr> = Vec::new();
    sync_senders(&mut senders, &recv_socket, &core_tx, &local_addrs, &diag);

    send_query(
        &send_socket,
        &senders,
        &known_hosts,
        &multicast_target,
        &broadcast_target,
        &diag,
    )
    .await;

    let mut scan_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut iface_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    let mut tick_count: u64 = 0;
    let mut recv_buf = vec![0u8; 65535];
    let mut send_buf = vec![0u8; 65535];

    loop {
        tokio::select! {
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    break;
                }
            }

            // Requests from the app (search again, direct queries)
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Command::SearchNow => {}
                    Command::SetKnownHosts(hosts) => {
                        diag.lock().unwrap().known_hosts = hosts.clone();
                        known_hosts = hosts;
                    }
                }
                send_query(&send_socket, &senders, &known_hosts, &multicast_target, &broadcast_target, &diag).await;
            }

            // Interface polling (every 5s)
            _ = iface_interval.tick() => {
                let new_addrs = get_local_ipv4_addrs();
                let addrs_changed = {
                    let mut addrs = local_addrs.lock().unwrap();
                    let changed = *addrs != new_addrs;
                    if changed {
                        *addrs = new_addrs;
                    }
                    changed
                };
                let senders_changed =
                    sync_senders(&mut senders, &recv_socket, &core_tx, &local_addrs, &diag);
                if addrs_changed || senders_changed {
                    tracing::debug!("Network interfaces changed");
                    // Trigger immediate query on interface change
                    send_query(&send_socket, &senders, &known_hosts, &multicast_target, &broadcast_target, &diag).await;
                }
            }

            // Periodic query
            _ = scan_interval.tick() => {
                tick_count += 1;
                // Skip queries when paired
                if *paired_rx.borrow() {
                    continue;
                }
                if tick_count <= 6 || tick_count.is_multiple_of(6) {
                    send_query(&send_socket, &senders, &known_hosts, &multicast_target, &broadcast_target, &diag).await;
                }
            }

            result = recv_socket.recv_from(&mut recv_buf) => {
                match result {
                    Ok((len, from)) => {
                        let core = {
                            let addrs = local_addrs.lock().unwrap();
                            process_response(&recv_buf[..len], from, &addrs)
                        };
                        if let Some(core) = core {
                            record_reply(&diag, None, from);
                            let _ = core_tx.send(core);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("SOOD recv error (recv_socket): {}", e);
                    }
                }
            }

            result = send_socket.recv_from(&mut send_buf) => {
                match result {
                    Ok((len, from)) => {
                        let core = {
                            let addrs = local_addrs.lock().unwrap();
                            process_response(&send_buf[..len], from, &addrs)
                        };
                        if let Some(core) = core {
                            record_reply(&diag, None, from);
                            let _ = core_tx.send(core);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("SOOD recv error (send_socket): {}", e);
                    }
                }
            }
        }
    }
}

/// Send the query out of every adapter (multicast, the limited broadcast and the
/// subnet's own broadcast), directly to each known host, and once more from the
/// OS-chosen socket.
async fn send_query(
    default_socket: &UdpSocket,
    senders: &[IfaceSender],
    known_hosts: &[Ipv4Addr],
    multicast_target: &SocketAddr,
    broadcast_target: &SocketAddr,
    diag: &Diag,
) {
    let packet = build_query_packet();
    let mut sent_from: Vec<Ipv4Addr> = Vec::new();

    for sender in senders {
        let ip = sender.iface.ip;
        let mut ok = false;
        match sender.socket.send_to(&packet, multicast_target).await {
            Ok(_) => ok = true,
            Err(e) => tracing::debug!("SOOD multicast send on {} failed: {}", ip, e),
        }
        match sender.socket.send_to(&packet, broadcast_target).await {
            Ok(_) => ok = true,
            Err(e) => tracing::debug!("SOOD broadcast send on {} failed: {}", ip, e),
        }
        if let Some(b) = sender.iface.broadcast {
            let target = SocketAddr::V4(SocketAddrV4::new(b, SOOD_PORT));
            match sender.socket.send_to(&packet, target).await {
                Ok(_) => ok = true,
                Err(e) => tracing::debug!("SOOD subnet broadcast send on {} failed: {}", ip, e),
            }
        }
        if ok {
            sent_from.push(ip);
        }
    }

    if let Err(e) = default_socket.send_to(&packet, multicast_target).await {
        tracing::debug!("SOOD multicast send failed: {}", e);
    }
    if let Err(e) = default_socket.send_to(&packet, broadcast_target).await {
        tracing::debug!("SOOD broadcast send failed: {}", e);
    }
    // Direct (unicast) queries: no multicast or broadcast needed to reach these.
    for host in known_hosts {
        let target = SocketAddr::V4(SocketAddrV4::new(*host, SOOD_PORT));
        if let Err(e) = default_socket.send_to(&packet, target).await {
            tracing::debug!("SOOD direct query to {} failed: {}", host, e);
        }
    }

    let mut d = diag.lock().unwrap();
    d.searches += 1;
    d.last_search_unix = Some(now_unix());
    for a in d.adapters.iter_mut() {
        if sent_from.contains(&a.ip) {
            a.queries_sent += 1;
        }
    }
}

/// Process a received SOOD response, applying localhost detection.
fn process_response(
    buf: &[u8],
    from: SocketAddr,
    local_addrs: &HashSet<IpAddr>,
) -> Option<DiscoveredCore> {
    let msg = match parse(buf, from) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("SOOD parse error: {}", e);
            return None;
        }
    };

    if msg.msg_type != SoodType::Response {
        return None;
    }

    let core_id = msg.props.get("unique_id")?.as_ref()?.clone();
    let http_port_str = msg.props.get("http_port")?.as_ref()?;
    let http_port: u16 = http_port_str.parse().ok()?;
    let name = msg.props.get("name").and_then(|v| v.clone());
    let display_version = msg.props.get("display_version").and_then(|v| v.clone());

    // Localhost detection: if the response IP is one of our own addresses,
    // connect to 127.0.0.1 instead (avoids loopback issues)
    let host = if local_addrs.contains(&msg.from.ip()) {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        msg.from.ip()
    };

    Some(DiscoveredCore {
        core_id,
        host,
        http_port,
        name,
        display_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_response_valid() {
        let mut props = HashMap::new();
        props.insert(
            "service_id".to_string(),
            Some(ROON_CORE_SERVICE_ID.to_string()),
        );
        props.insert("unique_id".to_string(), Some("test-core-123".to_string()));
        props.insert("http_port".to_string(), Some("9100".to_string()));
        props.insert("_tid".to_string(), Some("tid-placeholder".to_string()));

        let mut buf = Vec::new();
        buf.extend_from_slice(b"SOOD\x02R");
        for (name, value) in &props {
            buf.push(name.len() as u8);
            buf.extend_from_slice(name.as_bytes());
            match value {
                Some(v) => {
                    buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                    buf.extend_from_slice(v.as_bytes());
                }
                None => {
                    buf.extend_from_slice(&0xFFFFu16.to_be_bytes());
                }
            }
        }

        let from: SocketAddr = "192.168.1.100:9003".parse().unwrap();
        let empty_local = HashSet::new();
        let core = process_response(&buf, from, &empty_local).unwrap();
        assert_eq!(core.core_id, "test-core-123");
        assert_eq!(core.http_port, 9100);
        assert_eq!(core.host, IpAddr::V4("192.168.1.100".parse().unwrap()));
    }

    #[test]
    fn test_process_response_localhost_detection() {
        let mut props = HashMap::new();
        props.insert("unique_id".to_string(), Some("local-core".to_string()));
        props.insert("http_port".to_string(), Some("9330".to_string()));

        let mut buf = Vec::new();
        buf.extend_from_slice(b"SOOD\x02R");
        for (name, value) in &props {
            buf.push(name.len() as u8);
            buf.extend_from_slice(name.as_bytes());
            if let Some(v) = value {
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                buf.extend_from_slice(v.as_bytes());
            }
        }

        let from: SocketAddr = "192.168.1.20:9003".parse().unwrap();
        let mut local = HashSet::new();
        local.insert(IpAddr::V4("192.168.1.20".parse().unwrap()));

        let core = process_response(&buf, from, &local).unwrap();
        assert_eq!(core.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn test_process_response_ignores_queries() {
        let buf = b"SOOD\x02Q";
        let from: SocketAddr = "192.168.1.100:9003".parse().unwrap();
        assert!(process_response(buf, from, &HashSet::new()).is_none());
    }

    #[test]
    fn test_process_response_missing_fields() {
        let buf = b"SOOD\x02R";
        let from: SocketAddr = "192.168.1.100:9003".parse().unwrap();
        assert!(process_response(buf, from, &HashSet::new()).is_none());
    }

    #[test]
    fn test_build_query_packet_is_valid() {
        let packet = build_query_packet();
        let from: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let msg = parse(&packet, from).unwrap();
        assert_eq!(msg.msg_type, SoodType::Query);
        assert_eq!(
            msg.props.get("query_service_id").unwrap().as_ref().unwrap(),
            ROON_CORE_SERVICE_ID
        );
        assert!(msg.props.contains_key("_tid"));
    }

    #[tokio::test]
    async fn test_loopback_send_recv() {
        let recv = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv.local_addr().unwrap();

        let send = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        send.set_broadcast(true).unwrap();

        let packet = build_query_packet();
        send.send_to(&packet, recv_addr).await.unwrap();

        let mut buf = vec![0u8; 65535];
        let (len, from) =
            tokio::time::timeout(std::time::Duration::from_secs(1), recv.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();

        let msg = parse(&buf[..len], from).unwrap();
        assert_eq!(msg.msg_type, SoodType::Query);
    }

    #[test]
    fn test_directed_broadcast() {
        let ip: Ipv4Addr = "192.168.1.20".parse().unwrap();
        assert_eq!(
            directed_broadcast(ip, "255.255.255.0".parse().unwrap()),
            Some("192.168.1.255".parse().unwrap())
        );
        assert_eq!(
            directed_broadcast("10.1.2.3".parse().unwrap(), "255.255.0.0".parse().unwrap()),
            Some("10.1.255.255".parse().unwrap())
        );
        assert_eq!(directed_broadcast(ip, "255.255.255.255".parse().unwrap()), None);
        assert_eq!(directed_broadcast(ip, "255.255.255.254".parse().unwrap()), None);
    }

    #[test]
    fn test_list_ipv4_interfaces_skips_loopback() {
        for iface in list_ipv4_interfaces() {
            assert!(!iface.ip.is_loopback());
            assert!(!iface.ip.is_unspecified());
        }
    }

    #[tokio::test]
    async fn test_each_adapter_socket_sends_and_receives() {
        // A query sent from an adapter socket reaches a listener, and a reply
        // sent back to that socket comes through its reader as a DiscoveredCore.
        let ifaces = list_ipv4_interfaces();
        let Some(iface) = ifaces.first().copied() else {
            return; // no network adapters in this environment
        };
        let sock = match bind_iface_socket(iface.ip) {
            Ok(s) => Arc::new(s),
            Err(_) => return,
        };
        let port = sock.local_addr().unwrap().port();
        let (tx, mut rx) = broadcast::channel(4);
        let local = Arc::new(Mutex::new(HashSet::new()));
        let diag: Diag = Arc::new(Mutex::new(DiscoveryDiagnostics::default()));
        let task = tokio::spawn(read_replies(sock.clone(), Some(iface.ip), tx, local, diag.clone()));

        let mut buf = Vec::new();
        buf.extend_from_slice(b"SOOD\x02R");
        for (name, v) in [("unique_id", "core-x"), ("http_port", "9330")] {
            buf.push(name.len() as u8);
            buf.extend_from_slice(name.as_bytes());
            buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
            buf.extend_from_slice(v.as_bytes());
        }
        let peer = UdpSocket::bind((iface.ip, 0)).await.unwrap();
        peer.send_to(&buf, (iface.ip, port)).await.unwrap();

        let core = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(core.core_id, "core-x");
        assert_eq!(core.http_port, 9330);
        assert_eq!(diag.lock().unwrap().last_reply_from, Some(IpAddr::V4(iface.ip)));
        task.abort();
    }

    #[tokio::test]
    async fn test_control_search_and_known_hosts() {
        let (disc, _rx) = SoodDiscovery::start().await.unwrap();
        let control = disc.control();
        // The first search goes out as soon as discovery starts.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let first = control.diagnostics();
        assert!(first.searches >= 1);
        assert!(first.last_search_unix.is_some());

        control.set_known_hosts(vec!["127.0.0.1".parse().unwrap()]);
        control.search_now();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let after = control.diagnostics();
        assert!(after.searches >= first.searches + 2);
        assert_eq!(after.known_hosts, vec!["127.0.0.1".parse::<Ipv4Addr>().unwrap()]);
        disc.stop().await;
    }

    #[test]
    fn test_get_local_ipv4_addrs() {
        let addrs = get_local_ipv4_addrs();
        assert!(addrs.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }
}
