use dpdk_rs::{
    rte_eth_dev,
    rte_eth_devices,
    rte_mbuf,
    rte_mempool,
    rte_pktmbuf_free,
    rte_pktmbuf_alloc,
    rte_eth_tx_burst,
    rte_eth_rx_burst,
    rte_pktmbuf_chain,
};
use crate::memory::{MemoryManager, DPDKBuf, Mbuf};
use arrayvec::ArrayVec;
use catnip::{
    interop::{dmtr_sgarray_t, dmtr_sgaseg_t},
    protocols::{
        arp,
        ethernet2::frame::MIN_PAYLOAD_SIZE,
        ethernet2::MacAddress,
        tcp,
        udp,
    },
    runtime::{
        PacketBuf,
        Runtime,
        RECEIVE_BATCH_SIZE,
    },
    runtime::RuntimeBuf,
    scheduler::{
        Operation,
        Scheduler,
        SchedulerHandle,
    },
    collections::bytes::{
        Bytes,
        BytesMut,
    },
    timer::{
        Timer,
        TimerPtr,
        WaitFuture,
    },
};
use futures::FutureExt;
use std::collections::HashMap;
use rand::{
    distributions::{
        Distribution,
        Standard,
    },
    rngs::SmallRng,
    seq::SliceRandom,
    Rng,
    SeedableRng,
};
use std::{
    cell::RefCell,
    future::Future,
    mem,
    mem::MaybeUninit,
    net::Ipv4Addr,
    ptr,
    rc::Rc,
    slice,
    time::{
        Duration,
        Instant,
    },
};

#[derive(Clone)]
pub struct TimerRc(Rc<Timer<TimerRc>>);

impl TimerPtr for TimerRc {
    fn timer(&self) -> &Timer<Self> {
        &*self.0
    }
}

#[derive(Clone)]
pub struct DPDKRuntime {
    inner: Rc<RefCell<Inner>>,
    scheduler: Scheduler<Operation<Self>>,
}

impl DPDKRuntime {
    pub fn new(
        link_addr: MacAddress,
        ipv4_addr: Ipv4Addr,
        dpdk_port_id: u16,
        memory_manager: MemoryManager,
        arp_table: HashMap<Ipv4Addr, MacAddress>,
        disable_arp: bool,
        mss: usize,
        tcp_checksum_offload: bool,
        udp_checksum_offload: bool,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let rng = SmallRng::from_rng(&mut rng).expect("Failed to initialize RNG");
        let now = Instant::now();

        let arp_options = arp::Options::new(
            Duration::from_secs(15),
            Duration::from_secs(20),
            5,
            arp_table,
            disable_arp,
        );

        let mut tcp_options = tcp::Options::default();
        tcp_options.advertised_mss = mss;
        tcp_options.window_scale = 5;
        tcp_options.receive_window_size = 0xffff;
        tcp_options.tx_checksum_offload = tcp_checksum_offload;
        tcp_options.rx_checksum_offload = tcp_checksum_offload;

        let mut udp_options = udp::Options::new(
            udp_checksum_offload,
            udp_checksum_offload,
        );

        let inner = Inner {
            timer: TimerRc(Rc::new(Timer::new(now))),
            link_addr,
            ipv4_addr,
            rng,
            arp_options,
            tcp_options,
            udp_options,

            dpdk_port_id,
            memory_manager,
        };
        Self {
            inner: Rc::new(RefCell::new(inner)),
            scheduler: Scheduler::new(),
        }
    }

    pub fn alloc_body_mbuf(&self) -> Mbuf {
        self.inner.borrow().memory_manager.alloc_body_mbuf()
    }

    pub fn port_id(&self) -> u16 {
        self.inner.borrow().dpdk_port_id
    }

    pub fn memory_manager(&self) -> MemoryManager {
        self.inner.borrow().memory_manager.clone()
    }
}

