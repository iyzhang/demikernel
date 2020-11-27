// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::{
    cache::ArpCache,
    pdu::{
        ArpMessage,
        ArpOperation,
        ArpPdu,
    },
};
use crate::{
    fail::Fail,
    protocols::ethernet2::{
        frame::{
            EtherType2,
            Ethernet2Header,
        },
        MacAddress,
    },
    runtime::Runtime,
    scheduler::SchedulerHandle,
    sync::Bytes,
};
use futures::FutureExt;
use hashbrown::HashMap;
use std::{
    cell::RefCell,
    future::Future,
    net::Ipv4Addr,
    rc::Rc,
    time::{
        Duration,
        Instant,
    },
};

#[derive(Clone)]
pub struct ArpPeer<RT: Runtime> {
    rt: RT,
    // TODO: Move this to a strong owner that gets polled once.
    cache: Rc<RefCell<ArpCache>>,
    background: Rc<SchedulerHandle>,
}

impl<RT: Runtime> ArpPeer<RT> {
    pub fn new(now: Instant, rt: RT) -> Result<ArpPeer<RT>, Fail> {
        let options = rt.arp_options();
        let cache = Rc::new(RefCell::new(ArpCache::new(now, Some(options.cache_ttl), options.disable_arp)));
        let handle = rt.spawn(Self::background(rt.clone(), cache.clone()));
        let peer = ArpPeer {
            rt,
            cache,
            background: Rc::new(handle),
        };
        for (&link_addr, &ipv4_addr) in &options.initial_values {
            peer.insert(ipv4_addr, link_addr);
        }
        Ok(peer)
    }

    async fn background(rt: RT, cache: Rc<RefCell<ArpCache>>) {
        loop {
            let current_time = rt.now();
            {
                let mut cache = cache.borrow_mut();
                cache.advance_clock(current_time);
                cache.try_evict(2);
            }
            // TODO: Make this more precise.
            rt.wait(Duration::from_secs(1)).await;
        }
    }

    pub fn receive(&mut self, buf: Bytes) -> Result<(), Fail> {
        // from RFC 826:
        // > ?Do I have the hardware type in ar$hrd?
        // > [optionally check the hardware length ar$hln]
        // > ?Do I speak the protocol in ar$pro?
        // > [optionally check the protocol length ar$pln]
        let pdu = ArpPdu::parse(buf)?;

        // from RFC 826:
        // > Merge_flag := false
        // > If the pair <protocol type, sender protocol address> is
        // > already in my translation table, update the sender
        // > hardware address field of the entry with the new
        // > information in the packet and set Merge_flag to true.
        let merge_flag = {
            let mut cache = self.cache.borrow_mut();
            if cache.get_link_addr(pdu.sender_protocol_addr).is_some() {
                cache.insert(pdu.sender_protocol_addr, pdu.sender_hardware_addr);
                true
            } else {
                false
            }
        };
        // from RFC 826: ?Am I the target protocol address?
        if pdu.target_protocol_addr != self.rt.local_ipv4_addr() {
            if merge_flag {
                // we did do something.
                return Ok(());
            } else {
                // we didn't do anything.
                return Err(Fail::Ignored {
                    details: "unrecognized IP address",
                });
            }
        }
        // from RFC 826:
        // > If Merge_flag is false, add the triplet <protocol type,
        // > sender protocol address, sender hardware address> to
        // > the translation table.
        if !merge_flag {
            self.cache
                .borrow_mut()
                .insert(pdu.sender_protocol_addr, pdu.sender_hardware_addr);
        }

        match pdu.operation {
            ArpOperation::Request => {
                // from RFC 826:
                // > Swap hardware and protocol fields, putting the local
                // > hardware and protocol addresses in the sender fields.
                let reply = ArpMessage {
                    ethernet2_hdr: Ethernet2Header {
                        dst_addr: pdu.sender_hardware_addr,
                        src_addr: self.rt.local_link_addr(),
                        ether_type: EtherType2::Arp,
                    },
                    arp_pdu: ArpPdu {
                        operation: ArpOperation::Reply,
                        sender_hardware_addr: self.rt.local_link_addr(),
                        sender_protocol_addr: self.rt.local_ipv4_addr(),
                        target_hardware_addr: pdu.sender_hardware_addr,
                        target_protocol_addr: pdu.sender_protocol_addr,
                    },
                };
                self.rt.transmit(reply);
                Ok(())
            },
            ArpOperation::Reply => {
                debug!(
                    "reply from `{}/{}`",
                    pdu.sender_protocol_addr, pdu.sender_hardware_addr
                );
                self.cache
                    .borrow_mut()
                    .insert(pdu.sender_protocol_addr, pdu.sender_hardware_addr);
                Ok(())
            },
        }
    }

    pub fn try_query(&self, ipv4_addr: Ipv4Addr) -> Option<MacAddress> {
        self.cache.borrow().get_link_addr(ipv4_addr).cloned()
    }

    pub fn query(&self, ipv4_addr: Ipv4Addr) -> impl Future<Output = Result<MacAddress, Fail>> {
        let rt = self.rt.clone();
        let cache = self.cache.clone();
        async move {
            if let Some(&link_addr) = cache.borrow().get_link_addr(ipv4_addr) {
                return Ok(link_addr);
            }
            let msg = ArpMessage {
                ethernet2_hdr: Ethernet2Header {
                    dst_addr: MacAddress::broadcast(),
                    src_addr: rt.local_link_addr(),
                    ether_type: EtherType2::Arp,
                },
                arp_pdu: ArpPdu {
                    operation: ArpOperation::Request,
                    sender_hardware_addr: rt.local_link_addr(),
                    sender_protocol_addr: rt.local_ipv4_addr(),
                    target_hardware_addr: MacAddress::broadcast(),
                    target_protocol_addr: ipv4_addr,
                },
            };
            let arp_response = cache.borrow_mut().wait_link_addr(ipv4_addr).fuse();
            futures::pin_mut!(arp_response);

            // from TCP/IP illustrated, chapter 4:
            // > The frequency of the ARP request is very close to one per
            // > second, the maximum suggested by [RFC1122].
            let arp_options = rt.arp_options();

            for i in 0..arp_options.retry_count + 1 {
                rt.transmit(msg.clone());
                futures::select! {
                    link_addr = arp_response => {
                        debug!("ARP result available ({})", link_addr);
                        return Ok(link_addr);
                    },
                    _ = rt.wait(arp_options.request_timeout).fuse() => {
                        warn!("ARP request timeout; attempt {}.", i + 1);
                    },
                }
            }
            Err(Fail::Timeout {})
        }
    }

    pub fn export_cache(&self) -> HashMap<Ipv4Addr, MacAddress> {
        self.cache.borrow().export()
    }

    pub fn import_cache(&self, cache: HashMap<Ipv4Addr, MacAddress>) {
        self.cache.borrow_mut().import(cache);
    }

    pub fn insert(&self, ipv4_addr: Ipv4Addr, link_addr: MacAddress) {
        self.cache.borrow_mut().insert(ipv4_addr, link_addr);
    }
}
