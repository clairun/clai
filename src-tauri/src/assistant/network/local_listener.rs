//! Finds the program listening on a port of this computer, so the network
//! proxy connects to a localhost port only while the program the user allowed
//! is the one listening there.
//!
//! Everything comes from `/proc`, read through [`ProcSource`] so tests can
//! describe a process table instead.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::fs;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// `st` of a listening socket in `/proc/net/tcp`.
const LISTEN: &str = "0A";
/// How far up the parent chain a holder is checked for CLAI.
const MAX_ANCESTORS: usize = 64;
/// The command line is for the card only.
const MAX_CMDLINE: u64 = 2048;
/// What `/proc/<pid>/exe` appends once the file is unlinked.
const DELETED: &str = " (deleted)";
/// The uid `/proc` shows for every user not mapped into the user namespace
/// of CLAI, as in a Flatpak: the default `/proc/sys/kernel/overflowuid`.
const OVERFLOW_UID: u32 = 65534;
/// The largest buffer offered to `getpwuid_r`.
const MAX_PASSWD_BUF: usize = 1 << 20;
/// The first read of `/proc/net/tcp`: about 400 sockets.
const NET_TCP_CHUNK: usize = 64 * 1024;

/// The loopback address the proxy dials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loopback {
    V4,
    V6,
}

impl Loopback {
    /// Whether a socket listening on `addr` accepts a connection to this
    /// address. A `::` socket counts for IPv4 too: /proc does not show
    /// `IPV6_V6ONLY`, and a dial to such a socket just fails.
    fn reaches(self, addr: IpAddr) -> bool {
        match (self, addr) {
            (Self::V4, IpAddr::V4(ip)) => ip == Ipv4Addr::LOCALHOST || ip.is_unspecified(),
            (Self::V4, IpAddr::V6(ip)) => {
                ip.is_unspecified()
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|ip| self.reaches(IpAddr::V4(ip)))
            }
            (Self::V6, IpAddr::V6(ip)) => ip == Ipv6Addr::LOCALHOST || ip.is_unspecified(),
            (Self::V6, IpAddr::V4(_)) => false,
        }
    }
}

/// What listens on a localhost port, as CLAI sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listener {
    /// Nothing listens on the port at an address the dial reaches.
    Nothing,
    /// CLAI or a process it started holds the socket: never connected to.
    Clai,
    /// One program, at a path a saved grant can name.
    Program(Program),
    /// Sockets of another user whose processes CLAI cannot see: a saved
    /// grant can name the user, by uid, never a program.
    OtherUser {
        uid: u32,
        /// For display.
        name: String,
        sockets: Vec<u64>,
    },
    /// Nothing a saved grant could name: only these sockets, for one run.
    RunOnly {
        reason: RunOnly,
        sockets: Vec<u64>,
        holders: Vec<Holder>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// The file the process runs, at a path that is valid UTF-8.
    pub exe: String,
    /// The lowest pid holding the socket: a forking server has several.
    pub pid: u32,
    pub cmdline: String,
    /// The program runs whatever script, or relays wherever, it is told
    /// (python, node, socat), so its path says little about the listener.
    pub interpreter: bool,
}

/// Why no saved grant can name the listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOnly {
    /// CLAI cannot see every process behind the sockets, and they are not
    /// those of one other user it can name: they belong to its own user (as
    /// a Flatpak, CLAI sees no host processes), to a uid its user runs
    /// containers as, to a uid without a user name, or to several users.
    Unseen,
    /// A holder runs in another pid or mount namespace, such as a container
    /// or another sandbox, where its path names a different file.
    OtherNamespace,
    /// Different programs hold sockets on the port.
    SeveralPrograms,
    /// The file at the program path is not the one running: it was
    /// replaced or deleted since the program started.
    Replaced,
    /// The program path is not UTF-8, so a grant cannot store it.
    PathNotUtf8,
}

/// A process holding a listening socket, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// Lossy when the path is not UTF-8, and maybe not the running file.
    pub exe: String,
    pub cmdline: String,
}

/// Finds what listens on `port` at an address a dial to `dial` reaches.
/// Blocks on `/proc` reads: a scan of every fd of this user when the port
/// is held.
pub fn inspect(port: u16, dial: Loopback) -> io::Result<Listener> {
    lookup(&HostProc, port, dial)
}

/// A listening socket from `/proc/net/tcp` or `tcp6`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Socket {
    addr: IpAddr,
    port: u16,
    uid: u32,
    inode: u64,
}

/// `/proc/<pid>/ns/pid` and `ns/mnt` targets, such as `pid:[4026531836]`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Namespaces {
    pid: PathBuf,
    mnt: PathBuf,
}

/// A visible process holding one of the sockets.
struct Seen {
    pid: u32,
    exe: PathBuf,
    namespaces: Namespaces,
    cmdline: String,
}

impl From<&Seen> for Holder {
    fn from(seen: &Seen) -> Self {
        Self {
            pid: seen.pid,
            exe: seen.exe.to_string_lossy().into_owned(),
            cmdline: seen.cmdline.clone(),
        }
    }
}

