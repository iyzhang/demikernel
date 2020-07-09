// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

mod isn_generator;

#[cfg(test)]
mod tests;

use super::{
    connection::{TcpConnection, TcpConnectionHandle, TcpConnectionId},
    segment::{TcpSegment, TcpSegmentDecoder, TcpSegmentEncoder},
};
use crate::{
    prelude::*,
    protocols::{arp, ip, ipv4},
    r#async::{Retry, WhenAny},
};
use isn_generator::IsnGenerator;
use rand::seq::SliceRandom;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    convert::TryFrom,
    num::Wrapping,
    rc::Rc,
    time::{Duration, Instant},
};

struct TcpPeerState<'a> {
    arp: arp::Peer<'a>,
    assigned_handles: HashMap<TcpConnectionHandle, Rc<TcpConnectionId>>,
    background_queue: Rc<RefCell<VecDeque<Future<'a, ()>>>>,
    background_work: Rc<RefCell<WhenAny<'a, ()>>>,
    connections: HashMap<Rc<TcpConnectionId>, Rc<RefCell<TcpConnection<'a>>>>,
    isn_generator: IsnGenerator,
    open_ports: HashSet<ip::Port>,
    rt: Runtime<'a>,
    unassigned_connection_handles: VecDeque<TcpConnectionHandle>,
    unassigned_private_ports: VecDeque<ip::Port>, // todo: shared state.
}

impl<'a> TcpPeerState<'a> {
    fn new(rt: Runtime<'a>, arp: arp::Peer<'a>) -> Self {
        // initialize the pool of available private ports.
        let unassigned_private_ports = {
            let mut ports = Vec::new();
            for i in ip::Port::first_private_port().into()..65535 {
                ports.push(ip::Port::try_from(i).unwrap());
            }
            let mut rng = rt.rng_mut();
            ports.shuffle(&mut *rng);
            VecDeque::from(ports)
        };

        // initialize the pool of available connection handles.
        let unassigned_connection_handles = {
            let mut handles = Vec::new();
            for i in 1..u16::max_value() {
                handles.push(TcpConnectionHandle::try_from(i).unwrap());
            }
            let mut rng = rt.rng_mut();
            handles.shuffle(&mut *rng);
            VecDeque::from(handles)
        };

        let isn_generator = IsnGenerator::new(&rt);

        TcpPeerState {
            arp,
            assigned_handles: HashMap::new(),
            background_queue: Rc::new(RefCell::new(VecDeque::new())),
            background_work: Rc::new(RefCell::new(WhenAny::new())),
            connections: HashMap::new(),
            isn_generator,
            open_ports: HashSet::new(),
            rt,
            unassigned_connection_handles,
            unassigned_private_ports,
        }
    }

