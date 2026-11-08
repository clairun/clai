//! Runs the built sandbox helper the way the Linux sandbox does, outside
//! bwrap: a test Unix socket stands in for CLAI's network proxy.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_clai-sandbox-helper");

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn read_head(stream: &mut UnixStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

/// A stand-in for the proxy: answers one CONNECT with `answer` and, on a
/// 200, echoes the tunnel until the client stops sending.
fn proxy_socket(dir: &Path, answer: &'static str) -> (String, thread::JoinHandle<String>) {
    let path = dir.join("net.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let head = read_head(&mut stream);
        stream.write_all(answer.as_bytes()).unwrap();
        if answer.contains(" 200 ") {
            let mut reader = stream.try_clone().unwrap();
            std::io::copy(&mut reader, &mut stream).unwrap();
        }
        head
    });
    (path.display().to_string(), handle)
}

/// A stand-in for the proxy that answers every CONNECT with `answer` and, on
/// a 200, echoes the tunnel; each request head is sent to the receiver.
fn proxy_for_every_connection(
    dir: &Path,
    answer: &'static str,
) -> (String, std::sync::mpsc::Receiver<String>) {
    let path = dir.join("net.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (sender, heads) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let sender = sender.clone();
            thread::spawn(move || {
                let _ = sender.send(read_head(&mut stream));
                stream.write_all(answer.as_bytes()).unwrap();
                if answer.contains(" 200 ") {
                    let mut reader = stream.try_clone().unwrap();
                    let _ = std::io::copy(&mut reader, &mut stream);
                }
            });
        }
    });
    (path.display().to_string(), heads)
}

/// Connects to `port` on 127.0.0.1 and on ::1, sends `ping` and prints what
/// comes back: the echo, `closed`, or `refused` when nothing listens.
const LOOPBACK_CLIENT: &str = "
import socket, sys
port = int(sys.argv[1])
for host in ('127.0.0.1', '::1'):
    try:
        with socket.create_connection((host, port), 5) as s:
            s.sendall(b'ping')
            print(host, s.recv(16).decode() or 'closed')
    except ConnectionRefusedError:
        print(host, 'refused')
    except ConnectionResetError:
        print(host, 'closed')
";

/// `bridge` forwarding `port`, around the loopback client for `port`.
fn forwarding_bridge(socket: &str, port: u16) -> Output {
    let port = port.to_string();
    Command::new(HELPER)
        .args([
            "bridge",
            "--socket",
            socket,
            "--http",
            &free_port().to_string(),
        ])
        .args([
            "--forward",
            &port,
            "--",
            "python3",
            "-c",
            LOOPBACK_CLIENT,
            &port,
        ])
        .output()
        .unwrap()
}

/// `bridge` around the helper's own `connect`, as ssh would run it.
fn bridged_connect(socket: &str, host: &str, stdin: &[u8]) -> Output {
    let port = free_port().to_string();
    let proxy = format!("127.0.0.1:{port}");
    let mut child = Command::new(HELPER)
        .args(["bridge", "--socket", socket, "--http", &port, "--"])
        .args([HELPER, "connect", "--proxy", &proxy, host, "22"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn bridge_status(script: &str) -> std::process::ExitStatus {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("unused.sock");
    Command::new(HELPER)
        .arg("bridge")
        .arg("--socket")
        .arg(&socket)
        .args(["--http", &free_port().to_string(), "--", "sh", "-c", script])
        .status()
        .unwrap()
}

#[test]
fn ssh_traffic_reaches_the_proxy_through_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, proxy) = proxy_socket(dir.path(), "HTTP/1.1 200 Connection established\r\n\r\n");
    let output = bridged_connect(&socket, "github.com", b"SSH-2.0-test\r\n");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"SSH-2.0-test\r\n");
    assert_eq!(
        proxy.join().unwrap(),
        "CONNECT github.com:22 HTTP/1.1\r\nHost: github.com:22\r\n\r\n"
    );
}

/// ssh keeps its end open and waits for each reply, so every byte it
/// writes must reach the server at once, not when more data follows.
#[test]
fn bytes_written_while_the_tunnel_stays_open_arrive_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _proxy) = proxy_socket(dir.path(), "HTTP/1.1 200 Connection established\r\n\r\n");
    let port = free_port().to_string();
    let proxy = format!("127.0.0.1:{port}");
    let mut child = Command::new(HELPER)
        .args(["bridge", "--socket", &socket, "--http", &port, "--"])
        .args([HELPER, "connect", "--proxy", &proxy, "github.com", "22"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // The size of an ssh KEXINIT: more than one TCP segment on loopback is
    // not needed, a tail that is not a full segment is.
    let message = vec![b'k'; 1712];
    stdin.write_all(&message).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut echoed = vec![0_u8; 1712];
        let _ = sender.send(stdout.read_exact(&mut echoed).map(|()| echoed));
    });
    let echoed = receiver.recv_timeout(Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);
    assert_eq!(
        echoed.expect("the bytes are still held back").unwrap(),
        message
    );
}

