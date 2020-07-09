// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::{
    datagram::{Icmpv4Datagram, Icmpv4Type},
    echo::{Icmpv4Echo, Icmpv4EchoMut, Icmpv4EchoOp},
    error::Icmpv4Error,
};
use crate::{
    prelude::*,
    protocols::{arp, ipv4},
    r#async::WhenAny,
};
use byteorder::{NativeEndian, WriteBytesExt};
use rand::Rng;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    convert::TryFrom,
    io::Write,
    net::Ipv4Addr,
    num::Wrapping,
    process,
    rc::Rc,
    time::{Duration, Instant},
};

pub struct Icmpv4Peer<'a> {
    rt: Runtime<'a>,
    arp: arp::Peer<'a>,
    async_work: WhenAny<'a, ()>,
    outstanding_requests: Rc<RefCell<HashSet<(u16, u16)>>>,
    ping_seq_num_counter: Rc<Cell<Wrapping<u16>>>,
}

impl<'a> Icmpv4Peer<'a> {
    pub fn new(rt: Runtime<'a>, arp: arp::Peer<'a>) -> Icmpv4Peer<'a> {
        // from [TCP/IP Illustrated]():
        // > When a new instance of the ping program is run, the Sequence
        // > Number field starts with the value 0 and is increased by 1 every
        // > time a new Echo Request message is sent.
        let ping_seq_num_counter = Wrapping(0);
        Icmpv4Peer {
            rt,
            arp,
            async_work: WhenAny::new(),
            outstanding_requests: Rc::new(RefCell::new(HashSet::new())),
            ping_seq_num_counter: Rc::new(Cell::new(ping_seq_num_counter)),
        }
    }

    pub fn receive(&mut self, datagram: ipv4::Datagram<'_>) -> Result<()> {
        trace!("Icmpv4Peer::receive(...)");
        let options = self.rt.options();
        let datagram = Icmpv4Datagram::try_from(datagram)?;
        assert_eq!(
            datagram.ipv4().frame().header().dest_addr(),
            options.my_link_addr
        );
        assert_eq!(datagram.ipv4().header().dest_addr(), options.my_ipv4_addr);

        match datagram.header().r#type()? {
            Icmpv4Type::EchoRequest => {
                let dest_ipv4_addr = datagram.ipv4().header().src_addr();
                let datagram = Icmpv4Echo::try_from(datagram)?;
                self.reply_to_ping(
                    dest_ipv4_addr,
                    datagram.id(),
                    datagram.seq_num(),
                );
                Ok(())
            }
            Icmpv4Type::EchoReply => {
                let datagram = Icmpv4Echo::try_from(datagram)?;
                let id = datagram.id();
                let seq_num = datagram.seq_num();
                let mut outstanding_requests =
                    self.outstanding_requests.borrow_mut();
                outstanding_requests.remove(&(id, seq_num));
                Ok(())
            }
            _ => match Icmpv4Error::try_from(datagram) {
                Ok(e) => {
                    self.rt.emit_event(Event::Icmpv4Error {
                        id: e.id(),
                        next_hop_mtu: e.next_hop_mtu(),
                        context: e.context().to_vec(),
                    });
                    Ok(())
                }
                Err(e) => Err(e),
            },
        }
    }

    pub fn ping(
        &self,
        dest_ipv4_addr: Ipv4Addr,
        timeout: Option<Duration>,
    ) -> Future<'a, Duration> {
        let timeout = timeout.unwrap_or_else(|| Duration::from_millis(5000));
        let rt = self.rt.clone();
        let arp = self.arp.clone();
        let outstanding_requests = self.outstanding_requests.clone();
        let id = self.generate_ping_id();
        let seq_num = self.generate_seq_num();
        self.rt.start_coroutine(move || {
            let t0 = rt.now();
            let options = rt.options();
            debug!("initiating ARP query");
            let dest_link_addr =
                r#await!(arp.query(dest_ipv4_addr), rt.now()).unwrap();
            debug!(
                "ARP query complete ({} -> {})",
                dest_ipv4_addr, dest_link_addr
            );

            let mut bytes = Icmpv4Echo::new_vec();
            let mut echo = Icmpv4EchoMut::attach(&mut bytes);
            echo.r#type(Icmpv4EchoOp::Request);
            echo.id(id);
            echo.seq_num(seq_num);
            let mut ipv4_header = echo.icmpv4().ipv4().header();
            ipv4_header.src_addr(options.my_ipv4_addr);
            ipv4_header.dest_addr(dest_ipv4_addr);
            let mut frame_header = echo.icmpv4().ipv4().frame().header();
            frame_header.dest_addr(dest_link_addr);
            frame_header.src_addr(options.my_link_addr);
            let _ = echo.seal()?;
            rt.emit_event(Event::Transmit(Rc::new(RefCell::new(bytes))));

            let key = (id, seq_num);
            {
                let mut outstanding_requests =
                    outstanding_requests.borrow_mut();
                assert!(outstanding_requests.insert(key));
            }

            if yield_until!(
                {
                    let outstanding_requests = outstanding_requests.borrow();
                    !outstanding_requests.contains(&key)
                },
                rt.now(),
                timeout
            ) {
                CoroutineOk(rt.now() - t0)
            } else {
                Err(Fail::Timeout {})
            }
        })
    }

    fn reply_to_ping(
        &mut self,
        dest_ipv4_addr: Ipv4Addr,
        id: u16,
        seq_num: u16,
    ) {
        let rt = self.rt.clone();
        let arp = self.arp.clone();
        let fut = self.rt.start_coroutine(move || {
            let options = rt.options();
            debug!("initiating ARP query");
            let dest_link_addr =
                r#await!(arp.query(dest_ipv4_addr), rt.now()).unwrap();
            debug!(
                "ARP query complete ({} -> {})",
                dest_ipv4_addr, dest_link_addr
            );
            let mut bytes = Icmpv4Echo::new_vec();
            let mut echo = Icmpv4EchoMut::attach(&mut bytes);
            echo.r#type(Icmpv4EchoOp::Reply);
            echo.id(id);
            echo.seq_num(seq_num);
            let ipv4 = echo.icmpv4().ipv4();
            let mut ipv4_header = ipv4.header();
            ipv4_header.src_addr(options.my_ipv4_addr);
            ipv4_header.dest_addr(dest_ipv4_addr);
            let frame = ipv4.frame();
            let mut frame_header = frame.header();
            frame_header.src_addr(options.my_link_addr);
            frame_header.dest_addr(dest_link_addr);
            let _ = echo.seal()?;
            rt.emit_event(Event::Transmit(Rc::new(RefCell::new(bytes))));

            CoroutineOk(())
        });

        self.async_work.add(fut);
    }

    fn generate_ping_id(&self) -> u16 {
        let mut checksum = ipv4::Checksum::new();
        let options = self.rt.options();
        checksum
            .write_u32::<NativeEndian>(options.my_ipv4_addr.into())
            .unwrap();
        checksum.write_u32::<NativeEndian>(process::id()).unwrap();
        let mut rng = self.rt.rng_mut();
        let mut nonce = [0; 2];
        rng.fill(&mut nonce);
        checksum.write_all(&nonce).unwrap();
        checksum.finish()
    }

    fn generate_seq_num(&self) -> u16 {
        let seq_num = self.ping_seq_num_counter.get();
        self.ping_seq_num_counter.set(seq_num + Wrapping(1));
        seq_num.0
    }

    pub fn advance_clock(&self, now: Instant) {
        if let Some(result) = self.async_work.poll(now) {
            assert!(result.is_ok());
        }
    }
}