    fn get_connection_given_handle(
        &self,
        handle: TcpConnectionHandle,
    ) -> Result<&Rc<RefCell<TcpConnection<'a>>>> {
        if let Some(cxnid) = self.assigned_handles.get(&handle) {
            Ok(self.connections.get(cxnid).unwrap())
        } else {
            Err(Fail::ResourceNotFound {
                details: "unrecognized connection handle",
            })
        }
    }

    fn acquire_private_port(&mut self) -> Result<ip::Port> {
        if let Some(p) = self.unassigned_private_ports.pop_front() {
            Ok(p)
        } else {
            Err(Fail::ResourceExhausted {
                details: "no more private ports",
            })
        }
    }

    fn release_private_port(&mut self, port: ip::Port) {
        assert!(port.is_private());
        self.unassigned_private_ports.push_back(port);
    }

    fn acquire_connection_handle(&mut self) -> Result<TcpConnectionHandle> {
        if let Some(h) = self.unassigned_connection_handles.pop_front() {
            Ok(h)
        } else {
            Err(Fail::ResourceExhausted {
                details: "no more connection handles",
            })
        }
    }

    #[allow(dead_code)]
    fn release_connection_handle(&mut self, handle: TcpConnectionHandle) {
        self.unassigned_connection_handles.push_back(handle);
    }

    fn new_connection(
        &mut self,
        cxnid: Rc<TcpConnectionId>,
        rt: Runtime<'a>,
    ) -> Result<Rc<RefCell<TcpConnection<'a>>>> {
        let options = self.rt.options();
        let handle = self.acquire_connection_handle()?;
        let local_isn = self.isn_generator.next(&*cxnid);
        let cxn = TcpConnection::new(
            cxnid.clone(),
            handle,
            local_isn,
            options.tcp.receive_window_size,
            rt.clone(),
        );
        let local_port = cxnid.local.port();
        let cxn = Rc::new(RefCell::new(cxn));
        assert!(self
            .connections
            .insert(cxnid.clone(), cxn.clone())
            .is_none());
        assert!(self.assigned_handles.insert(handle, cxnid).is_none());
        self.open_ports.insert(local_port);
        Ok(cxn)
    }

    fn cast(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        bytes: Rc<RefCell<Vec<u8>>>,
    ) -> Future<'a, ()> {
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            let (arp, rt, remote_ipv4_addr) = {
                let state = state.borrow();
                let rt = state.rt.clone();
                let mut bytes = bytes.borrow_mut();
                let mut encoder = TcpSegmentEncoder::attach(bytes.as_mut());
                encoder.ipv4().header().src_addr(rt.options().my_ipv4_addr);
                let decoder = encoder.unmut();
                (state.arp.clone(), rt, decoder.ipv4().header().dest_addr())
            };

            let remote_link_addr =
                r#await!(arp.query(remote_ipv4_addr), rt.now())?;

            {
                let options = rt.options();
                let mut bytes = bytes.borrow_mut();
                let mut encoder = TcpSegmentEncoder::attach(bytes.as_mut());
                encoder.ipv4().header().src_addr(options.my_ipv4_addr);
                let mut frame_header = encoder.ipv4().frame().header();
                frame_header.src_addr(options.my_link_addr);
                frame_header.dest_addr(remote_link_addr);
                let _ = encoder.seal()?;
            }

            rt.emit_event(Event::Transmit(bytes));
            CoroutineOk(())
        })
    }

    fn new_active_connection(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        cxnid: Rc<TcpConnectionId>,
    ) -> Future<'a, ()> {
        trace!("TcpRuntime::new_active_connection(.., {:?})", cxnid);
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            trace!(
                "TcpRuntime::new_active_connection(.., {:?})::coroutine",
                cxnid
            );

            let (cxn, rt) = {
                let mut state = state.borrow_mut();
                let rt = state.rt.clone();
                (state.new_connection(cxnid.clone(), rt.clone())?, rt)
            };

            let options = rt.options();
            let retries = options.tcp.handshake_retries;
            let timeout = options.tcp.handshake_timeout;
            let ack_segment = r#await!(
                TcpPeerState::handshake(state.clone(), cxn.clone()),
                rt.now(),
                Retry::binary_exponential(timeout, retries)
            )?;

            let remote_isn = ack_segment.seq_num;

            let (bytes, handle) = {
                let mut cxn = cxn.borrow_mut();
                cxn.set_remote_isn(remote_isn);
                cxn.negotiate_mss(ack_segment.mss)?;
                cxn.set_remote_receive_window_size(ack_segment.window_size)?;
                cxn.incr_seq_num();
                let segment = TcpSegment::default().connection(&cxn);
                (Rc::new(RefCell::new(segment.encode())), cxn.get_handle())
            };

            r#await!(TcpPeerState::cast(state.clone(), bytes), rt.now())?;
            info!(
                "{}: connection established (handle = {})",
                options.my_ipv4_addr, handle
            );
            CoroutineOk(())
        })
    }

    fn new_passive_connection(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        syn_segment: TcpSegment,
    ) -> Future<'a, ()> {
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            let (cxn, rt) = {
                let mut state = state.borrow_mut();
                let rt = state.rt.clone();
                let options = rt.options();

                assert!(syn_segment.syn && !syn_segment.ack);
                let local_port = syn_segment.dest_port.unwrap();
                assert!(state.open_ports.contains(&local_port));

                let remote_ipv4_addr = syn_segment.src_ipv4_addr.unwrap();
                let remote_port = syn_segment.src_port.unwrap();
                let cxnid = Rc::new(TcpConnectionId {
                    local: ipv4::Endpoint::new(
                        options.my_ipv4_addr,
                        local_port,
                    ),
                    remote: ipv4::Endpoint::new(remote_ipv4_addr, remote_port),
                });

                let cxn = state.new_connection(cxnid, rt.clone())?;
                {
                    let mut cxn = cxn.borrow_mut();
                    cxn.negotiate_mss(syn_segment.mss)?;
                    cxn.set_remote_isn(syn_segment.seq_num);
                }

                (cxn, rt)
            };

            let options = rt.options();
            let ack_segment = r#await!(
                TcpPeerState::handshake(state.clone(), cxn.clone()),
                rt.now(),
                Retry::binary_exponential(
                    options.tcp.handshake_timeout,
                    options.tcp.handshake_retries
                )
            )?;

            {
                // SYN+ACK packet has been acknowledged; increment the sequence
                // number and notify the caller.
                let mut cxn = cxn.borrow_mut();
                cxn.set_remote_receive_window_size(ack_segment.window_size)?;
                cxn.incr_seq_num();
                rt.emit_event(Event::IncomingTcpConnection(cxn.get_handle()));
            }

            r#await!(
                TcpPeerState::on_connection_established(state, cxn),
                rt.now()
            )?;

            CoroutineOk(())
        })
    }

    fn handshake(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        cxn: Rc<RefCell<TcpConnection<'a>>>,
    ) -> Future<'a, Rc<TcpSegment>> {
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            trace!("TcpRuntime::handshake()");
            let rt = state.borrow().rt.clone();
            let (bytes, ack_was_sent, expected_ack_num) = {
                let cxn = cxn.borrow();
                let segment = TcpSegment::default()
                    .connection(&cxn)
                    .mss(cxn.get_mss())
                    .syn();
                let ack_was_sent = segment.ack;
                let expected_ack_num = segment.seq_num + Wrapping(1);
                let bytes = Rc::new(RefCell::new(segment.encode()));
                (bytes, ack_was_sent, expected_ack_num)
            };

            r#await!(TcpPeerState::cast(state.clone(), bytes), rt.now())?;

            loop {
                if yield_until!(
                    !cxn.borrow().receive_queue().is_empty(),
                    state.rt.now()
                ) {
                    let segment = cxn
                        .borrow_mut()
                        .receive_queue_mut()
                        .pop_front()
                        .unwrap();
                    debug!("popped a segment for handshake: {:?}", segment);
                    if segment.rst {
                        return Err(Fail::ConnectionRefused {});
                    }

                    if segment.ack
                        && ack_was_sent != segment.syn
                        && segment.ack_num == expected_ack_num
                    {
                        return CoroutineOk(Rc::new(segment));
                    }
                }
            }
        })
    }

    fn close_connection(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        cxnid: Rc<TcpConnectionId>,
        error: Option<Fail>,
        notify: bool,
    ) -> Future<'a, ()> {
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            let (rst_segment, cxn_handle, rt) = {
                let mut state = state.borrow_mut();
                let cxn = if let Some(cxn) = state.connections.remove(&cxnid) {
                    cxn
                } else {
                    return Err(Fail::ResourceNotFound {
                        details: "unrecognized connection ID",
                    });
                };

                let cxn = cxn.borrow();
                let rst_segment = TcpSegment::default().connection(&cxn).rst();
                let local_port = cxnid.local.port();
                if local_port.is_private() {
                    state.release_private_port(local_port)
                }

                (rst_segment, cxn.get_handle(), state.rt.clone())
            };

            if let Some(e) = error {
                if notify {
                    rt.emit_event(Event::TcpConnectionClosed {
                        handle: cxn_handle,
                        error: Some(e),
                    });
                }

                let bytes = Rc::new(RefCell::new(rst_segment.encode()));
                let _ = r#await!(TcpPeerState::cast(state, bytes), rt.now());
            } else if notify {
                rt.emit_event(Event::TcpConnectionClosed {
                    handle: cxn_handle,
                    error: None,
                });
            }

            CoroutineOk(())
        })
    }

    pub fn on_connection_established(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        cxn: Rc<RefCell<TcpConnection<'a>>>,
    ) -> Future<'a, ()> {
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            trace!("TcpRuntime::on_connection_established(...)::coroutine",);

            let (cxnid, rt) = {
                let state = state.borrow();
                (cxn.borrow().get_id().clone(), state.rt.clone())
            };

            let error = match r#await!(
                TcpPeerState::main_connection_loop(state.clone(), cxn.clone()),
                rt.now()
            ) {
                Ok(()) => None,
                Err(e) => Some(e),
            };

            r#await!(
                TcpPeerState::close_connection(state, cxnid, error, true),
                rt.now()
            )?;

            CoroutineOk(())
        })
    }

    pub fn main_connection_loop(
        state: Rc<RefCell<TcpPeerState<'a>>>,
        cxn: Rc<RefCell<TcpConnection<'a>>>,
    ) -> Future<'a, ()> {
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            trace!("TcpRuntime::main_connection_loop(...)::coroutine",);

            let rt = state.borrow().rt.clone();
            let options = rt.options();
            let mut ack_owed_since = None;
            loop {
                {
                    let mut cxn = cxn.borrow_mut();
                    while let Some(segment) =
                        cxn.receive_queue_mut().pop_front()
                    {
                        if segment.rst {
                            return Err(Fail::ConnectionAborted {});
                        }

                        // if there's a payload, we need to acknowledge it at
                        // some point. we set a timer if it hasn't already been
                        // set.
                        if ack_owed_since.is_none()
                            && !segment.payload.is_empty()
                        {
                            ack_owed_since = Some(rt.now());
                            /*debug!(
                                "{}: ack_owed_since = {:?}",
                                options.my_ipv4_addr, ack_owed_since
                            );*/
                        }

                        match cxn.receive(segment) {
                            Ok(()) => (),
                            Err(Fail::Ignored { details }) => {
                                warn!("TCP segment ignored: {}", details)
                            }
                            e => e?,
                        }
                    }

                    cxn.enqueue_retransmissions()?;
                }

                loop {
                    let segment =
                        cxn.borrow_mut().try_get_next_transmittable_segment();
                    if let Some(segment) = segment {
                        ack_owed_since = None;
                        r#await!(
                            TcpPeerState::cast(state.clone(), segment),
                            rt.now()
                        )?;
                    } else {
                        break;
                    }
                }

                if let Some(timestamp) = ack_owed_since {
                    debug!(
                        "{}: ack_owed_since = {:?} ({:?})",
                        options.my_ipv4_addr,
                        ack_owed_since,
                        rt.now() - timestamp
                    );
                    debug!(
                        "{}: options.tcp.trailing_ack_delay() = {:?}",
                        options.my_ipv4_addr, options.tcp.trailing_ack_delay
                    );
                    if rt.now() - timestamp > options.tcp.trailing_ack_delay {
                        debug!(
                            "{}: delayed ACK timer has expired; sending pure \
                             ACK...",
                            options.my_ipv4_addr,
                        );
                        let pure_ack =
                            TcpSegment::default().connection(&cxn.borrow());
                        let bytes = Rc::new(RefCell::new(pure_ack.encode()));
                        r#await!(
                            TcpPeerState::cast(state.clone(), bytes),
                            rt.now()
                        )?;

                        ack_owed_since = None;
                    }
                } else {
                    //debug!("{}: ack_owed_since = None",
                    // options.my_ipv4_addr)
                }

                yield None;
            }
        })
    }
}

