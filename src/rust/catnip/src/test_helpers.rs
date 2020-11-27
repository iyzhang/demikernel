// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use crate::{
    engine::Engine,
    protocols::{
        arp,
        ethernet2::MacAddress,
        tcp,
    },
    runtime::{
        PacketBuf,
        Runtime,
    },
    scheduler::{
        Operation,
        Scheduler,
        SchedulerHandle,
    },
    sync::{
        Bytes,
        BytesMut,
    },
    timer::{
        Timer,
        TimerRc,
    },
};
use futures::{
    FutureExt,
};
use rand::{
    distributions::{
        Distribution,
        Standard,
    },
    rngs::SmallRng,
    Rng,
    SeedableRng,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    net::Ipv4Addr,
    rc::Rc,
    time::{
        Duration,
        Instant,
    },
};

pub const RECEIVE_WINDOW_SIZE: usize = 1024;
pub const ALICE_MAC: MacAddress = MacAddress::new([0x12, 0x23, 0x45, 0x67, 0x89, 0xab]);
pub const ALICE_IPV4: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 1);
pub const BOB_MAC: MacAddress = MacAddress::new([0xab, 0x89, 0x67, 0x45, 0x23, 0x12]);
pub const BOB_IPV4: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 2);
pub const CARRIE_MAC: MacAddress = MacAddress::new([0xef, 0xcd, 0xab, 0x89, 0x67, 0x45]);
pub const CARRIE_IPV4: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 3);

pub type TestEngine = Engine<TestRuntime>;

#[derive(Clone)]
pub struct TestRuntime {
    inner: Rc<RefCell<Inner>>,
    scheduler: Scheduler<Operation<TestRuntime>>,
}

impl TestRuntime {
    pub fn new(
        name: &'static str,
        now: Instant,
        link_addr: MacAddress,
        ipv4_addr: Ipv4Addr,
    ) -> Self {
        let mut arp_options = arp::Options::default();
        arp_options.retry_count = 2;
        arp_options.cache_ttl = Duration::from_secs(600);
        arp_options.request_timeout = Duration::from_secs(1);
        arp_options.initial_values.insert(ALICE_MAC, ALICE_IPV4);
        arp_options.initial_values.insert(BOB_MAC, BOB_IPV4);
        arp_options.initial_values.insert(CARRIE_MAC, CARRIE_IPV4);

        let inner = Inner {
            name,
            timer: TimerRc(Rc::new(Timer::new(now))),
            rng: SmallRng::from_seed([0; 16]),
            incoming: VecDeque::new(),
            outgoing: VecDeque::new(),
            link_addr,
            ipv4_addr,
            tcp_options: tcp::Options::default(),
            arp_options,
        };
        Self {
            inner: Rc::new(RefCell::new(inner)),
            scheduler: Scheduler::new(),
        }
    }

    pub fn pop_frame(&self) -> Bytes {
        self.inner.borrow_mut().outgoing.pop_front().unwrap()
    }

    pub fn push_frame(&self, buf: Bytes) {
        self.inner.borrow_mut().incoming.push_back(buf);
    }

    pub fn poll_scheduler(&self) {
        // let mut ctx = Context::from_waker(noop_waker_ref());
        self.scheduler.poll();
    }
}

struct Inner {
    #[allow(unused)]
    name: &'static str,
    timer: TimerRc,
    rng: SmallRng,
    incoming: VecDeque<Bytes>,
    outgoing: VecDeque<Bytes>,

    link_addr: MacAddress,
    ipv4_addr: Ipv4Addr,
    tcp_options: tcp::Options,
    arp_options: arp::Options,
}

impl Runtime for TestRuntime {
    type WaitFuture = crate::timer::WaitFuture<TimerRc>;

    fn transmit(&self, pkt: impl PacketBuf) {
        let size = pkt.compute_size();
        let mut buf = BytesMut::zeroed(size);
        pkt.serialize(&mut buf[..]);
        self.inner.borrow_mut().outgoing.push_back(buf.freeze());
    }

    fn receive(&self) -> Option<Bytes> {
        self.inner.borrow_mut().incoming.pop_front()
    }

    fn scheduler(&self) -> &Scheduler<Operation<Self>> {
        &self.scheduler
    }

    fn local_link_addr(&self) -> MacAddress {
        self.inner.borrow().link_addr.clone()
    }

    fn local_ipv4_addr(&self) -> Ipv4Addr {
        self.inner.borrow().ipv4_addr.clone()
    }

    fn tcp_options(&self) -> tcp::Options {
        self.inner.borrow().tcp_options.clone()
    }

    fn arp_options(&self) -> arp::Options {
        self.inner.borrow().arp_options.clone()
    }

    fn advance_clock(&self, now: Instant) {
        self.inner.borrow_mut().timer.0.advance_clock(now);
    }

    fn wait(&self, duration: Duration) -> Self::WaitFuture {
        let inner = self.inner.borrow_mut();
        let now = inner.timer.0.now();
        inner
            .timer
            .0
            .wait_until(inner.timer.clone(), now + duration)
    }

    fn wait_until(&self, when: Instant) -> Self::WaitFuture {
        let inner = self.inner.borrow_mut();
        inner.timer.0.wait_until(inner.timer.clone(), when)
    }

    fn now(&self) -> Instant {
        self.inner.borrow().timer.0.now()
    }

    fn rng_gen<T>(&self) -> T
    where
        Standard: Distribution<T>,
    {
        let mut inner = self.inner.borrow_mut();
        inner.rng.gen()
    }

    fn spawn<F: Future<Output = ()> + 'static>(&self, future: F) -> SchedulerHandle {
        self.scheduler
            .insert(Operation::Background(future.boxed_local()))
    }
}

pub fn new_alice(now: Instant) -> Engine<TestRuntime> {
    let rt = TestRuntime::new("alice", now, ALICE_MAC, ALICE_IPV4);
    Engine::new(rt).unwrap()
}

pub fn new_bob(now: Instant) -> Engine<TestRuntime> {
    let rt = TestRuntime::new("bob", now, BOB_MAC, BOB_IPV4);
    Engine::new(rt).unwrap()
}

pub fn new_carrie(now: Instant) -> Engine<TestRuntime> {
    let rt = TestRuntime::new("carrie", now, CARRIE_MAC, CARRIE_IPV4);
    Engine::new(rt).unwrap()
}
