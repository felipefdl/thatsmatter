//! Network stack for rs-matter with SO_REUSEADDR/SO_REUSEPORT on UDP binds.
//!
//! HAOS already has something on UDP 5353 (Home Assistant Core, host mDNS, etc.).
//! BuiltinMdns must share that port the same way Matterbridge / rs-matter tests do.

use std::io;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};

use async_io::Async;
use edge_nal::{Dns, TcpBind, TcpConnect, UdpBind, UdpConnect};
use edge_nal_std::{Stack, UdpSocket};
use rs_matter_stack::nal::NetStack;
use socket2::{Domain, Protocol, Socket, Type};

/// `edge_nal_std::Stack` with UDP binds that enable address/port reuse.
#[derive(Default)]
pub struct MatterNetStack {
  std: Stack,
}

impl MatterNetStack {
  pub const fn new() -> Self {
    Self { std: Stack::new() }
  }
}

/// Unit bind helper so `NetStack::UdpBind` does not borrow `self` forever.
#[derive(Copy, Clone, Debug, Default)]
pub struct ReuseUdpBind;

impl UdpBind for ReuseUdpBind {
  type Error = io::Error;

  type Socket<'a>
    = UdpSocket
  where
    Self: 'a;

  async fn bind(&self, local: SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
    let domain = match local {
      SocketAddr::V4(_) => Domain::IPV4,
      SocketAddr::V6(_) => Domain::IPV6,
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    if matches!(local, SocketAddr::V6(_)) {
      // Dual-stack: Matter BuiltinMdns binds `[::]:5353` and joins v4+v6 groups.
      socket.set_only_v6(false)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&local.into())?;
    let std_sock: StdUdpSocket = socket.into();
    std_sock.set_broadcast(true)?;
    let async_sock = Async::new_nonblocking(std_sock)?;
    Ok(UdpSocket::new(async_sock))
  }
}

impl NetStack for MatterNetStack {
  type UdpBind<'a>
    = ReuseUdpBind
  where
    Self: 'a;
  type UdpConnect<'a>
    = &'a Stack
  where
    Self: 'a;
  type TcpBind<'a>
    = &'a Stack
  where
    Self: 'a;
  type TcpConnect<'a>
    = &'a Stack
  where
    Self: 'a;
  type Dns<'a>
    = &'a Stack
  where
    Self: 'a;

  fn udp_bind(&self) -> Option<Self::UdpBind<'_>> {
    Some(ReuseUdpBind)
  }

  fn udp_connect(&self) -> Option<Self::UdpConnect<'_>> {
    Some(&self.std)
  }

  fn tcp_bind(&self) -> Option<Self::TcpBind<'_>> {
    Some(&self.std)
  }

  fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>> {
    Some(&self.std)
  }

  fn dns(&self) -> Option<Self::Dns<'_>> {
    Some(&self.std)
  }
}

// Silence unused-trait import warnings when NetStack only needs the associated types.
const _: fn() = || {
  fn assert_udp_connect<T: UdpConnect>() {}
  fn assert_tcp_bind<T: TcpBind>() {}
  fn assert_tcp_connect<T: TcpConnect>() {}
  fn assert_dns<T: Dns>() {}
  assert_udp_connect::<&Stack>();
  assert_tcp_bind::<&Stack>();
  assert_tcp_connect::<&Stack>();
  assert_dns::<&Stack>();
};