/// The `/proc` reads a lookup makes; [`HostProc`] makes the real ones.
trait ProcSource {
    /// `/proc/self/net/tcp`, or `tcp6`.
    fn net_tcp(&self, v6: bool) -> io::Result<String>;
    /// The pid `/proc/self` names, so CLAI is found in the same `/proc` as
    /// the holders.
    fn self_pid(&self) -> Option<u32>;
    fn euid(&self) -> u32;
    /// The name of user `uid` in the user database.
    fn user_name(&self, uid: u32) -> Option<String>;
    /// The text of `/etc/subuid`; empty when it cannot be read.
    fn subuid(&self) -> String;
    fn pids(&self) -> Vec<u32>;
    /// The socket inodes among the fds of `pid`; none when unreadable.
    fn socket_inodes(&self, pid: u32) -> Vec<u64>;
    fn parent(&self, pid: u32) -> Option<u32>;
    fn namespaces(&self, pid: u32) -> Option<Namespaces>;
    /// The target of `/proc/<pid>/exe`, as the kernel prints it.
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    /// Whether `path` is the file `pid` runs.
    fn runs(&self, pid: u32, path: &Path) -> bool;
    fn cmdline(&self, pid: u32) -> String;
}

fn lookup(proc: &impl ProcSource, port: u16, dial: Loopback) -> io::Result<Listener> {
    let mut sockets = parse_proc_net(&proc.net_tcp(false)?);
    sockets.extend(parse_proc_net(&proc.net_tcp(true)?));
    sockets.retain(|socket| socket.port == port && dial.reaches(socket.addr));
    if sockets.is_empty() {
        return Ok(Listener::Nothing);
    }
    let clai = proc
        .self_pid()
        .ok_or_else(|| io::Error::other("cannot read the pid of CLAI"))?;
    let clai_namespaces = proc
        .namespaces(clai)
        .ok_or_else(|| io::Error::other("cannot read the namespaces of CLAI"))?;
    let inodes: BTreeSet<u64> = sockets.iter().map(|socket| socket.inode).collect();
    let mut seen = Vec::new();
    let mut seen_inodes = BTreeSet::new();
    let mut unreadable = false;
    let mut pids = proc.pids();
    pids.sort_unstable();
    for pid in pids {
        let held: Vec<u64> = proc
            .socket_inodes(pid)
            .into_iter()
            .filter(|inode| inodes.contains(inode))
            .collect();
        if held.is_empty() {
            continue;
        }
        if is_clai(proc, pid, clai) {
            return Ok(Listener::Clai);
        }
        let (Some(namespaces), Some(exe)) = (proc.namespaces(pid), proc.exe(pid)) else {
            unreadable = true;
            continue;
        };
        // The pid may have exited and been reused since its fds were read.
        if !proc
            .socket_inodes(pid)
            .iter()
            .any(|inode| held.contains(inode))
        {
            continue;
        }
        seen_inodes.extend(held);
        seen.push(Seen {
            pid,
            exe,
            namespaces,
            cmdline: proc.cmdline(pid),
        });
    }
    // A holder that is exiting, or that CLAI may not inspect.
    if unreadable {
        return Ok(run_only(RunOnly::Unseen, &sockets, &seen));
    }
    Ok(classify(
        proc,
        &sockets,
        &seen,
        &seen_inodes,
        &clai_namespaces,
    ))
}

/// `seen` is sorted by pid and holds no CLAI process.
fn classify(
    proc: &impl ProcSource,
    sockets: &[Socket],
    seen: &[Seen],
    seen_inodes: &BTreeSet<u64>,
    clai_namespaces: &Namespaces,
) -> Listener {
    if seen
        .iter()
        .any(|holder| holder.namespaces != *clai_namespaces)
    {
        return run_only(RunOnly::OtherNamespace, sockets, seen);
    }
    if sockets
        .iter()
        .any(|socket| !seen_inodes.contains(&socket.inode))
    {
        if seen.is_empty() {
            if let Some((uid, name)) = other_user(proc, sockets) {
                return Listener::OtherUser {
                    uid,
                    name,
                    sockets: inodes(sockets),
                };
            }
        }
        return run_only(RunOnly::Unseen, sockets, seen);
    }
    let Some(first) = seen.first() else {
        return run_only(RunOnly::Unseen, sockets, seen);
    };
    if seen.iter().any(|holder| holder.exe != first.exe) {
        return run_only(RunOnly::SeveralPrograms, sockets, seen);
    }
    match program_path(proc, first) {
        Ok(exe) => Listener::Program(Program {
            interpreter: is_interpreter(&exe),
            exe,
            pid: first.pid,
            cmdline: first.cmdline.clone(),
        }),
        Err(reason) => run_only(reason, sockets, seen),
    }
}