struct Inner {
    timer: TimerRc,
    memory_manager: MemoryManager,
    link_addr: MacAddress,
    ipv4_addr: Ipv4Addr,
    rng: SmallRng,
    arp_options: arp::Options,
    tcp_options: tcp::Options<DPDKRuntime>,
    udp_options: udp::Options,

    dpdk_port_id: u16,
}

impl Runtime for DPDKRuntime {
    type WaitFuture = WaitFuture<TimerRc>;
    type Buf = DPDKBuf;

    fn into_sgarray(&self, buf: Self::Buf) -> dmtr_sgarray_t {
        self.inner.borrow().memory_manager.into_sgarray(buf)
    }

    fn alloc_sgarray(&self, size: usize) -> dmtr_sgarray_t {
        self.inner.borrow().memory_manager.alloc_sgarray(size)
    }

    fn free_sgarray(&self, sga: dmtr_sgarray_t) {
        self.inner.borrow().memory_manager.free_sgarray(sga)
    }

    fn clone_sgarray(&self, sga: &dmtr_sgarray_t) -> Self::Buf {
        self.inner.borrow().memory_manager.clone_sgarray(sga)
    }

    fn transmit(&self, buf: impl PacketBuf<DPDKBuf>) {
        // Alloc header mbuf, check header size.
        // Serialize header.
        // Decide if we can inline the data --
        //   1) How much space is left?
        //   2) Is the body small enough?
        // If we can inline, copy and return.
        // If we can't inline...
        //   1) See if the body is managed => take
        //   2) Not managed => alloc body
        // Chain body buffer.

        // First, allocate a header mbuf and write the header into it.
        let inner = self.inner.borrow_mut();
        let mut header_mbuf = inner.memory_manager.alloc_header_mbuf();
        let header_size = buf.header_size();
        assert!(header_size <= header_mbuf.len());
        buf.write_header(unsafe { &mut header_mbuf.slice_mut()[..header_size] });

        if let Some(body) = buf.take_body() {
            // Next, see how much space we have remaining and inline the body if we have room.
            let inline_space = header_mbuf.len() - header_size;

            // Chain a buffer
            if body.len() > inline_space {
                assert!(header_size + body.len() >= MIN_PAYLOAD_SIZE);

                // We're only using the header_mbuf for, well, the header.
                header_mbuf.trim(header_mbuf.len() - header_size);

                let body_mbuf = match body {
                    DPDKBuf::Managed(mbuf) => mbuf,
                    DPDKBuf::External(bytes) => {
                        let mut mbuf = inner.memory_manager.alloc_body_mbuf();
                        assert!(mbuf.len() >= bytes.len());
                        unsafe { mbuf.slice_mut()[..bytes.len()].copy_from_slice(&bytes[..]) };
                        mbuf.trim(mbuf.len() - bytes.len());
                        mbuf
                    },
                };
                unsafe {
                    assert_eq!(rte_pktmbuf_chain(header_mbuf.ptr(), body_mbuf.into_raw()), 0);
                }
                let mut header_mbuf_ptr = header_mbuf.into_raw();
                let num_sent = unsafe {
                    rte_eth_tx_burst(inner.dpdk_port_id, 0, &mut header_mbuf_ptr, 1)
                };
                assert_eq!(num_sent, 1);
            }
            // Otherwise, write in the inline space.
            else {
                let body_buf = unsafe { &mut header_mbuf.slice_mut()[header_size..(header_size + body.len())] };
                body_buf.copy_from_slice(&body[..]);

                if header_size + body.len() < MIN_PAYLOAD_SIZE {
                    let padding_bytes = MIN_PAYLOAD_SIZE - (header_size + body.len());
                    let padding_buf = unsafe {
                        &mut header_mbuf.slice_mut()[(header_size + body.len())..][..padding_bytes]
                    };
                    for byte in padding_buf {
                        *byte = 0;
                    }
                }

                let frame_size = std::cmp::max(header_size + body.len(), MIN_PAYLOAD_SIZE);
                header_mbuf.trim(header_mbuf.len() - frame_size);

                let mut header_mbuf_ptr = header_mbuf.into_raw();
                let num_sent = unsafe {
                    rte_eth_tx_burst(inner.dpdk_port_id, 0, &mut header_mbuf_ptr, 1)
                };
                assert_eq!(num_sent, 1);
            }
        }
        // No body on our packet, just send the headers.
        else {
            if header_size < MIN_PAYLOAD_SIZE {
                let padding_bytes = MIN_PAYLOAD_SIZE - header_size;
                let padding_buf = unsafe {
                    &mut header_mbuf.slice_mut()[header_size..][..padding_bytes]
                };
                for byte in padding_buf {
                    *byte = 0;
                }
            }
            let frame_size = std::cmp::max(header_size, MIN_PAYLOAD_SIZE);
            header_mbuf.trim(header_mbuf.len() - frame_size);
            let mut header_mbuf_ptr = header_mbuf.into_raw();
            let num_sent = unsafe {
                rte_eth_tx_burst(inner.dpdk_port_id, 0, &mut header_mbuf_ptr, 1)
            };
            assert_eq!(num_sent, 1);
        }
    }

