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