#[test]
fn a_refused_tunnel_fails_like_an_unreachable_ssh_host() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _proxy) = proxy_socket(
        dir.path(),
        "HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n`example.org` is not on this agent's network allowlist.\n",
    );
    let output = bridged_connect(&socket, "example.org", b"");
    assert_eq!(output.status.code(), Some(255), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("403 Forbidden"), "{stderr}");
    assert!(stderr.contains("`example.org` is not on"), "{stderr}");
}

#[test]
fn the_bridge_exits_as_its_command_does() {
    assert_eq!(bridge_status("exit 7").code(), Some(7));
    assert_eq!(bridge_status("kill -9 $$").signal(), Some(9));
    assert_eq!(bridge_status("kill -TERM $$").signal(), Some(15));
}

/// The runner kills a timed-out command's process group; the helper and the
/// command it runs are in that group, so neither survives.
#[test]
fn killing_the_process_group_leaves_no_command_behind() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(HELPER)
        .arg("bridge")
        .arg("--socket")
        .arg(dir.path().join("unused.sock"))
        .args([
            "--http",
            &free_port().to_string(),
            "--",
            "sh",
            "-c",
            "echo $$; exec sleep 60",
        ])
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut stdout = child.stdout.take().unwrap();
    let mut byte = [0_u8; 1];
    while !line.ends_with('\n') && stdout.read(&mut byte).unwrap() == 1 {
        line.push(char::from(byte[0]));
    }
    let command_pid: i32 = line.trim().parse().unwrap();

    // SAFETY: kill(2) on our own child's process group; no memory effects.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    assert_eq!(child.wait().unwrap().signal(), Some(9));
    let gone = (0..50).any(|_| {
        let state = std::fs::read_to_string(format!("/proc/{command_pid}/stat"));
        let gone = state.map_or(true, |stat| stat.contains(") Z "));
        if !gone {
            thread::sleep(Duration::from_millis(20));
        }
        gone
    });
    assert!(gone, "the command outlived the helper");
}

/// Each connection to a forwarded port, on either loopback, is a CONNECT to
/// that port of this computer through the proxy socket.
#[test]
fn a_forwarded_port_answers_on_both_loopbacks_through_the_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, heads) =
        proxy_for_every_connection(dir.path(), "HTTP/1.1 200 Connection established\r\n\r\n");
    let port = free_port();
    let output = forwarding_bridge(&socket, port);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "127.0.0.1 ping\n::1 ping\n",
        "{output:?}"
    );
    let connect = format!("CONNECT localhost:{port} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n");
    for _ in 0..2 {
        assert_eq!(heads.recv_timeout(Duration::from_secs(5)).unwrap(), connect);
    }
}

/// A refusal closes the connection, and the helper prints the reason.
#[test]
fn a_refused_forward_closes_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _heads) = proxy_for_every_connection(
        dir.path(),
        "HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\nPort 5432 on this computer is now served by `/usr/bin/nc`.\n",
    );
    let output = forwarding_bridge(&socket, free_port());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "127.0.0.1 closed\n::1 closed\n",
        "{output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("403 Forbidden"), "{stderr}");
    assert!(stderr.contains("now served by `/usr/bin/nc`"), "{stderr}");
}

#[test]
fn a_port_that_cannot_be_forwarded_still_runs_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let output = Command::new(HELPER)
        .arg("bridge")
        .arg("--socket")
        .arg(dir.path().join("unused.sock"))
        .args(["--http", &free_port().to_string(), "--forward", &port])
        .args(["--", "sh", "-c", "echo ran"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"ran\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "cannot forward localhost:{port} at 127.0.0.1:{port}"
        )),
        "{stderr}"
    );
}

/// The proxy's own port is not forwarded on either loopback: 127.0.0.1 stays
/// the proxy and nothing listens on ::1.
#[test]
fn the_proxy_port_is_not_forwarded() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port().to_string();
    let output = Command::new(HELPER)
        .arg("bridge")
        .arg("--socket")
        .arg(dir.path().join("unused.sock"))
        .args(["--http", &port, "--forward", &port])
        .args(["--", "python3", "-c", LOOPBACK_CLIENT, &port])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "127.0.0.1 closed\n::1 refused\n",
        "{output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "localhost:{port} is CLAI's proxy port inside the sandbox and is not forwarded"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("cannot forward"), "{stderr}");
}
