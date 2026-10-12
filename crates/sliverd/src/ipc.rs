//! Glue between `sliver_ipc::transport` and the daemon core: turning received fds into
//! [`Attachment`]s. The event loop owns the sockets; the core only sees decoded messages.

use std::os::fd::OwnedFd;

use sliver_ipc::AgentMsg;
use sliver_ipc::shm::MappedBuffer;

use crate::backend::PixelSource;

/// What came with a packet in `SCM_RIGHTS`.
pub enum Attachment {
    None,
    /// An imported, sealed, mapped buffer (only for `attach_buffer`).
    Buffer(Box<dyn PixelSource>),
    /// The buffer's fd couldn't be imported (not sealed, too small, mmap failed).
    Rejected(String),
    /// `attach_buffer` without exactly one fd, or an fd on any other message (`malformed`).
    WrongCount,
}

impl PixelSource for MappedBuffer {
    fn copy_tight(&self, dst: &mut Vec<u8>) {
        self.copy_to(dst);
    }
}

/// Imports the fd of an `attach_buffer` (seal + size check, read-only mmap). fds on any other
/// message are closed and reported.
pub fn import_attachment(msg: &AgentMsg, mut fds: Vec<OwnedFd>) -> Attachment {
    if msg.carries_fd() != (fds.len() == 1) {
        return Attachment::WrongCount;
    }
    match (msg, fds.len()) {
        (_, 0) => Attachment::None,
        (
            AgentMsg::AttachBuffer {
                width,
                height,
                stride,
                format,
                ..
            },
            1,
        ) => {
            let fd = fds.pop().expect("one fd");
            match MappedBuffer::import(fd, *width, *height, *stride, *format) {
                Ok(b) => Attachment::Buffer(Box::new(b)),
                Err(e) => Attachment::Rejected(e.to_string()),
            }
        }
        (_, _) => Attachment::WrongCount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sliver_ipc::proto::PixelFormat;
    use sliver_ipc::shm::ShmBuffer;
    use std::os::fd::AsFd;

    #[test]
    fn imports_sealed_memfd() {
        let mut b = ShmBuffer::new(4, 2, PixelFormat::Xrgb8888).unwrap();
        b.pixels_mut()[0] = 7;
        let msg = b.attach_msg(1);
        let fd = b.as_fd().try_clone_to_owned().unwrap();
        let Attachment::Buffer(src) = import_attachment(&msg, vec![fd]) else {
            panic!("not imported");
        };
        let mut out = Vec::new();
        src.copy_tight(&mut out);
        assert_eq!((out.len(), out[0]), (32, 7));
        let fd = b.as_fd().try_clone_to_owned().unwrap();
        assert!(matches!(
            import_attachment(&AgentMsg::Wake, vec![fd]),
            Attachment::WrongCount
        ));
        assert!(matches!(
            import_attachment(&msg, vec![]),
            Attachment::WrongCount
        ));
        assert!(matches!(
            import_attachment(&AgentMsg::Wake, vec![]),
            Attachment::None
        ));
    }
}