/// The other user all `sockets` belong to, by uid and name. A saved grant
/// names that uid, so it must be a user the user of CLAI does not control:
/// not a uid its rootless containers run as (`/etc/subuid`), and only a uid
/// with a user name, which subordinate uids rarely have. A Flatpak may see
/// few users, so there such sockets stay run-only.
fn other_user(proc: &impl ProcSource, sockets: &[Socket]) -> Option<(u32, String)> {
    let uid = sockets[0].uid;
    let euid = proc.euid();
    if uid == euid || uid == OVERFLOW_UID || sockets.iter().any(|socket| socket.uid != uid) {
        return None;
    }
    let user = proc.user_name(euid);
    if subordinate(&proc.subuid(), user.as_deref(), euid, uid) {
        return None;
    }
    Some((uid, proc.user_name(uid)?))
}

/// Whether `/etc/subuid` gives `uid` to the user, named there by name or by
/// uid, as shadow-utils reads it.
fn subordinate(subuid: &str, user: Option<&str>, euid: u32, uid: u32) -> bool {
    let euid = euid.to_string();
    subuid
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim().split(":");
            let owner = fields.next()?;
            let start: u64 = fields.next()?.parse().ok()?;
            let count: u64 = fields.next()?.parse().ok()?;
            (Some(owner) == user || owner == euid).then_some(start..start.saturating_add(count))
        })
        .any(|range| range.contains(&u64::from(uid)))
}

fn run_only(reason: RunOnly, sockets: &[Socket], seen: &[Seen]) -> Listener {
    Listener::RunOnly {
        reason,
        sockets: inodes(sockets),
        holders: seen.iter().map(Holder::from).collect(),
    }
}

fn inodes(sockets: &[Socket]) -> Vec<u64> {
    let unique: BTreeSet<u64> = sockets.iter().map(|socket| socket.inode).collect();
    unique.into_iter().collect()
}

/// Whether `pid` is CLAI or one of its descendants.
fn is_clai(proc: &impl ProcSource, mut pid: u32, clai: u32) -> bool {
    for _ in 0..MAX_ANCESTORS {
        if pid == clai {
            return true;
        }
        match proc.parent(pid) {
            Some(parent) if parent != 0 => pid = parent,
            _ => return false,
        }
    }
    // Too deep to tell.
    true
}

/// The path a grant can store: valid UTF-8 and naming the file the process
/// runs. Once the file is unlinked the kernel appends ` (deleted)`; the bare
/// path counts only while it still names that same file.
fn program_path(proc: &impl ProcSource, holder: &Seen) -> Result<String, RunOnly> {
    let path = holder.exe.to_str().ok_or(RunOnly::PathNotUtf8)?;
    [Some(path), path.strip_suffix(DELETED)]
        .into_iter()
        .flatten()
        .find(|candidate| proc.runs(holder.pid, Path::new(candidate)))
        .map(str::to_owned)
        .ok_or(RunOnly::Replaced)
}

fn is_interpreter(exe: &str) -> bool {
    const NAMES: [&str; 18] = [
        "node", "nodejs", "bun", "deno", "java", "dotnet", "Rscript", "julia", "bash", "sh",
        "dash", "zsh", "fish", "socat", "ncat", "nc", "busybox", "toybox",
    ];
    const PREFIXES: [&str; 6] = ["python", "pypy", "ruby", "perl", "php", "lua"];
    let name = exe.rsplit("/").next().unwrap_or(exe);
    NAMES.contains(&name) || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The listening sockets in the text of `/proc/net/tcp` or `tcp6`.
fn parse_proc_net(text: &str) -> Vec<Socket> {
    text.lines().skip(1).filter_map(parse_row).collect()
}

/// `sl local_address rem_address st tx:rx tr:when retrnsmt uid timeout inode …`
fn parse_row(line: &str) -> Option<Socket> {
    let fields: Vec<&str> = line.split_whitespace().take(10).collect();
    let [_, local, _, state, _, _, _, uid, _, inode] = fields[..] else {
        return None;
    };
    if state != LISTEN {
        return None;
    }
    let (addr, port) = local.split_once(":")?;
    Some(Socket {
        addr: parse_addr(addr)?,
        port: u16::from_str_radix(port, 16).ok()?,
        uid: uid.parse().ok()?,
        inode: inode.parse().ok()?,
    })
}

/// The kernel prints an address as 32-bit words, each in host byte order.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    let mut octets = Vec::with_capacity(16);
    for start in (0..hex.len()).step_by(8) {
        let word = u32::from_str_radix(hex.get(start..start + 8)?, 16).ok()?;
        octets.extend(word.to_ne_bytes());
    }
    match octets.len() {
        4 => Some(IpAddr::from(<[u8; 4]>::try_from(octets).ok()?)),
        16 => Some(IpAddr::from(<[u8; 16]>::try_from(octets).ok()?)),
        _ => None,
    }
}

struct HostProc;

