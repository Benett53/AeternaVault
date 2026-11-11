//! Runs the real `aeternavault-cli` binary against temporary folders.
//!
//! `AETERNAVAULT_HOME` keeps configuration, history and the remembered key in
//! the test folder, and `AETERNAVAULT_PROFILE_ROOT` switches on demo mode, so
//! nothing on the computer is changed.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir_all(root.join("profile")).unwrap();
        let env = Env { _tmp: tmp, root };
        env.ok(&["destination", "set", env.path("dest").to_str().unwrap()]);
        env
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aeternavault-cli"));
        command
            .args(args)
            .env("AETERNAVAULT_HOME", self.path("home"))
            .env("AETERNAVAULT_PROFILE_ROOT", self.path("profile"))
            .env_remove("AETERNAVAULT_PASSPHRASE")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs with `--passphrase-stdin` and the given lines on standard input.
    fn run_with_secret(&self, args: &[&str], secret: &str) -> Output {
        let mut full = vec!["--passphrase-stdin"];
        full.extend_from_slice(args);
        let mut child = self
            .command(&full)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "{secret}").unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert_success(args, &output);
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        parse(&self.ok(&full))
    }
}

fn assert_success(args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{args:?} failed with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "no JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn all_files(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect()
}

fn add_documents(env: &Env) -> PathBuf {
    let docs = env.path("Data");
    write(&docs.join("letter.txt"), "Dear archive,");
    write(&docs.join("Taxes/receipt.txt"), "42 EUR");
    env.ok(&["source", "add", docs.to_str().unwrap(), "--name", "Data"]);
    docs
}

#[test]
fn back_up_list_and_restore() {
    let env = Env::new();
    add_documents(&env);

    let result = env.json(&["backup"]);
    assert_eq!(result["files"], 2, "{result}");

    let list = env.json(&["list"]);
    let backups = list.as_array().unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0]["status"], "completed");

    let files = env.json(&["files", "latest"]);
    assert_eq!(files.as_array().unwrap().len(), 2);

    let target = env.path("restored");
    env.ok(&[
        "restore",
        "latest",
        "--to",
        target.to_str().unwrap(),
        "--yes",
    ]);
    assert_eq!(
        fs::read_to_string(target.join("Data/letter.txt")).unwrap(),
        "Dear archive,"
    );

    // Every run is recorded in the history.
    let history = env.json(&["history"]);
    assert!(history.as_array().unwrap().len() >= 2);
}

