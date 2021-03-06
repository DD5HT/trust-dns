// Copyright 2015-2016 Benjamin Fry <benjaminfry@me.com>
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use std::fmt::{self, Display};
use std::net::SocketAddr;

use futures::{Async, Future, Poll, Stream};

use error::ProtoError;
use udp::UdpStream;
use xfer::{DnsClientStream, SerialMessage};
use BufDnsStreamHandle;
use DnsStreamHandle;

/// A UDP client stream of DNS binary packets
#[must_use = "futures do nothing unless polled"]
pub struct UdpClientStream {
    name_server: SocketAddr,
    udp_stream: UdpStream,
}

impl UdpClientStream {
    /// it is expected that the resolver wrapper will be responsible for creating and managing
    ///  new UdpClients such that each new client would have a random port (reduce chance of cache
    ///  poisoning)
    ///
    /// # Return
    ///
    /// a tuple of a Future Stream which will handle sending and receiving messsages, and a
    ///  handle which can be used to send messages into the stream.
    pub fn new(name_server: SocketAddr) -> (UdpClientConnect, Box<DnsStreamHandle + Send>) {
        let (stream_future, sender) = UdpStream::new(name_server);

        let new_future = Box::new(
            stream_future
                .map(move |udp_stream| UdpClientStream {
                    name_server,
                    udp_stream,
                }).map_err(ProtoError::from),
        );
        let new_future = UdpClientConnect(new_future);

        let sender = Box::new(BufDnsStreamHandle::new(name_server, sender));

        (new_future, sender)
    }
}

impl Display for UdpClientStream {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(formatter, "UDP({})", self.name_server)
    }
}

impl DnsClientStream for UdpClientStream {
    fn name_server_addr(&self) -> SocketAddr {
        self.name_server
    }
}

impl Stream for UdpClientStream {
    type Item = SerialMessage;
    type Error = ProtoError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match try_ready!(self.udp_stream.poll().map_err(ProtoError::from)) {
            Some(message) => {
                if message.addr() != self.name_server {
                    debug!(
                        "{} does not match name_server: {}",
                        message.addr(),
                        self.name_server
                    )
                }

                Ok(Async::Ready(Some(message)))
            }
            None => Ok(Async::Ready(None)),
        }
    }
}

/// A future that resolves to an UdpClientStream
pub struct UdpClientConnect(Box<Future<Item = UdpClientStream, Error = ProtoError> + Send>);

impl Future for UdpClientConnect {
    type Item = UdpClientStream;
    type Error = ProtoError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.0.poll()
    }
}

#[cfg(not(target_os = "linux"))]
#[cfg(test)]
use std::net::Ipv6Addr;
#[cfg(test)]
use std::net::{IpAddr, Ipv4Addr};

#[test]
fn test_udp_client_stream_ipv4() {
    udp_client_stream_test(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)))
}

#[test]
#[cfg(not(target_os = "linux"))] // ignored until Travis-CI fixes IPv6
fn test_udp_client_stream_ipv6() {
    udp_client_stream_test(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)))
}

#[cfg(test)]
fn udp_client_stream_test(server_addr: IpAddr) {
    use tokio::runtime::current_thread::Runtime;

    use std;
    let succeeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let succeeded_clone = succeeded.clone();
    std::thread::Builder::new()
        .name("thread_killer".to_string())
        .spawn(move || {
            let succeeded = succeeded_clone.clone();
            for _ in 0..15 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if succeeded.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
            }

            panic!("timeout");
        }).unwrap();

    let server = std::net::UdpSocket::bind(SocketAddr::new(server_addr, 0)).unwrap();
    server
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap(); // should recieve something within 5 seconds...
    server
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap(); // should recieve something within 5 seconds...
    let server_addr = server.local_addr().unwrap();

    let test_bytes: &'static [u8; 8] = b"DEADBEEF";
    let send_recv_times = 4;

    // an in and out server
    let server_handle = std::thread::Builder::new()
        .name("test_udp_client_stream_ipv4:server".to_string())
        .spawn(move || {
            let mut buffer = [0_u8; 512];

            for _ in 0..send_recv_times {
                // wait for some bytes...
                let (len, addr) = server.recv_from(&mut buffer).expect("receive failed");

                assert_eq!(&buffer[0..len], test_bytes);

                // bounce them right back...
                assert_eq!(
                    server.send_to(&buffer[0..len], addr).expect("send failed"),
                    len
                );
            }
        }).unwrap();

    // setup the client, which is going to run on the testing thread...
    let mut io_loop = Runtime::new().unwrap();

    // the tests should run within 5 seconds... right?
    // TODO: add timeout here, so that test never hangs...
    // let timeout = Timeout::new(Duration::from_secs(5));
    let (stream, mut sender) = UdpClientStream::new(server_addr);
    let mut stream: UdpClientStream = io_loop.block_on(stream).ok().unwrap();

    for _ in 0..send_recv_times {
        // test once
        sender
            .send(SerialMessage::new(test_bytes.to_vec(), server_addr))
            .unwrap();
        let (buffer, stream_tmp) = io_loop.block_on(stream.into_future()).ok().unwrap();
        stream = stream_tmp;
        assert_eq!(buffer.expect("no buffer received").bytes(), test_bytes);
    }

    succeeded.store(true, std::sync::atomic::Ordering::Relaxed);
    server_handle.join().expect("server thread failed");
}