pub struct TcpPeer<'a> {
    state: Rc<RefCell<TcpPeerState<'a>>>,
}

impl<'a> TcpPeer<'a> {
    pub fn new(rt: Runtime<'a>, arp: arp::Peer<'a>) -> Self {
        TcpPeer {
            state: Rc::new(RefCell::new(TcpPeerState::new(rt, arp))),
        }
    }

    pub fn receive(&mut self, datagram: ipv4::Datagram<'_>) -> Result<()> {
        trace!("TcpPeer::receive(...)");
        let decoder = TcpSegmentDecoder::try_from(datagram)?;
        let segment = TcpSegment::try_from(decoder)?;
        info!(
            "{} received segment: {:?}",
            self.state.borrow().rt.options().my_ipv4_addr,
            segment
        );
        let local_ipv4_addr = segment.dest_ipv4_addr.unwrap();
        // i haven't yet seen anything that explicitly disallows categories of
        // IP addresses but it seems sensible to drop datagrams where the
        // source address does not really support a connection.
        let remote_ipv4_addr =
            segment.src_ipv4_addr.ok_or(Fail::Malformed {
                details: "source IPv4 address is missing",
            })?;
        if remote_ipv4_addr.is_broadcast()
            || remote_ipv4_addr.is_multicast()
            || remote_ipv4_addr.is_unspecified()
        {
            return Err(Fail::Malformed {
                details: "only unicast addresses are supported by TCP",
            });
        }

        let local_port = segment.dest_port.ok_or(Fail::Malformed {
            details: "destination port is zero",
        })?;

        let remote_port = segment.src_port.ok_or(Fail::Malformed {
            details: "source port is zero",
        })?;

        if self.state.borrow().open_ports.contains(&local_port) {
            if segment.syn && !segment.ack && !segment.rst {
                let background_work =
                    self.state.borrow().background_work.clone();
                background_work.borrow_mut().add(
                    TcpPeerState::new_passive_connection(
                        self.state.clone(),
                        segment,
                    ),
                );
                return Ok(());
            }

            let cxnid = TcpConnectionId {
                local: ipv4::Endpoint::new(local_ipv4_addr, local_port),
                remote: ipv4::Endpoint::new(remote_ipv4_addr, remote_port),
            };

            if let Some(cxn) = self.state.borrow_mut().connections.get(&cxnid)
            {
                cxn.borrow_mut().receive_queue_mut().push_back(segment);
                return Ok(());
            } else {
                return Err(Fail::ResourceNotFound {
                    details: "unrecognized connection ID",
                });
            }
        }

        // `local_port` is not open; send the appropriate RST segment.
        debug!("local port {} is not open; sending RST.", local_port);
        let mut ack_num =
            segment.seq_num + Wrapping(u32::try_from(segment.payload.len())?);
        // from [TCP/IP Illustrated](https://learning.oreilly.com/library/view/TCP_IP+Illustrated,+Volume+1:+The+Protocols/9780132808200/ch13.html#ch13):
        // > Although there is no data in the arriving segment, the SYN
        // > bit logically occupies 1 byte of sequence number space;
        // > therefore, in this example the ACK number in the reset
        // > segment is set to the ISN, plus the data length (0), plus 1
        // > for the SYN bit.
        if segment.syn {
            ack_num += Wrapping(1);
        }

        let segment = TcpSegment::default()
            .dest_ipv4_addr(remote_ipv4_addr)
            .dest_port(remote_port)
            .src_port(local_port)
            .ack(ack_num)
            .rst();
        let bytes = Rc::new(RefCell::new(segment.encode()));
        let background_work = self.state.borrow().background_work.clone();
        background_work
            .borrow_mut()
            .add(TcpPeerState::cast(self.state.clone(), bytes));
        Ok(())
    }

