//! TUN interface abstraction.
//!
//! Everywhere except macOS this is a re-export of [`tokio_tun`]. macOS's `utun(4)`
//! driver is not a character device but a kernel control socket, so it gets its own
//! implementation here. Both expose the same API for the [`Stack`](crate::Stack).

#[cfg(not(target_os = "macos"))]
mod platform {
    pub use tokio_tun::{Tun, TunBuilder};
}

#[cfg(target_os = "macos")]
mod platform {
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrIn, SockaddrIn6, socket};
    use std::io;
    use std::mem;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use tokio::io::unix::AsyncFd;

    /// `_IOWR('N', 3, struct ctl_info)`, from `<sys/kern_control.h>`.
    const CTLIOCGINFO: libc::c_ulong = 0xc064_4e03;

    /// `UTUN_OPT_IFNAME` and the kernel control it belongs to, from `<net/if_utun.h>`.
    const UTUN_OPT_IFNAME: libc::c_int = 2;
    const UTUN_CONTROL_NAME: &[u8] = b"com.apple.net.utun_control";

    /// `IN6_IFF_NODAD`, from `<netinet6/in6_var.h>`.
    const IN6_IFF_NODAD: libc::c_int = 0x0020;
    /// `ND6_INFINITE_LIFETIME`, from `<netinet6/nd6.h>`.
    const ND6_INFINITE_LIFETIME: u32 = 0xffff_ffff;

    /// `struct ifaliasreq` from `<net/if.h>`: the address to add, its peer and the
    /// netmask. The kernel reads the peer from `dstaddr` on a point to point interface.
    /// Not in libc yet, see <https://github.com/rust-lang/libc/issues/4435>.
    #[repr(C)]
    struct IfAliasReq {
        name: [libc::c_char; libc::IFNAMSIZ],
        addr: libc::sockaddr_in,
        dstaddr: libc::sockaddr_in,
        mask: libc::sockaddr_in,
    }

    /// `struct in6_aliasreq` from `<netinet6/in6_var.h>`.
    #[repr(C)]
    struct In6AliasReq {
        name: [libc::c_char; libc::IFNAMSIZ],
        addr: libc::sockaddr_in6,
        dstaddr: libc::sockaddr_in6,
        prefixmask: libc::sockaddr_in6,
        flags: libc::c_int,
        lifetime: libc::in6_addrlifetime,
    }

    // The interface ioctls, from `<sys/sockio.h>` and `<netinet6/in6_var.h>`. nix builds
    // the request code from the size of the struct, so the layout of the two above is
    // what the kernel ends up seeing.
    nix::ioctl_write_ptr!(siocaifaddr, b'i', 26, IfAliasReq);
    nix::ioctl_write_ptr!(siocaifaddr_in6, b'i', 26, In6AliasReq);
    nix::ioctl_readwrite!(siocgifflags, b'i', 17, libc::ifreq);
    nix::ioctl_write_ptr!(siocsifflags, b'i', 16, libc::ifreq);

    /// A macOS `utun(4)` interface.
    ///
    /// Unlike Linux's Tun this is a socket, and every packet read from or written to
    /// it is prefixed with a 4 byte address family in network byte order. That prefix
    /// is handled here so that callers only ever deal in bare IP packets.
    pub struct Tun {
        name: String,
        io: AsyncFd<OwnedFd>,
    }

    impl Tun {
        /// Receives a packet from the utun interface.
        pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                let mut guard = self.io.readable().await?;
                match guard.try_io(|inner| recv_packet(inner.as_raw_fd(), buf)) {
                    Ok(res) => return res,
                    Err(_) => continue,
                }
            }
        }

        /// Sends a packet to the utun interface.
        pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
            loop {
                let mut guard = self.io.writable().await?;
                match guard.try_io(|inner| send_packet(inner.as_raw_fd(), buf)) {
                    Ok(res) => return res,
                    Err(_) => continue,
                }
            }
        }

        /// Tries to send a packet to the utun interface.
        ///
        /// When the interface queue is full, `Err(io::ErrorKind::WouldBlock)` is returned.
        pub fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
            send_packet(self.io.as_raw_fd(), buf)
        }

        /// Returns the name of the utun interface, for example `utun4`.
        pub fn name(&self) -> &str {
            &self.name
        }
    }

    /// Builds a [`Tun`], mirroring the subset of `tokio_tun::TunBuilder` that Phantun uses.
    #[derive(Default)]
    pub struct TunBuilder {
        name: String,
        address: Option<Ipv4Addr>,
        destination: Option<Ipv4Addr>,
        up: bool,
    }

    impl TunBuilder {
        pub fn new() -> Self {
            Self::default()
        }

        /// Sets the name of device, if it is empty, then the device name is set by the kernel.
        pub fn name(mut self, name: &str) -> Self {
            self.name = name.into();
            self
        }

        /// Sets the IPv4 address of the device.
        pub fn address(mut self, address: Ipv4Addr) -> Self {
            self.address = Some(address);
            self
        }

        /// Sets the IPv4 destination (peer) address of the device.
        pub fn destination(mut self, destination: Ipv4Addr) -> Self {
            self.destination = Some(destination);
            self
        }

        /// Sets up the device.
        pub fn up(mut self) -> Self {
            self.up = true;
            self
        }

        /// Accepted for compatibility with `tokio_tun::TunBuilder`, utun is single queue
        /// so a single [`Tun`] is always built.
        pub fn queues(self, _queues: usize) -> Self {
            self
        }

        /// Builds a new instance of [`Tun`].
        pub fn build(self) -> io::Result<Vec<Tun>> {
            let (fd, name) = open_utun(&self.name)?;

            // Sets the addresses and brings the interface up, the equivalent of what
            // tokio-tun does with netlink on Linux.
            if let Some(address) = self.address {
                // a point to point interface has no peer unless one is given, the
                // kernel falls back to the address itself then, as ifconfig does.
                assign_ipv4(&name, address, self.destination.unwrap_or(address))?;
            }
            if self.up {
                set_up(&name)?;
            }

            Ok(vec![Tun {
                name,
                io: AsyncFd::new(fd)?,
            }])
        }
    }

    /// Creates a utun interface and returns its file descriptor and kernel assigned name.
    ///
    /// Creating a utun interface requires root.
    fn open_utun(name: &str) -> io::Result<(OwnedFd, String)> {
        // an empty name lets the kernel pick the first free interface. Control units are
        // numbered from 1 while interface units are numbered from 0, hence the + 1.
        let unit = if name.is_empty() {
            0
        } else {
            match name
                .strip_prefix("utun")
                .and_then(|unit| unit.parse::<u32>().ok())
            {
                Some(unit) => unit + 1,
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{name} is not a valid utun interface name"),
                    ));
                }
            }
        };

        let fd = unsafe { libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        let mut info: libc::ctl_info = unsafe { mem::zeroed() };
        for (dst, src) in info.ctl_name.iter_mut().zip(UTUN_CONTROL_NAME) {
            *dst = *src as libc::c_char;
        }
        if unsafe { libc::ioctl(fd.as_raw_fd(), CTLIOCGINFO, &mut info) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let addr = libc::sockaddr_ctl {
            sc_len: mem::size_of::<libc::sockaddr_ctl>() as libc::c_uchar,
            sc_family: libc::AF_SYSTEM as libc::c_uchar,
            ss_sysaddr: libc::AF_SYS_CONTROL as u16,
            sc_id: info.ctl_id,
            sc_unit: unit,
            sc_reserved: [0; 5],
        };
        let sockaddr = std::ptr::from_ref(&addr).cast::<libc::sockaddr>();
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                sockaddr,
                mem::size_of_val(&addr) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut name = [0u8; libc::IF_NAMESIZE];
        let mut len = name.len() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SYSPROTO_CONTROL,
                UTUN_OPT_IFNAME,
                name.as_mut_ptr().cast(),
                &mut len,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // the returned length includes the terminating NUL
        let name = String::from_utf8_lossy(&name[..len.saturating_sub(1) as usize]).into_owned();

        // AsyncFd requires a non-blocking file descriptor
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }

        Ok((fd, name))
    }

    /// Adds `address` with `destination` as its point to point peer, the equivalent of
    /// `ifconfig <name> <address> <destination> netmask 255.255.255.255`.
    fn assign_ipv4(name: &str, address: Ipv4Addr, destination: Ipv4Addr) -> io::Result<()> {
        let mut req = IfAliasReq {
            name: [0; libc::IFNAMSIZ],
            addr: sockaddr_in(address),
            dstaddr: sockaddr_in(destination),
            // the netmask has to be explicit, otherwise the kernel picks one from the
            // address class and installs a route for the whole subnet.
            mask: sockaddr_in(Ipv4Addr::new(255, 255, 255, 255)),
        };
        write_name(&mut req.name, name)?;
        use_socket(AddressFamily::Inet, |fd| {
            checked(name, unsafe { siocaifaddr(fd, &req) })
        })
    }

    /// Adds `address` with `destination` as its point to point peer, the equivalent of
    /// `ifconfig <name> inet6 <address> <destination> prefixlen 128`.
    pub fn assign_ipv6_address(
        name: &str,
        address: Ipv6Addr,
        destination: Ipv6Addr,
    ) -> io::Result<()> {
        let mut req = In6AliasReq {
            name: [0; libc::IFNAMSIZ],
            addr: sockaddr_in6(address),
            dstaddr: sockaddr_in6(destination),
            // a /128, the mask covers the whole address
            prefixmask: sockaddr_in6(Ipv6Addr::from(u128::MAX)),
            // the peer address is configured, there is nothing to detect, so skip
            // duplicate address detection and the tentative window that comes with it.
            flags: IN6_IFF_NODAD,
            lifetime: libc::in6_addrlifetime {
                ia6t_expire: 0,
                ia6t_preferred: 0,
                ia6t_vltime: ND6_INFINITE_LIFETIME,
                ia6t_pltime: ND6_INFINITE_LIFETIME,
            },
        };
        write_name(&mut req.name, name)?;
        use_socket(AddressFamily::Inet6, |fd| {
            checked(name, unsafe { siocaifaddr_in6(fd, &req) })
        })
    }

    /// Brings `name` up, the equivalent of `ifconfig <name> up`.
    fn set_up(name: &str) -> io::Result<()> {
        use_socket(AddressFamily::Inet, |fd| {
            let mut req: libc::ifreq = unsafe { mem::zeroed() };
            write_name(&mut req.ifr_name, name)?;
            checked(name, unsafe { siocgifflags(fd, &mut req) })?;
            // SAFETY: the kernel just filled the flags member of the union in.
            unsafe { req.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short };
            checked(name, unsafe { siocsifflags(fd, &req) })
        })
    }

    /// Runs `block` with a socket of the given `family` open. The ioctls above need a
    /// socket to be issued on, but do not care which one.
    fn use_socket(
        family: AddressFamily,
        block: impl FnOnce(RawFd) -> io::Result<()>,
    ) -> io::Result<()> {
        let fd = socket(family, SockType::Datagram, SockFlag::empty(), None)?;
        block(fd.as_raw_fd())
    }

    /// Turns a bare errno into an error naming the interface it came from.
    fn checked(name: &str, res: nix::Result<libc::c_int>) -> io::Result<()> {
        res.map(|_| ()).map_err(|err| {
            let err = io::Error::from(err);
            io::Error::new(err.kind(), format!("unable to configure {name}: {err}"))
        })
    }

    /// Copies an interface name into a fixed size ioctl field.
    fn write_name(dst: &mut [libc::c_char], name: &str) -> io::Result<()> {
        if name.len() >= dst.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} is not a valid utun interface name"),
            ));
        }
        for (dst, src) in dst.iter_mut().zip(name.bytes()) {
            *dst = src as libc::c_char;
        }
        Ok(())
    }

    fn sockaddr_in(address: Ipv4Addr) -> libc::sockaddr_in {
        *SockaddrIn::from(SocketAddrV4::new(address, 0)).as_ref()
    }

    fn sockaddr_in6(address: Ipv6Addr) -> libc::sockaddr_in6 {
        *SockaddrIn6::from(SocketAddrV6::new(address, 0, 0, 0)).as_ref()
    }

    /// Reads a single packet, dropping the utun address family prefix.
    fn recv_packet(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
        let mut af = [0u8; 4];
        let mut iov = [
            libc::iovec {
                iov_base: af.as_mut_ptr().cast(),
                iov_len: af.len(),
            },
            libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.len(),
            },
        ];
        let size = unsafe { libc::readv(fd, iov.as_mut_ptr(), iov.len() as libc::c_int) };
        if size < 0 {
            return Err(io::Error::last_os_error());
        }
        if (size as usize) < af.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated utun packet",
            ));
        }

        Ok(size as usize - af.len())
    }

    /// Writes a single packet, prefixing it with the utun address family header.
    fn send_packet(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
        let af = match buf.first().map(|version| version >> 4) {
            Some(4) => libc::AF_INET,
            Some(6) => libc::AF_INET6,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not an IP packet",
                ));
            }
        }
        .to_be_bytes();
        let iov = [
            libc::iovec {
                iov_base: af.as_ptr() as *mut libc::c_void,
                iov_len: af.len(),
            },
            libc::iovec {
                iov_base: buf.as_ptr() as *mut libc::c_void,
                iov_len: buf.len(),
            },
        ];
        let size = unsafe { libc::writev(fd, iov.as_ptr(), iov.len() as libc::c_int) };
        if size < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(size as usize - af.len())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// nix puts the size of the struct into the ioctl request and the kernel reads
        /// its fields at fixed offsets, either of which only shows up as an EINVAL at
        /// runtime once the process is already root.
        #[test]
        fn request_layout_matches_the_kernel() {
            assert_eq!(mem::size_of::<IfAliasReq>(), 64);
            assert_eq!(mem::size_of::<In6AliasReq>(), 128);
            assert_eq!(std::mem::offset_of!(IfAliasReq, addr), 16);
            assert_eq!(std::mem::offset_of!(IfAliasReq, dstaddr), 32);
            assert_eq!(std::mem::offset_of!(IfAliasReq, mask), 48);
            assert_eq!(std::mem::offset_of!(In6AliasReq, prefixmask), 72);
            assert_eq!(std::mem::offset_of!(In6AliasReq, flags), 100);
            assert_eq!(std::mem::offset_of!(In6AliasReq, lifetime), 104);
        }
    }
}

pub use platform::{Tun, TunBuilder};

/// Assigns an IPv6 address and its point to point peer to a `utun` interface.
#[cfg(target_os = "macos")]
pub use platform::assign_ipv6_address;
