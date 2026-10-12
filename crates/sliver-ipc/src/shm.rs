//! Pixel buffers shared through sealed memfds (`docs/api.md` §6).
//!
//! The agent creates a [`ShmBuffer`], draws into it and sends its fd with `attach_buffer`.
//! sliverd [`MappedBuffer::import`]s it and **copies** each presented frame out right away.
//! `F_SEAL_SHRINK | F_SEAL_GROW` guarantee the mapping can't be truncated under sliverd (no
//! SIGBUS). The agent can still write at any time, but that only tears its own frame.

use crate::proto::{AgentMsg, PixelFormat};
use crate::validate::required_len;
use rustix::fs::{MemfdFlags, SealFlags};
use rustix::mm::{MapFlags, ProtFlags};
use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

const REQUIRED_SEALS: SealFlags = SealFlags::SHRINK.union(SealFlags::GROW);

/// A private shared mapping, unmapped on drop.
#[derive(Debug)]
struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}

impl Mapping {
    fn new(fd: BorrowedFd<'_>, len: usize, writable: bool) -> io::Result<Mapping> {
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty mapping"));
        }
        let prot = if writable {
            ProtFlags::READ | ProtFlags::WRITE
        } else {
            ProtFlags::READ
        };
        // SAFETY: we ask the kernel for a fresh mapping (null hint, no MAP_FIXED), so no existing
        // memory is affected. The fd is sealed against shrinking, so all `len` bytes stay backed.
        let ptr =
            unsafe { rustix::mm::mmap(std::ptr::null_mut(), len, prot, MapFlags::SHARED, fd, 0)? };
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::other("mmap returned null"))?;
        Ok(Mapping { ptr, len })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: ptr/len came from a successful mmap and are unmapped exactly once.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

// SAFETY: the mapping is plain memory owned by this value. Moving it to another thread is fine.
unsafe impl Send for Mapping {}

/// Agent side: a frame buffer to draw into.
#[derive(Debug)]
pub struct ShmBuffer {
    fd: OwnedFd,
    map: Mapping,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

impl ShmBuffer {
    pub fn new(width: u32, height: u32, format: PixelFormat) -> io::Result<ShmBuffer> {
        let stride = width
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "width too large"))?;
        let len = usize::try_from(required_len(stride, height))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer too large"))?;
        let fd = rustix::fs::memfd_create(
            "sliver-frame",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?;
        rustix::fs::ftruncate(&fd, len as u64)?;
        rustix::fs::fcntl_add_seals(&fd, REQUIRED_SEALS | SealFlags::SEAL)?;
        let map = Mapping::new(fd.as_fd(), len, true)?;
        Ok(ShmBuffer {
            fd,
            map,
            width,
            height,
            stride,
            format,
        })
    }

    pub fn pixels(&self) -> &[u8] {
        // SAFETY: the mapping is `len` readable bytes, alive as long as `self`.
        unsafe { std::slice::from_raw_parts(self.map.ptr.as_ptr(), self.map.len) }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, writable, and `&mut self` makes this the only view in this process.
        unsafe { std::slice::from_raw_parts_mut(self.map.ptr.as_ptr(), self.map.len) }
    }

    /// The `attach_buffer` message to send along with [`ShmBuffer::fd`].
    pub fn attach_msg(&self, id: u32) -> AgentMsg {
        AgentMsg::AttachBuffer {
            id,
            width: self.width,
            height: self.height,
            stride: self.stride,
            format: self.format,
        }
    }
}