#[test]
fn exit_codes() {
    let env = Env::new();
    add_documents(&env);
    env.ok(&["backup"]);

    // Not confirmed: nothing is restored, exit code 2.
    let target = env.path("restored");
    let output = env.run(&["restore", "latest", "--to", target.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!target.exists());

    // An error: exit code 1.
    let output = env.run(&["files", "no-such-backup"]);
    assert_eq!(output.status.code(), Some(1));
    let output = env.run(&["source", "remove", "not-in-the-list"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn settings_can_be_read_and_changed() {
    let env = Env::new();
    env.ok(&["config", "set", "advanced.hardlink_unchanged", "false"]);
    let value = env.json(&["config", "get", "advanced.hardlink_unchanged"]);
    assert_eq!(value, Value::Bool(false));

    env.ok(&[
        "job", "add", "--every", "day", "--at", "21:30", "--name", "Evening",
    ]);
    let jobs = env.json(&["job", "list"]);
    assert_eq!(jobs.as_array().unwrap().len(), 1);
    env.ok(&["job", "disable", "Evening"]);
    env.ok(&["job", "remove", "Evening"]);
    assert!(env.json(&["job", "list"]).as_array().unwrap().is_empty());

    env.ok(&["retention", "on"]);
    let retention = env.json(&["retention", "show"]);
    assert_eq!(retention["enabled"], true, "{retention}");
}

#[test]
fn encrypted_backups_need_the_passphrase_to_read() {
    let env = Env::new();
    add_documents(&env);
    let passphrase = "correct horse battery staple";
    let recovery_file = env.path("recovery.txt");
    let setup = env.run_with_secret(
        &[
            "encryption",
            "setup",
            "--remember",
            "--recovery-file",
            recovery_file.to_str().unwrap(),
        ],
        passphrase,
    );
    assert_success(&["encryption", "setup"], &setup);
    // The recovery key opens everything: its file is for its owner only.
    assert!(fs::read_to_string(&recovery_file).unwrap().contains('-'));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&recovery_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Backing up uses the remembered key.
    env.ok(&["backup"]);
    let list = env.json(&["list"]);
    assert_eq!(list.as_array().unwrap().len(), 1);

    // No plain names or contents at the destination.
    for file in all_files(&env.path("dest")) {
        let name = file.display().to_string();
        assert!(!name.contains("letter"), "plain name: {name}");
        let content = fs::read(&file).unwrap();
        assert!(
            !content.windows(5).any(|w| w == b"Dear "),
            "plain content in {name}"
        );
    }

    // Reading names or contents never uses the remembered key.
    let output = env.run(&["files", "latest"]);
    assert_eq!(output.status.code(), Some(1));
    let wrong = env.run_with_secret(&["files", "latest"], "wrong");
    assert_eq!(wrong.status.code(), Some(1));
    let files = env.run_with_secret(&["--json", "files", "latest"], passphrase);
    assert_success(&["files", "latest"], &files);
    assert_eq!(parse(&files).as_array().unwrap().len(), 2);

    let target = env.path("restored");
    let restore = env.run_with_secret(
        &[
            "restore",
            "latest",
            "--to",
            target.to_str().unwrap(),
            "--yes",
        ],
        passphrase,
    );
    assert_success(&["restore"], &restore);
    assert_eq!(
        fs::read_to_string(target.join("Data/Taxes/receipt.txt")).unwrap(),
        "42 EUR"
    );

    // A damaged backup checked with the remembered key: problems are reported
    // without the names of the encrypted files.
    let blob = all_files(&env.path("dest"))
        .into_iter()
        .find(|f| f.extension().is_some_and(|e| e == "avb"))
        .unwrap();
    fs::remove_file(blob).unwrap();
    let check = env.run(&["--json", "verify", "latest"]);
    assert_eq!(check.status.code(), Some(2));
    let text = String::from_utf8_lossy(&check.stdout);
    assert!(text.contains("encrypted file"), "{text}");
    assert!(
        !text.contains("letter") && !text.contains("receipt"),
        "{text}"
    );
    // With the passphrase, the names are shown.
    let named = env.run_with_secret(&["--json", "verify", "latest"], passphrase);
    let text = String::from_utf8_lossy(&named.stdout);
    assert!(
        text.contains("letter") || text.contains("receipt"),
        "{text}"
    );
}

#[test]
fn piped_byte_order_mark_is_not_part_of_the_passphrase() {
    let env = Env::new();
    add_documents(&env);
    // Windows PowerShell 5.1 sends "\u{feff}secret\r\n" when piping a string.
    let setup = env.run_with_secret(&["encryption", "setup"], "\u{feff}piped passphrase\r");
    assert_success(&["encryption", "setup"], &setup);

    let output = env
        .command(&["files", "latest"])
        .env("AETERNAVAULT_PASSPHRASE", "piped passphrase")
        .output()
        .unwrap();
    // No backup yet, but the passphrase was accepted.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("not correct"), "{stderr}");
}

/// A job with its own destination is unlocked with the passphrase of that
/// destination, not of the general one.
#[test]
fn job_with_its_own_encrypted_destination_takes_its_passphrase() {
    let env = Env::new();
    add_documents(&env);
    let general = env.path("dest");
    let own = env.path("job-dest");
    for (destination, passphrase) in [(&own, "the job's passphrase"), (&general, "general words")] {
        env.ok(&["destination", "set", destination.to_str().unwrap()]);
        let setup = env.run_with_secret(&["encryption", "setup"], passphrase);
        assert_success(&["encryption", "setup"], &setup);
    }
    env.ok(&[
        "job",
        "add",
        "--every",
        "day",
        "--name",
        "Nightly",
        "--destination",
        own.to_str().unwrap(),
    ]);

    let wrong = env.run_with_secret(&["backup", "--job", "Nightly"], "general words");
    assert_eq!(wrong.status.code(), Some(1));
    let output = env.run_with_secret(&["backup", "--job", "Nightly"], "the job's passphrase");
    assert_success(&["backup", "--job"], &output);
    let at_job = env.json(&["list", "--destination", own.to_str().unwrap()]);
    assert_eq!(at_job.as_array().unwrap().len(), 1);
    assert!(env.json(&["list"]).as_array().unwrap().is_empty());
}

/// Answers one HTTP request on this computer and returns its body.
fn receive_one_request() -> (u16, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Read};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim().to_ascii_lowercase();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        reader
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .unwrap();
        let _ = sender.send(String::from_utf8_lossy(&body).into_owned());
    });
    (port, receiver)
}

/// The notification of a job with its own destination is about that
/// destination (name, free space), not about the general one.
#[test]
fn notification_names_the_destination_of_the_job() {
    let env = Env::new();
    add_documents(&env);
    let own = env.path("job-dest");
    env.ok(&[
        "job",
        "add",
        "--every",
        "day",
        "--name",
        "Nightly",
        "--destination",
        own.to_str().unwrap(),
    ]);
    let (port, received) = receive_one_request();
    let url = format!("http://127.0.0.1:{port}/hook");
    env.ok(&[
        "notify", "add", "--kind", "webhook", "--name", "Hook", "--url", &url,
    ]);
    env.ok(&["config", "set", "notifications.on_success", "true"]);
    env.ok(&[
        "config",
        "set",
        "notifications.success_message",
        "to {destination}",
    ]);

    env.ok(&["backup", "--job", "Nightly"]);
    let body = received
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    let message: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        message["message"].as_str().unwrap(),
        format!("to {}", own.display())
    );
}
