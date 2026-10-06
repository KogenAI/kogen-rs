use super::*;
use std::ffi::OsString;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug)]
struct ScriptObservation {
    program: OsString,
    args: Vec<OsString>,
    script: Vec<u8>,
    mode: u32,
    stdin_is_null: bool,
}

struct RecordingRunner(Mutex<Option<ScriptObservation>>);

impl ProcessPort for RecordingRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        let path = PathBuf::from(&request.args[0]);
        let metadata = fs::metadata(&path).expect("private script metadata");
        let observation = ScriptObservation {
            program: request.program,
            args: request.args,
            script: fs::read(path).expect("private script bytes"),
            mode: metadata.permissions().mode() & 0o777,
            stdin_is_null: matches!(request.stdin, StdinSource::Null),
        };
        *self.0.lock().expect("observation mutex") = Some(observation);
        Ok(ProcessResult {
            exit_status: Some(0),
            timed_out: false,
            unavailable: false,
            output_tail: b"ok".to_vec(),
            log_path: request.run_dir.join("logs/shell.log"),
            duration_ms: 1,
            sandbox: None,
        })
    }
}

#[cfg(unix)]
#[test]
fn large_shell_source_is_transported_through_mode_0600_file() {
    let root = std::env::temp_dir().join(format!("kogen-script-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create temp dir");
    let mut script = vec![b'#'; 300 * 1024];
    script.extend_from_slice(b"\nprintf ok\n");
    let runner = RecordingRunner(Mutex::new(None));
    let result = run_private_script(&runner, &script, &root, &root, ChildEnvironment::new())
        .expect("run shell script");
    assert_eq!(result.output_tail, b"ok");

    let observed = runner.0.lock().unwrap().take().expect("observed request");
    assert_eq!(observed.program, "sh");
    assert_eq!(observed.args.len(), 1);
    assert!(observed.args[0].len() < 4096);
    assert_eq!(observed.script, script);
    assert_eq!(observed.mode, 0o600);
    assert!(observed.stdin_is_null);
    let _ = fs::remove_dir_all(root);
}