impl AsFd for ShmBuffer {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[derive(Debug)]
pub enum ImportError {
    /// Not a memfd, or missing `F_SEAL_SHRINK`/`F_SEAL_GROW`.
    NotSealed,
    TooSmall {
        have: u64,
        need: u64,
    },
    Io(io::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NotSealed => {
                write!(f, "buffer fd must be a memfd sealed with SHRINK|GROW")
            }
            ImportError::TooSmall { have, need } => {
                write!(f, "buffer has {have} bytes, needs {need}")
            }
            ImportError::Io(e) => write!(f, "buffer import failed: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// sliverd side: a read-only view of an agent's buffer. Validate the header with
/// [`crate::validate::validate_buffer`] first.
#[derive(Debug)]
pub struct MappedBuffer {
    map: Mapping,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

impl MappedBuffer {
    pub fn import(
        fd: OwnedFd,
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    ) -> Result<MappedBuffer, ImportError> {
        let seals = rustix::fs::fcntl_get_seals(&fd).map_err(|_| ImportError::NotSealed)?;
        if !seals.contains(REQUIRED_SEALS) {
            return Err(ImportError::NotSealed);
        }
        let need = required_len(stride, height);
        let have = rustix::fs::fstat(&fd)
            .map_err(|e| ImportError::Io(e.into()))?
            .st_size as u64;
        if have < need || need == 0 {
            return Err(ImportError::TooSmall { have, need });
        }
        let len = usize::try_from(need).map_err(|_| ImportError::TooSmall { have, need })?;
        let map = Mapping::new(fd.as_fd(), len, false).map_err(ImportError::Io)?;
        // The mapping keeps the memory alive; the fd itself isn't needed any more.
        Ok(MappedBuffer {
            map,
            width,
            height,
            stride,
            format,
        })
    }

    /// Copy the frame out, tightly packed (`width * bpp` bytes per row), into `dst`.
    pub fn copy_to(&self, dst: &mut Vec<u8>) {
        let row = (self.width * self.format.bytes_per_pixel()) as usize;
        dst.clear();
        dst.reserve(row * self.height as usize);
        for y in 0..self.height as usize {
            let off = y * self.stride as usize;
            // SAFETY: validate_buffer ensured stride >= row, and import ensured the mapping holds
            // stride * height bytes, so [off, off + row) is in bounds. The agent may write
            // concurrently. That can only tear the copied pixels, which we treat as plain bytes.
            let src = unsafe { std::slice::from_raw_parts(self.map.ptr.as_ptr().add(off), row) };
            dst.extend_from_slice(src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_and_copy() {
        let mut b = ShmBuffer::new(4, 2, PixelFormat::Xrgb8888).unwrap();
        b.pixels_mut()[0] = 7;
        b.pixels_mut()[31] = 9;
        let fd = b.as_fd().try_clone_to_owned().unwrap();
        let m = MappedBuffer::import(fd, 4, 2, 16, PixelFormat::Xrgb8888).unwrap();
        let mut out = Vec::new();
        m.copy_to(&mut out);
        assert_eq!(out.len(), 32);
        assert_eq!((out[0], out[31]), (7, 9));
        // Later agent writes don't affect what was copied.
        b.pixels_mut()[0] = 1;
        assert_eq!(out[0], 7);
    }

    #[test]
    fn seals_hold() {
        let b = ShmBuffer::new(4, 2, PixelFormat::Xrgb8888).unwrap();
        assert!(rustix::fs::ftruncate(&b, 4).is_err());
        assert!(rustix::fs::fcntl_add_seals(&b, SealFlags::WRITE).is_err());
    }

    #[test]
    fn rejects_unsealed_and_short() {
        let fd =
            rustix::fs::memfd_create("t", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING).unwrap();
        rustix::fs::ftruncate(&fd, 32).unwrap();
        let dup = fd.try_clone().unwrap();
        assert!(matches!(
            MappedBuffer::import(dup, 4, 2, 16, PixelFormat::Xrgb8888),
            Err(ImportError::NotSealed)
        ));
        rustix::fs::fcntl_add_seals(&fd, REQUIRED_SEALS).unwrap();
        assert!(matches!(
            MappedBuffer::import(fd, 4, 4, 16, PixelFormat::Xrgb8888),
            Err(ImportError::TooSmall { have: 32, need: 64 })
        ));
        // A regular file (or anything not sealable) is refused.
        let f = std::fs::File::open("/proc/self/stat").unwrap();
        assert!(matches!(
            MappedBuffer::import(f.into(), 4, 2, 16, PixelFormat::Xrgb8888),
            Err(ImportError::NotSealed)
        ));
    }
}
