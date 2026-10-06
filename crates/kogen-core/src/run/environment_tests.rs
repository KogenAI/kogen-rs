use super::*;
use crate::run::ProcessResult;
use std::fs::{self, OpenOptions};
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

struct MiseStubRunner {
    requests: Mutex<Vec<ProcessRequest>>,
}

impl ProcessPort for MiseStubRunner {
    fn run(&self, request: ProcessRequest) -> Result<ProcessResult, ProcessError> {
        let log_path = request.run_dir.join("logs").join("stub-mise.log");
        fs::create_dir_all(log_path.parent().expect("log parent")).expect("create log dir");
        fs::write(
            &log_path,
            br#"{"MISE_VALUE":"from-mise","PATH":"/mise/env/bin","MISE_STATE_DIR":"/outside/state","MISE_CACHE_DIR":"/outside/cache","MISE_TRUSTED_CONFIG_PATHS":"/mise/trusted"}"#,
        )
        .expect("write fake mise output");
        self.requests.lock().expect("request mutex").push(request);
        Ok(ProcessResult {
            exit_status: Some(0),
            timed_out: false,
            unavailable: false,
            output_tail: Vec::new(),
            log_path,
            duration_ms: 1,
            sandbox: None,
        })
    }
}

#[cfg(unix)]
#[test]
fn filters_host_values_and_merges_mise_before_project_values() {
    let root = test_dir();
    let bin = root.join("bin");
    let runtime = root.join("kogen-bin");
    fs::create_dir_all(&bin).expect("create bin");
    fs::create_dir_all(&runtime).expect("create runtime path");
    let mise = bin.join("mise");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&mise)
        .expect("create mise stub");

    let original_path = std::env::join_paths([&bin, &runtime]).expect("join PATH");
    let base = EnvironmentMap::from([
        ("PATH".into(), original_path),
        ("HOME".into(), "/host/home".into()),
        ("HTTP_PROXY".into(), "http://proxy.invalid".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/host/gitconfig".into()),
        (
            "MISE_TRUSTED_CONFIG_PATHS".into(),
            "/existing/trusted".into(),
        ),
        ("MIX_HOME".into(), "/host/mix".into()),
        ("KOGEN_SECRET".into(), "never-pass".into()),
    ]);
    let mut request = EnvironmentRequest::new(base, root.join("run"), &root, &root);
    request.stack_home_keys.insert("MIX_HOME".to_owned());
    request.kogen_runtime_paths.push(runtime.clone());
    request
        .project
        .insert("MISE_VALUE".to_owned(), "from-project".to_owned());

    let runner = MiseStubRunner {
        requests: Mutex::new(Vec::new()),
    };
    let env = build_child_environment(&runner, request).expect("child env");
    assert_eq!(env.get(OsStr::new("KOGEN_SECRET")), None);
    assert_eq!(
        env.get(OsStr::new("HTTP_PROXY")).unwrap(),
        "http://proxy.invalid"
    );
    assert_eq!(
        env.get(OsStr::new("GIT_CONFIG_GLOBAL")).unwrap(),
        "/host/gitconfig"
    );
    assert_eq!(env.get(OsStr::new("MIX_HOME")).unwrap(), "/host/mix");
    assert_eq!(env.get(OsStr::new("MISE_VALUE")).unwrap(), "from-project");
    assert_eq!(
        env.get(OsStr::new("MISE_STATE_DIR")).unwrap(),
        root.join("run/mise-state").as_os_str()
    );
    assert_eq!(
        env.get(OsStr::new("MISE_CACHE_DIR")).unwrap(),
        root.join("run/mise-cache").as_os_str()
    );
    assert_eq!(
        env.get(OsStr::new("TMPDIR")).unwrap(),
        root.join("run/tmp").as_os_str()
    );
    let child_paths =
        std::env::split_paths(env.get(OsStr::new("PATH")).unwrap()).collect::<Vec<_>>();
    assert_eq!(child_paths.first(), Some(&bin));
    assert!(!child_paths.contains(&runtime));
    let trusted = std::env::split_paths(env.get(OsStr::new("MISE_TRUSTED_CONFIG_PATHS")).unwrap())
        .collect::<Vec<_>>();
    assert!(trusted.contains(&PathBuf::from("/existing/trusted")));
    assert!(trusted.contains(&PathBuf::from("/mise/trusted")));
    assert!(trusted.contains(&root));
    assert_eq!(runner.requests.lock().unwrap().len(), 1);

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn project_path_is_used_verbatim_after_mise() {
    let root = test_dir();
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("create bin");
    let mise = bin.join("mise");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(mise)
        .expect("create mise stub");
    let mut request = EnvironmentRequest::new(
        EnvironmentMap::from([("PATH".into(), bin.as_os_str().to_owned())]),
        root.join("run"),
        &root,
        &root,
    );
    request
        .project
        .insert("PATH".to_owned(), "/project/path".to_owned());
    let runner = MiseStubRunner {
        requests: Mutex::new(Vec::new()),
    };
    let env = build_child_environment(&runner, request).expect("child env");
    assert_eq!(env.get(OsStr::new("PATH")).unwrap(), "/project/path");
    let _ = fs::remove_dir_all(root);
}

fn test_dir() -> PathBuf {
    static NEXT_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "kogen-environment-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}
