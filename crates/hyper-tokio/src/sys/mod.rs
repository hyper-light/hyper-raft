//! The operating system's batched UDP calls, receive timestamps and monotonic clock, where the
//! portable path through tokio and std has none.

use std::io;

/// Bound: the sizes asked of the kernel for a receive buffer, each half the last, before the ask
/// is refused (hyper-rt's own bound, `netsys::BUFFER_ASKS`): 32 halvings reach a single byte from
/// any size a 32-bit option takes.
#[cfg(any(target_os = "macos", windows))]
pub(crate) const BUFFER_ASKS: usize = 32;

/// Asks the kernel for a receive buffer of `bytes` on `socket` and returns the buffer it reports
/// after, as hyper-rt's `UdpSocket::set_recv_buffer_bytes` does: Linux's `SO_RCVBUFFORCE` first,
/// past `net.core.rmem_max` for a process that may, then `SO_RCVBUF`; a size the OS refuses outright
/// halved and asked again, at most [`BUFFER_ASKS`] times.
pub(crate) fn set_recv_buffer(socket: &std::net::UdpSocket, bytes: usize) -> io::Result<usize> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        linux::set_recv_buffer(socket.as_raw_fd(), bytes)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        macos::set_recv_buffer(socket.as_raw_fd(), bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        windows::set_recv_buffer(socket.as_raw_socket(), bytes)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (socket, bytes);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a receive buffer is asked only on Linux, macOS and Windows",
        ))
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;
