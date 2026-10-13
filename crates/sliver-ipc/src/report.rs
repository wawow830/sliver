//! Asking sliverd what it found and opened (`report`, since protocol 1.1), for `sliverctl doctor`
//! (T140). Read-only and any uid: it never becomes an agent, so the bar's agent is unaffected.

use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::proto::{AgentMsg, DaemonMsg, HwReport, PROTO};
use crate::transport::Conn;

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Report(Box<HwReport>),
    /// sliverd answered with an error: it doesn't know `report` (older than protocol 1.1, which
    /// answers `unknown_message`). Its message.
    TooOld(String),
}

/// Connects to sliverd at `path`, asks for its [`HwReport`] and waits up to `timeout`.
/// Errors: not running (connect fails), hung (timeout), or hung up without an answer.
/// `client` names the asker in sliverd's log, e.g. `sliverctl 0.1.0`.
pub fn ask(path: impl AsRef<Path>, client: &str, timeout: Duration) -> io::Result<Reply> {
    let conn = Conn::connect(path)?;
    conn.send(&AgentMsg::Report {
        proto: PROTO,
        client: client.into(),
    })?;
    let ts = Timespec {
        tv_sec: timeout.as_secs() as i64,
        tv_nsec: i64::from(timeout.subsec_nanos()),
    };
    let fd = conn.as_fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    if poll(&mut fds, Some(&ts))? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "sliverd didn't answer",
        ));
    }
    match conn.recv::<DaemonMsg>()? {
        Some(p) => match p.msg {
            Ok(DaemonMsg::HwReport(r)) => Ok(Reply::Report(r)),
            Ok(DaemonMsg::Error { msg, .. }) => Ok(Reply::TooOld(msg)),
            Ok(other) => Err(io::Error::other(format!("unexpected answer: {other:?}"))),
            Err(e) => Err(io::Error::other(format!("{e:?}"))),
        },
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "sliverd hung up without answering",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ErrorCode;
    use crate::transport::Listener;

    fn serve_once(name: &str, answer: Option<DaemonMsg>) -> std::path::PathBuf {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("report-{name}-{}.sock", std::process::id()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        let l = Listener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let c = l.accept().unwrap();
            let got = c.recv::<AgentMsg>().unwrap().unwrap().msg.unwrap();
            assert!(matches!(got, AgentMsg::Report { proto: PROTO, .. }));
            match answer {
                Some(m) => c.send(&m).unwrap(),
                None => std::thread::sleep(Duration::from_millis(500)),
            }
        });
        path
    }

    #[test]
    fn report_too_old_and_hung() {
        let r = HwReport {
            backend: "fake".into(),
            ..Default::default()
        };
        let p = serve_once("ok", Some(DaemonMsg::HwReport(Box::new(r.clone()))));
        assert_eq!(
            ask(&p, "test", Duration::from_secs(2)).unwrap(),
            Reply::Report(Box::new(r))
        );

        let old = DaemonMsg::Error {
            code: ErrorCode::UnknownMessage,
            msg: "unknown message type \"report\"".into(),
            serial: None,
        };
        let p2 = serve_once("old", Some(old));
        assert_eq!(
            ask(&p2, "test", Duration::from_secs(2)).unwrap(),
            Reply::TooOld("unknown message type \"report\"".into())
        );

        let p3 = serve_once("hung", None);
        let e = ask(&p3, "test", Duration::from_millis(50)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);

        for p in [p, p2, p3] {
            let _ = std::fs::remove_file(p);
        }
        assert!(
            ask(
                "/nonexistent/sliverd.sock",
                "test",
                Duration::from_millis(50)
            )
            .is_err()
        );
    }
}