    fn receive(&self) -> ArrayVec<DPDKBuf, RECEIVE_BATCH_SIZE> {
        let mut inner = self.inner.borrow_mut();
        let mut out = ArrayVec::new();

        let mut packets: [*mut rte_mbuf; RECEIVE_BATCH_SIZE] = unsafe { mem::zeroed() };
        let nb_rx = unsafe {
            rte_eth_rx_burst(
                inner.dpdk_port_id,
                0,
                packets.as_mut_ptr(),
                RECEIVE_BATCH_SIZE as u16,
            )
        };
        assert!(nb_rx as usize <= RECEIVE_BATCH_SIZE);

        for &packet in &packets[..nb_rx as usize] {
            let mbuf = Mbuf {
                ptr: packet,
                mm: inner.memory_manager.clone(),
            };
            out.push(DPDKBuf::Managed(mbuf));
        }
        out
    }

    fn local_link_addr(&self) -> MacAddress {
        self.inner.borrow().link_addr.clone()
    }

    fn local_ipv4_addr(&self) -> Ipv4Addr {
        self.inner.borrow().ipv4_addr.clone()
    }

    fn tcp_options(&self) -> tcp::Options<Self> {
        self.inner.borrow().tcp_options.clone()
    }

    fn udp_options(&self) -> udp::Options {
        self.inner.borrow().udp_options.clone()
    }

    fn arp_options(&self) -> arp::Options {
        self.inner.borrow().arp_options.clone()
    }

    fn advance_clock(&self, now: Instant) {
        self.inner.borrow_mut().timer.0.advance_clock(now);
    }

    fn wait(&self, duration: Duration) -> Self::WaitFuture {
        let self_ = self.inner.borrow_mut();
        let now = self_.timer.0.now();
        self_
            .timer
            .0
            .wait_until(self_.timer.clone(), now + duration)
    }

    fn wait_until(&self, when: Instant) -> Self::WaitFuture {
        let self_ = self.inner.borrow_mut();
        self_.timer.0.wait_until(self_.timer.clone(), when)
    }

    fn now(&self) -> Instant {
        self.inner.borrow().timer.0.now()
    }

    fn rng_gen<T>(&self) -> T
    where
        Standard: Distribution<T>,
    {
        let mut self_ = self.inner.borrow_mut();
        self_.rng.gen()
    }

    fn rng_shuffle<T>(&self, slice: &mut [T]) {
        let mut inner = self.inner.borrow_mut();
        slice.shuffle(&mut inner.rng);
    }

    fn spawn<F: Future<Output = ()> + 'static>(&self, future: F) -> SchedulerHandle {
        self.scheduler
            .insert(Operation::Background(future.boxed_local()))
    }

    fn scheduler(&self) -> &Scheduler<Operation<Self>> {
        &self.scheduler
    }
}
