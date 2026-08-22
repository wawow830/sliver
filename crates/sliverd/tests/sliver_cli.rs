use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn missing_config_path_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_sliver"))
        .output()
        .expect("failed to run sliver");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr was not UTF-8");
    assert!(stderr.contains("usage: sliver FILE"), "{stderr}");
}

#[test]
fn successful_apply_is_silent_and_sends_absolute_path() {
    let directory = tempfile::tempdir().expect("failed to create temporary directory");
    let runtime = directory.path().join("runtime");
    let socket_directory = runtime.join("sliver");
    std::fs::create_dir_all(&socket_directory).expect("failed to create socket directory");
    let socket = socket_directory.join("supervisor.sock");
    let listener = UnixListener::bind(&socket).expect("failed to bind supervisor stub");
    listener
        .set_nonblocking(true)
        .expect("failed to make supervisor stub nonblocking");
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("supervisor stub did not receive request: {error}"),
            }
        };
        let mut length = [0u8; 4];
        stream
            .read_exact(&mut length)
            .expect("failed to read path length");
        let mut path = vec![0; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut path).expect("failed to read path");
        stream
            .write_all(&[0, 0, 0, 0, 0])
            .expect("failed to send success reply");
        path
    });
    let source = directory.path().join("config.lua");
    std::fs::write(&source, "return {}").expect("failed to create config");

    let output = Command::new(env!("CARGO_BIN_EXE_sliver"))
        .arg(&source)
        .env("XDG_RUNTIME_DIR", &runtime)
        .output()
        .expect("failed to run sliver");

    let requested_path = server.join().expect("supervisor stub panicked");
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(requested_path, source.as_os_str().as_bytes());
}