    pub fn connect(
        &self,
        remote_endpoint: ipv4::Endpoint,
    ) -> Future<'a, TcpConnectionHandle> {
        trace!("TcpPeer::connect({:?})", remote_endpoint);
        let state = self.state.clone();
        let rt = state.borrow().rt.clone();
        rt.start_coroutine(move || {
            trace!("TcpPeer::connect({:?})::coroutine", remote_endpoint);
            let (rt, cxnid) = {
                let mut state = state.borrow_mut();
                let rt = state.rt.clone();
                let options = rt.options();
                let cxnid = Rc::new(TcpConnectionId {
                    local: ipv4::Endpoint::new(
                        options.my_ipv4_addr,
                        state.acquire_private_port()?,
                    ),
                    remote: remote_endpoint,
                });

                (rt, cxnid)
            };

            let error = match r#await!(
                TcpPeerState::new_active_connection(
                    state.clone(),
                    cxnid.clone()
                ),
                rt.now()
            ) {
                Ok(()) => {
                    let cxn = state
                        .borrow_mut()
                        .connections
                        .get(&cxnid)
                        .unwrap()
                        .clone();
                    let handle = cxn.borrow().get_handle();
                    // we cannot insert a new future into `background_work`
                    // because it may be borrowed already (e.g.
                    // `advance_clock()`).
                    let background_queue =
                        state.borrow().background_queue.clone();
                    background_queue.borrow_mut().push_back(
                        TcpPeerState::on_connection_established(
                            state.clone(),
                            cxn,
                        ),
                    );

                    return CoroutineOk(handle);
                }
                Err(e) => e,
            };

            let _ = r#await!(
                TcpPeerState::close_connection(
                    state.clone(),
                    cxnid,
                    Some(error.clone()),
                    false
                ),
                rt.now()
            );