impl ProcSource for HostProc {
    fn net_tcp(&self, v6: bool) -> io::Result<String> {
        let path = if v6 {
            "/proc/self/net/tcp6"
        } else {
            "/proc/self/net/tcp"
        };
        let mut file = match fs::File::open(path) {
            // IPv6 is disabled.
            Err(error) if error.kind() == io::ErrorKind::NotFound && v6 => {
                return Ok(String::new());
            }
            result => result?,
        };
        // Most reads walk the whole kernel socket table again, so read in
        // large chunks.
        let mut text = String::with_capacity(NET_TCP_CHUNK);
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    fn self_pid(&self) -> Option<u32> {
        fs::read_link("/proc/self").ok()?.to_str()?.parse().ok()
    }

    fn euid(&self) -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn user_name(&self, uid: u32) -> Option<String> {
        let mut buf = vec![0u8; 1024];
        loop {
            // SAFETY: an all-zero passwd is valid: null pointers and zeros.
            let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
            let mut found = std::ptr::null_mut();
            // SAFETY: every pointer is valid for the call, and the buffer
            // holds buf.len() bytes.
            let error = unsafe {
                libc::getpwuid_r(
                    uid,
                    &mut entry,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    &mut found,
                )
            };
            if error == libc::ERANGE && buf.len() < MAX_PASSWD_BUF {
                buf.resize(buf.len() * 2, 0);
                continue;
            }
            if error != 0 || found.is_null() {
                return None;
            }
            // SAFETY: on success pw_name points to a C string inside buf.
            let name = unsafe { CStr::from_ptr(entry.pw_name) };
            return name.to_str().ok().map(str::to_owned);
        }
    }

    fn subuid(&self) -> String {
        fs::read_to_string("/etc/subuid").unwrap_or_default()
    }

    fn pids(&self) -> Vec<u32> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect()
    }

