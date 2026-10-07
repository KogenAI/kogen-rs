use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[test]
fn chatty_child_times_out_and_keeps_bounded_tail() {
    let root = test_dir("chatty");
    let mut request = ProcessRequest::new("/bin/sh", &root, &root);
    request.args = vec![
        "-c".into(),
        "while :; do printf '0123456789abcdef'; done".into(),
    ];
    request.timeout = Duration::from_secs(1);
    request.log_name = "chatty".to_owned();

    let started = Instant::now();
    let result = ProcessSupervisor.run(request).expect("process result");
    assert!(result.timed_out);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(result.output_tail.len() <= OUTPUT_TAIL_BYTES);
    assert!(fs::metadata(&result.log_path).expect("log exists").len() > OUTPUT_TAIL_BYTES as u64);
    assert_eq!(
        fs::metadata(&result.log_path)
            .expect("log metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn term_trapper_gets_grace_period_then_is_killed() {
    let root = test_dir("term-trap");
    let mut request = ProcessRequest::new("/bin/sh", &root, &root);
    request.args = vec![
        "-c".into(),
        "trap 'printf term-seen; while :; do :; done' TERM; while :; do sleep 1; done".into(),
    ];
    request.env = BTreeMap::from([("PATH".into(), "/bin:/usr/bin".into())]);
    request.timeout = Duration::from_millis(100);
    request.log_name = "term-trap".to_owned();

    let result = ProcessSupervisor.run(request).expect("process result");
    assert!(result.timed_out);
    assert!(
        result
            .output_tail
            .windows(9)
            .any(|part| part == b"term-seen")
    );
    assert_eq!(result.exit_status, Some(137));
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn finished_parent_stops_background_grandchild() {
    let root = test_dir("grandchild");
    let pid_file = root.join("grandchild.pid");
    let mut request = ProcessRequest::new("/bin/sh", &root, &root);
    request.args = vec!["-c".into(), "sleep 60 & echo $! > \"$PID_FILE\"".into()];
    request.env = BTreeMap::from([
        ("PATH".into(), "/bin:/usr/bin".into()),
        ("PID_FILE".into(), pid_file.as_os_str().to_owned()),
    ]);
    request.timeout = Duration::from_secs(5);

    let result = ProcessSupervisor.run(request).expect("process result");
    assert_eq!(result.exit_status, Some(0));
    let pid = fs::read_to_string(pid_file).expect("grandchild pid");
    assert_process_stops(pid.trim().parse().expect("pid number"));
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn parent_death_watcher_stops_child_group_after_sigkill() {
    const WORKER_MARKER: &str = "KOGEN_PROCESS_TEST_WORKER_MARKER";
    if let Some(marker) = std::env::var_os(WORKER_MARKER) {
        let marker = std::path::PathBuf::from(marker);
        let root = marker.parent().expect("worker run dir");
        let mut request = ProcessRequest::new("/bin/sh", root, root);
        request.args = vec![
            "-c".into(),
            "echo $$ > \"$PID_FILE\"; sleep 0.5; exec sleep 60".into(),
        ];
        request.env = BTreeMap::from([
            ("PATH".into(), "/bin:/usr/bin".into()),
            ("PID_FILE".into(), marker.as_os_str().to_owned()),
        ]);
        request.timeout = Duration::from_secs(30);
        let _ = ProcessSupervisor.run(request);
        panic!("worker unexpectedly outlived its test parent");
    }

    let root = test_dir("parent-death");
    let marker = root.join("target.pid");
    let mut worker = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "run::process::tests::parent_death_watcher_stops_child_group_after_sigkill",
            "--nocapture",
        ])
        .env(WORKER_MARKER, &marker)
        .spawn()
        .expect("spawn worker");
    wait_for_file(&marker);
    thread::sleep(Duration::from_millis(150));
    let target = fs::read_to_string(&marker).expect("target pid");
    worker.kill().expect("kill worker");
    let _ = worker.wait();
    assert_process_stops(target.trim().parse().expect("pid number"));
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn missing_executable_is_reported_as_unavailable_127() {
    let root = test_dir("missing");
    let result = ProcessSupervisor
        .run(ProcessRequest::new(
            "/does/not/exist/kogen-test",
            &root,
            &root,
        ))
        .expect("process result");
    assert_eq!(result.exit_status, Some(127));
    assert!(result.unavailable);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn process_log_read_rejects_a_child_replaced_log_symlink() {
    let root = test_dir("log-symlink");
    let run_dir = root.join("run");
    let logs = run_dir.join("logs");
    fs::create_dir_all(&logs).expect("create child-writable logs directory");
    let outside = root.join("outside");
    fs::write(&outside, b"must not be read as process output").unwrap();

    let mut request = ProcessRequest::new("/bin/sh", &root, &run_dir);
    request.log_name = "replace-log".to_owned();
    request.args = vec![
        "-c".into(),
        format!(
            "for f in '{}/replace-log-{}-'*.log; do /bin/rm -f \"$f\"; /bin/ln -s '{}' \"$f\"; done",
            logs.display(),
            std::process::id(),
            outside.display()
        )
        .into(),
    ];
    let result = ProcessSupervisor.run(request);
    assert!(
        result.is_err(),
        "controller must not read a child-created symlink"
    );
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"must not be read as process output"
    );
    let _ = fs::remove_dir_all(root);
}

fn test_dir(label: &str) -> std::path::PathBuf {
    static NEXT_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-process-{label}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

#[cfg(unix)]
fn assert_process_stops(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let result = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if result.is_err() || result.is_ok_and(|status| !status.success()) || is_zombie(pid) {
            return;
        }
        assert!(Instant::now() < deadline, "process {pid} survived cleanup");
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn is_zombie(pid: u32) -> bool {
    let output = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    output.is_ok_and(|output| {
        let state = String::from_utf8_lossy(&output.stdout);
        state.trim().starts_with('Z') || state.trim().is_empty()
    })
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "worker did not create pid marker"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