            Err(error)
        })
    }

    pub fn listen(&mut self, port: ip::Port) -> Result<()> {
        let mut state = self.state.borrow_mut();
        if state.open_ports.contains(&port) {
            return Err(Fail::ResourceBusy {
                details: "port already in use",
            });
        }

        assert!(state.open_ports.insert(port));
        Ok(())
    }

    pub fn write(
        &self,
        handle: TcpConnectionHandle,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let state = self.state.borrow();
        let mut cxn = state.get_connection_given_handle(handle)?.borrow_mut();
        cxn.write(bytes);
        Ok(())
    }

    pub fn peek(&self, handle: TcpConnectionHandle) -> Result<Rc<Vec<u8>>> {
        let state = self.state.borrow();
        let cxn = state.get_connection_given_handle(handle)?.borrow();
        if let Some(bytes) = cxn.peek() {
            Ok(bytes.clone())
        } else {
            Err(Fail::ResourceExhausted {
                details: "The unread queue is empty.",
            })
        }
    }

    pub fn read(
        &mut self,
        handle: TcpConnectionHandle,
    ) -> Result<Rc<Vec<u8>>> {
        let state = self.state.borrow();
        let mut cxn = state.get_connection_given_handle(handle)?.borrow_mut();
        if let Some(bytes) = cxn.read() {
            Ok(bytes)
        } else {
            Err(Fail::ResourceExhausted {
                details: "The unread queue is empty.",
            })
        }
    }

    pub fn get_mss(&self, handle: TcpConnectionHandle) -> Result<usize> {
        let state = self.state.borrow();
        let cxn = state.get_connection_given_handle(handle)?.borrow();
        Ok(cxn.get_mss())
    }

    pub fn get_rto(&self, handle: TcpConnectionHandle) -> Result<Duration> {
        let state = self.state.borrow();
        let cxn = state.get_connection_given_handle(handle)?.borrow();
        Ok(cxn.get_rto())
    }

    pub fn advance_clock(&self, now: Instant) {
        let background_work = self.state.borrow().background_work.clone();
        let mut background_work = background_work.borrow_mut();

        {
            let background_queue =
                self.state.borrow().background_queue.clone();
            let mut background_queue = background_queue.borrow_mut();
            while let Some(fut) = background_queue.pop_front() {
                background_work.add(fut);
            }
        }

        if let Some(result) = background_work.poll(now) {
            match result {
                Ok(_) => (),
                Err(e) => warn!("background coroutine failed: {:?}", e),
            }
        }
    }

    pub fn get_connection_id(
        &self,
        handle: TcpConnectionHandle,
    ) -> Result<Rc<TcpConnectionId>> {
        let state = self.state.borrow();
        let cxn = state.get_connection_given_handle(handle)?.borrow();
        Ok(cxn.get_id().clone())
    }
}