    fn socket_inodes(&self, pid: u32) -> Vec<u64> {
        // Fails with EACCES for the processes of other users.
        let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let target = fs::read_link(entry.path()).ok()?;
                let inode = target
                    .to_str()?
                    .strip_prefix("socket:[")?
                    .strip_suffix("]")?;
                inode.parse().ok()
            })
            .collect()
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // `pid (comm) state ppid …`, where comm may hold spaces and `)`.
        let (_, rest) = stat.rsplit_once(")")?;
        rest.split_whitespace().nth(1)?.parse().ok()
    }

    fn namespaces(&self, pid: u32) -> Option<Namespaces> {
        Some(Namespaces {
            pid: fs::read_link(format!("/proc/{pid}/ns/pid")).ok()?,
            mnt: fs::read_link(format!("/proc/{pid}/ns/mnt")).ok()?,
        })
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    fn runs(&self, pid: u32, path: &Path) -> bool {
        // Stat through the link reaches the running file even once unlinked.
        match (fs::metadata(format!("/proc/{pid}/exe")), fs::metadata(path)) {
            (Ok(running), Ok(named)) => {
                running.dev() == named.dev() && running.ino() == named.ino()
            }
            _ => false,
        }
    }

    fn cmdline(&self, pid: u32) -> String {
        let mut bytes = Vec::new();
        if let Ok(file) = fs::File::open(format!("/proc/{pid}/cmdline")) {
            // A partial read still shows what it got.
            let _ = file.take(MAX_CMDLINE).read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes)
            .replace("\0", " ")
            .trim_end()
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    const CLAI: u32 = 100;
    const EUID: u32 = 1000;
    const PORT: u16 = 5432;
    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    /// Captured on x86_64 from listeners on 127.0.0.1:5432, 0.0.0.0:6379,
    /// [::1]:5432, [::]:8080 and [::ffff:127.0.0.1]:9000, the sandbox proxy
    /// on 127.0.0.1:3128, and a connection between 127.0.0.1:7000 (also
    /// listening) and 127.0.0.1:59442.
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:18EB 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 537729 1 000000008dd3ad63 100 0 0 10 0
   1: 0100007F:0C38 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 531441 1 000000006e852d0a 100 0 0 10 0
   2: 0100007F:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 537728 1 00000000913ede42 100 0 0 10 0
   3: 0100007F:1B58 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 537733 1 00000000d758d72d 100 0 0 10 0
   4: 0100007F:1B58 0100007F:E832 01 00000000:00000000 00:00000000 00000000  1000        0 533353 1 0000000093a4b50e 20 0 0 10 -1
   5: 0100007F:E832 0100007F:1B58 01 00000000:00000000 00:00000000 00000000  1000        0 533352 2 00000000c74b2966 20 0 0 10 -1
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:1538 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 537730 1 00000000def3d090 100 0 0 10 0
   1: 0000000000000000FFFF00000100007F:2328 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 537732 1 00000000bde8ea37 100 0 0 10 0
   2: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 537731 1 000000006d7c7f4f 100 0 0 10 0
";

    #[cfg(target_endian = "little")]
    #[test]
    fn proc_net_lists_the_listening_sockets() {
        let socket = |addr: &str, port, inode| Socket {
            addr: addr.parse().unwrap(),
            port,
            uid: 1000,
            inode,
        };
        assert_eq!(
            parse_proc_net(TCP),
            [
                socket("0.0.0.0", 6379, 537729),
                socket("127.0.0.1", 3128, 531441),
                socket("127.0.0.1", 5432, 537728),
                socket("127.0.0.1", 7000, 537733),
            ]
        );
        assert_eq!(
            parse_proc_net(TCP6),
            [
                socket("::1", 5432, 537730),
                socket("::ffff:127.0.0.1", 9000, 537732),
                socket("::", 8080, 537731),
            ]
        );
    }

    #[test]
    fn a_dial_counts_the_sockets_that_accept_it() {
        for (addr, v4, v6) in [
            ("127.0.0.1", true, false),
            ("0.0.0.0", true, false),
            ("::ffff:127.0.0.1", true, false),
            ("::", true, true),
            ("::1", false, true),
            ("127.0.0.53", false, false),
            ("::ffff:127.0.0.53", false, false),
            ("192.168.1.10", false, false),
            ("fe80::1", false, false),
        ] {
            let addr: IpAddr = addr.parse().unwrap();
            assert_eq!(Loopback::V4.reaches(addr), v4, "{addr} from 127.0.0.1");
            assert_eq!(Loopback::V6.reaches(addr), v6, "{addr} from ::1");
        }
    }

    #[test]
    fn interpreters_and_relays_are_flagged() {
        for (exe, flagged) in [
            ("/usr/bin/python3.12", true),
            ("/usr/bin/node", true),
            ("/usr/bin/socat", true),
            ("/usr/bin/bash", true),
            ("/usr/bin/busybox", true),
            ("/bin/toybox", true),
            ("/usr/bin/pypy3", true),
            ("/usr/bin/ssh", false),
            ("/usr/sbin/nginx", false),
            ("/opt/python/bin/ssh", false),
        ] {
            assert_eq!(is_interpreter(exe), flagged, "{exe}");
        }
    }

    struct FakeProcess {
        parent: u32,
        sockets: Vec<u64>,
        namespaces: Namespaces,
        exe: PathBuf,
        /// The paths that name the file the process runs.
        runs: Vec<PathBuf>,
        /// Holds its sockets for the first read of its fds only.
        exits: bool,
        fd_reads: Cell<u32>,
        hides_namespaces: bool,
        hides_exe: bool,
    }

    /// CLAI (pid 100) under init, the sockets listening on [`PORT`], and
    /// the user database.
    struct Fake {
        tcp: Vec<String>,
        tcp6: Vec<String>,
        processes: BTreeMap<u32, FakeProcess>,
        users: BTreeMap<u32, String>,
        subuid: String,
    }

    fn namespaces(pid: &str, mnt: &str) -> Namespaces {
        Namespaces {
            pid: PathBuf::from(pid),
            mnt: PathBuf::from(mnt),
        }
    }

    impl Fake {
        fn new() -> Self {
            Self {
                tcp: Vec::new(),
                tcp6: Vec::new(),
                processes: BTreeMap::new(),
                users: [
                    (0, "root"),
                    (26, "postgres"),
                    (EUID, "me"),
                    (OVERFLOW_UID, "nobody"),
                ]
                .into_iter()
                .map(|(uid, name)| (uid, name.to_string()))
                .collect(),
                subuid: "me:100000:65536\n".to_string(),
            }
            .holder(1, 0, "/usr/lib/systemd/systemd", &[])
            .holder(CLAI, 1, "/usr/bin/clai", &[])
        }

        /// A socket as the kernel prints it on this machine.
        fn listening(mut self, addr: &str, port: u16, uid: u32, inode: u64) -> Self {
            let octets = match addr.parse::<IpAddr>().unwrap() {
                IpAddr::V4(ip) => ip.octets().to_vec(),
                IpAddr::V6(ip) => ip.octets().to_vec(),
            };
            let hex: String = octets
                .chunks(4)
                .map(|word| format!("{:08X}", u32::from_ne_bytes(word.try_into().unwrap())))
                .collect();
            let rows = if octets.len() == 16 {
                &mut self.tcp6
            } else {
                &mut self.tcp
            };
            rows.push(format!(
                "   0: {hex}:{port:04X} {hex}:0000 0A 00000000:00000000 00:00000000 00000000 {uid:5} 0 {inode} 1"
            ));
            self
        }

        fn holder(mut self, pid: u32, parent: u32, exe: &str, sockets: &[u64]) -> Self {
            self.processes.insert(
                pid,
                FakeProcess {
                    parent,
                    sockets: sockets.to_vec(),
                    namespaces: namespaces("pid:[1]", "mnt:[1]"),
                    exe: PathBuf::from(exe),
                    runs: vec![PathBuf::from(exe)],
                    exits: false,
                    fd_reads: Cell::new(0),
                    hides_namespaces: false,
                    hides_exe: false,
                },
            );
            self
        }

        fn with(mut self, pid: u32, change: impl FnOnce(&mut FakeProcess)) -> Self {
            change(self.processes.get_mut(&pid).unwrap());
            self
        }

        fn user(mut self, uid: u32, name: &str) -> Self {
            self.users.insert(uid, name.to_string());
            self
        }

        fn with_subuid(mut self, text: &str) -> Self {
            self.subuid = text.to_string();
            self
        }
    }

    impl ProcSource for Fake {
        fn net_tcp(&self, v6: bool) -> io::Result<String> {
            let rows = if v6 { &self.tcp6 } else { &self.tcp };
            Ok(std::iter::once(HEADER)
                .chain(rows.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join("\n"))
        }

        fn self_pid(&self) -> Option<u32> {
            Some(CLAI)
        }

        fn euid(&self) -> u32 {
            EUID
        }

        fn user_name(&self, uid: u32) -> Option<String> {
            self.users.get(&uid).cloned()
        }

        fn subuid(&self) -> String {
            self.subuid.clone()
        }

        fn pids(&self) -> Vec<u32> {
            // Unsorted, as /proc may list them.
            self.processes.keys().rev().copied().collect()
        }

        fn socket_inodes(&self, pid: u32) -> Vec<u64> {
            let process = &self.processes[&pid];
            process.fd_reads.set(process.fd_reads.get() + 1);
            if process.exits && process.fd_reads.get() > 1 {
                return Vec::new();
            }
            process.sockets.clone()
        }

        fn parent(&self, pid: u32) -> Option<u32> {
            Some(self.processes.get(&pid)?.parent)
        }

        fn namespaces(&self, pid: u32) -> Option<Namespaces> {
            let process = self.processes.get(&pid)?;
            (!process.hides_namespaces).then(|| process.namespaces.clone())
        }

        fn exe(&self, pid: u32) -> Option<PathBuf> {
            let process = self.processes.get(&pid)?;
            (!process.hides_exe).then(|| process.exe.clone())
        }

        fn runs(&self, pid: u32, path: &Path) -> bool {
            self.processes[&pid].runs.iter().any(|named| named == path)
        }

        fn cmdline(&self, pid: u32) -> String {
            format!("{} -N", self.processes[&pid].exe.display())
        }
    }

    fn program(exe: &str, pid: u32, cmdline: &str) -> Listener {
        Listener::Program(Program {
            exe: exe.to_string(),
            pid,
            cmdline: cmdline.to_string(),
            interpreter: is_interpreter(exe),
        })
    }

    fn run_only(reason: RunOnly, sockets: &[u64], holders: &[(u32, &str)]) -> Listener {
        Listener::RunOnly {
            reason,
            sockets: sockets.to_vec(),
            holders: holders
                .iter()
                .map(|&(pid, exe)| Holder {
                    pid,
                    exe: exe.to_string(),
                    cmdline: format!("{exe} -N"),
                })
                .collect(),
        }
    }

    const SSH: &str = "/usr/bin/ssh";
    const PYTHON: &str = "/usr/bin/python3.12";

    /// A socket on 127.0.0.1:5432 of this user, inode 11.
    fn tunnel() -> Fake {
        Fake::new().listening("127.0.0.1", PORT, EUID, 11)
    }

    /// The tunnel held by a process `depth` processes deep below init.
    fn nested(depth: u32) -> Fake {
        let parent = |pid: u32| if pid == 1000 { 1 } else { pid - 1 };
        let holder = 1000 + depth - 1;
        (1000..holder)
            .fold(tunnel(), |fake, pid| {
                fake.holder(pid, parent(pid), "/usr/bin/sh", &[])
            })
            .holder(holder, parent(holder), SSH, &[11])
    }

    fn assert_cases<const N: usize>(cases: [(&str, Fake, Listener); N]) {
        for (name, fake, expected) in cases {
            assert_eq!(
                lookup(&fake, PORT, Loopback::V4).unwrap(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn the_program_behind_the_dialled_sockets_is_found() {
        assert_cases([
            ("nothing listens", Fake::new(), Listener::Nothing),
            (
                "only other ports and addresses",
                Fake::new()
                    .listening("127.0.0.1", PORT + 1, EUID, 11)
                    .listening("127.0.0.53", PORT, EUID, 12)
                    .listening("::1", PORT, EUID, 13)
                    .holder(300, 1, SSH, &[11, 12, 13]),
                Listener::Nothing,
            ),
            (
                "a program",
                tunnel().holder(300, 1, SSH, &[11]),
                program(SSH, 300, "/usr/bin/ssh -N"),
            ),
            (
                "an interpreter on the IPv4 and IPv6 wildcards",
                Fake::new()
                    .listening("0.0.0.0", PORT, EUID, 11)
                    .listening("::", PORT, EUID, 12)
                    .holder(300, 1, PYTHON, &[11, 12]),
                program(PYTHON, 300, "/usr/bin/python3.12 -N"),
            ),
            (
                "a forking server, lowest pid shown",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .holder(301, 300, SSH, &[11]),
                program(SSH, 300, "/usr/bin/ssh -N"),
            ),
            (
                "a program deep below init",
                nested(63),
                program(SSH, 1062, "/usr/bin/ssh -N"),
            ),
        ]);
    }

    #[test]
    fn clai_and_its_descendants_are_never_programs() {
        assert_cases([
            (
                "CLAI itself",
                tunnel().with(CLAI, |clai| clai.sockets = vec![11]),
                Listener::Clai,
            ),
            (
                "a CLAI descendant",
                tunnel()
                    .holder(200, CLAI, "/usr/bin/bwrap", &[])
                    .holder(201, 200, PYTHON, &[11]),
                Listener::Clai,
            ),
            (
                "CLAI wins over another namespace",
                tunnel()
                    .holder(200, CLAI, PYTHON, &[11])
                    .holder(300, 1, SSH, &[11])
                    .with(300, |p| p.namespaces = namespaces("pid:[2]", "mnt:[2]")),
                Listener::Clai,
            ),
            (
                "CLAI wins over a holder it cannot read",
                tunnel()
                    .holder(150, 1, SSH, &[11])
                    .with(150, |p| p.hides_exe = true)
                    .holder(200, CLAI, PYTHON, &[11]),
                Listener::Clai,
            ),
            (
                "a holder too deep below init to tell",
                nested(64),
                Listener::Clai,
            ),
        ]);
    }

    #[test]
    fn a_holder_in_another_namespace_is_run_only() {
        assert_cases([
            (
                "another pid namespace",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .with(300, |p| p.namespaces = namespaces("pid:[2]", "mnt:[1]")),
                run_only(RunOnly::OtherNamespace, &[11], &[(300, SSH)]),
            ),
            (
                "another mount namespace only",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .with(300, |p| p.namespaces = namespaces("pid:[1]", "mnt:[2]")),
                run_only(RunOnly::OtherNamespace, &[11], &[(300, SSH)]),
            ),
        ]);
    }

    #[test]
    fn sockets_without_one_visible_program_are_never_programs() {
        assert_cases([
            (
                "two programs",
                Fake::new()
                    .listening("127.0.0.1", PORT, EUID, 11)
                    .listening("::ffff:127.0.0.1", PORT, EUID, 12)
                    .holder(300, 1, SSH, &[11])
                    .holder(301, 1, PYTHON, &[12]),
                run_only(
                    RunOnly::SeveralPrograms,
                    &[11, 12],
                    &[(300, SSH), (301, PYTHON)],
                ),
            ),
            (
                "own user, no process visible",
                tunnel(),
                run_only(RunOnly::Unseen, &[11], &[]),
            ),
            (
                "another user, no process visible",
                Fake::new().listening("127.0.0.1", PORT, 26, 11),
                Listener::OtherUser {
                    uid: 26,
                    name: "postgres".to_string(),
                    sockets: vec![11],
                },
            ),
            (
                "two other users",
                Fake::new()
                    .listening("127.0.0.1", PORT, 26, 11)
                    .listening("::", PORT, 0, 12),
                run_only(RunOnly::Unseen, &[11, 12], &[]),
            ),
            (
                "a program and an unseen socket of another user",
                tunnel()
                    .listening("::", PORT, 26, 12)
                    .holder(300, 1, SSH, &[11]),
                run_only(RunOnly::Unseen, &[11, 12], &[(300, SSH)]),
            ),
            (
                "a holder whose pid is reused while it is read",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .with(300, |p| p.exits = true),
                run_only(RunOnly::Unseen, &[11], &[]),
            ),
            (
                "a holder whose namespaces cannot be read, beside a program",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .holder(301, 300, SSH, &[11])
                    .with(301, |p| p.hides_namespaces = true),
                run_only(RunOnly::Unseen, &[11], &[(300, SSH)]),
            ),
            (
                "a holder whose program cannot be read, beside a program",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .holder(301, 300, SSH, &[11])
                    .with(301, |p| p.hides_exe = true),
                run_only(RunOnly::Unseen, &[11], &[(300, SSH)]),
            ),
        ]);
    }

    #[test]
    fn only_a_named_user_outside_the_control_of_clai_is_another_user() {
        let other = |uid, name: &str| Listener::OtherUser {
            uid,
            name: name.to_string(),
            sockets: vec![11],
        };
        let unseen = || run_only(RunOnly::Unseen, &[11], &[]);
        let socket_of = |uid| Fake::new().listening("127.0.0.1", PORT, uid, 11);
        assert_cases([
            ("a named user", socket_of(26), other(26, "postgres")),
            (
                "a uid the user runs containers as",
                socket_of(100_998).user(100_998, "container"),
                unseen(),
            ),
            (
                "the first uid the user runs containers as",
                socket_of(100_000).user(100_000, "container"),
                unseen(),
            ),
            (
                "the uid after those the user runs containers as",
                socket_of(165_536).user(165_536, "service"),
                other(165_536, "service"),
            ),
            ("a uid without a user name", socket_of(27), unseen()),
            (
                "a uid without a user name, and no subordinate uids",
                socket_of(100_998).with_subuid(""),
                unseen(),
            ),
            (
                "subordinate uids listed by the uid of the user",
                socket_of(200_005)
                    .user(200_005, "container")
                    .with_subuid("1000:200000:65536\n"),
                unseen(),
            ),
            (
                "subordinate uids of other users only",
                socket_of(100_998)
                    .user(100_998, "service")
                    .with_subuid("postgres:100000:65536\n2000:100000:65536\nme:300000:65536\n"),
                other(100_998, "service"),
            ),
            (
                "the uid of every user outside the user namespace of CLAI",
                socket_of(OVERFLOW_UID),
                unseen(),
            ),
        ]);
    }

    #[test]
    fn a_program_path_must_be_utf8_and_name_the_running_file() {
        let deleted = "/usr/bin/ssh (deleted)";
        assert_cases([
            (
                "a program whose file was replaced",
                tunnel()
                    .holder(300, 1, deleted, &[11])
                    .with(300, |p| p.runs.clear()),
                run_only(RunOnly::Replaced, &[11], &[(300, deleted)]),
            ),
            (
                "a deleted path that still names the running file",
                tunnel()
                    .holder(300, 1, deleted, &[11])
                    .with(300, |p| p.runs = vec![PathBuf::from(SSH)]),
                program(SSH, 300, "/usr/bin/ssh (deleted) -N"),
            ),
            (
                "a file whose name ends like a deleted one",
                tunnel().holder(300, 1, "/opt/x (deleted)", &[11]),
                program("/opt/x (deleted)", 300, "/opt/x (deleted) -N"),
            ),
            (
                "a path that names another file",
                tunnel()
                    .holder(300, 1, SSH, &[11])
                    .with(300, |p| p.runs.clear()),
                run_only(RunOnly::Replaced, &[11], &[(300, SSH)]),
            ),
            (
                "a path that is not UTF-8",
                tunnel().holder(300, 1, SSH, &[11]).with(300, |p| {
                    p.exe = PathBuf::from(OsStr::from_bytes(b"/opt/\xff/ssh"));
                    p.runs = vec![p.exe.clone()];
                }),
                Listener::RunOnly {
                    reason: RunOnly::PathNotUtf8,
                    sockets: vec![11],
                    holders: vec![Holder {
                        pid: 300,
                        exe: "/opt/\u{fffd}/ssh".to_string(),
                        cmdline: "/opt/\u{fffd}/ssh -N".to_string(),
                    }],
                },
            ),
        ]);
    }

    #[test]
    fn an_ipv6_dial_counts_only_ipv6_sockets() {
        let fake = Fake::new()
            .listening("127.0.0.1", PORT, EUID, 11)
            .listening("::1", PORT, EUID, 12)
            .holder(300, 1, "/usr/bin/ssh", &[11])
            .holder(301, 1, "/usr/bin/python3", &[12]);
        assert_eq!(
            lookup(&fake, PORT, Loopback::V6).unwrap(),
            program("/usr/bin/python3", 301, "/usr/bin/python3 -N")
        );
    }

    fn listen() -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[test]
    fn clai_and_the_processes_it_starts_are_never_programs() {
        let (listener, port) = listen();
        assert_eq!(inspect(port, Loopback::V4).unwrap(), Listener::Clai);
        // Only the child holds the socket once the command is dropped.
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::from(OwnedFd::from(listener)))
            .spawn()
            .unwrap();
        let listener = inspect(port, Loopback::V4);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(listener.unwrap(), Listener::Clai);
    }

    /// The shell exits once `sleep` starts, so `sleep` is re-parented away
    /// from this process, as a tunnel the user started would be.
    #[test]
    fn a_listener_started_apart_from_clai_is_its_program() {
        let (listener, port) = listen();
        let output = Command::new("sh")
            .args(["-c", "sleep 30 2>/dev/null & echo $! >&2"])
            .stdout(Stdio::from(OwnedFd::from(listener)))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8(output.stderr)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap() != "sleep\x0030\x00" {
            assert!(Instant::now() < deadline, "sleep did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        let listener = inspect(port, Loopback::V4);
        let exe = fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        Command::new("kill").arg(pid.to_string()).status().unwrap();
        let Ok(Listener::Program(program)) = listener else {
            panic!("{listener:?}");
        };
        assert_eq!(program.pid, pid);
        assert_eq!(program.cmdline, "sleep 30");
        assert!(!program.interpreter);
        assert_eq!(Path::new(&program.exe), exe);
    }

    #[test]
    fn the_host_names_this_process_and_its_users() {
        assert_eq!(HostProc.self_pid(), Some(std::process::id()));
        assert_eq!(HostProc.user_name(0).as_deref(), Some("root"));
        assert_eq!(HostProc.user_name(u32::MAX), None);
    }
}
