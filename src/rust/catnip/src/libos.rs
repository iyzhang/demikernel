use crate::{
    engine::{
        Engine,
        Protocol,
    },
    fail::Fail,
    file_table::FileDescriptor,
    interop::{
        dmtr_qresult_t,
        dmtr_sgarray_t,
    },
    protocols::ipv4::Endpoint,
    runtime::Runtime,
    scheduler::{
        Operation,
        SchedulerHandle,
    },
    sync::BytesMut,
};
use libc::c_int;
use std::{
    slice,
    time::Instant,
};
use tracy_client::static_span;

const TIMER_RESOLUTION: usize = 64;

pub type QToken = u64;

pub struct LibOS<RT: Runtime> {
    engine: Engine<RT>,
    rt: RT,

    ts_iters: usize,
}

impl<RT: Runtime> LibOS<RT> {
    pub fn new(rt: RT) -> Result<Self, Fail> {
        let engine = Engine::new(rt.clone())?;
        Ok(Self {
            engine,
            rt,
            ts_iters: 0,
        })
    }

    pub fn rt(&self) -> &RT {
        &self.rt
    }

    pub fn socket(
        &mut self,
        domain: c_int,
        socket_type: c_int,
        protocol: c_int,
    ) -> Result<FileDescriptor, Fail> {
        if domain != libc::AF_INET {
            return Err(Fail::Invalid {
                details: "Invalid domain",
            });
        }
        let engine_protocol = match socket_type {
            libc::SOCK_STREAM => Protocol::Tcp,
            libc::SOCK_DGRAM => Protocol::Udp,
            _ => {
                return Err(Fail::Invalid {
                    details: "Invalid socket type",
                })
            },
        };
        if protocol != 0 {
            return Err(Fail::Invalid {
                details: "Invalid protocol",
            });
        }
        Ok(self.engine.socket(engine_protocol))
    }

    pub fn bind(&mut self, fd: FileDescriptor, endpoint: Endpoint) -> Result<(), Fail> {
        self.engine.bind(fd, endpoint)
    }

    pub fn listen(&mut self, fd: FileDescriptor, backlog: usize) -> Result<(), Fail> {
        self.engine.listen(fd, backlog)
    }

    pub fn accept(&mut self, fd: FileDescriptor) -> u64 {
        let future = self.engine.accept(fd);
        self.rt.scheduler().insert(future).into_raw()
    }

    pub fn connect(&mut self, fd: FileDescriptor, remote: Endpoint) -> QToken {
        let future = self.engine.connect(fd, remote);
        self.rt.scheduler().insert(future).into_raw()
    }

    pub fn close(&mut self, fd: FileDescriptor) -> Result<(), Fail> {
        self.engine.close(fd)
    }

    pub fn push(&mut self, fd: FileDescriptor, sga: &dmtr_sgarray_t) -> QToken {
        let _s = static_span!();
        let mut len = 0;
        for i in 0..sga.sga_numsegs as usize {
            len += sga.sga_segs[i].sgaseg_len;
        }
        let mut buf = BytesMut::zeroed(len as usize);
        let mut pos = 0;
        for i in 0..sga.sga_numsegs as usize {
            let seg = &sga.sga_segs[i];
            let seg_slice = unsafe {
                slice::from_raw_parts(seg.sgaseg_buf as *mut u8, seg.sgaseg_len as usize)
            };
            buf[pos..(pos + seg_slice.len())].copy_from_slice(seg_slice);
            pos += seg_slice.len();
        }
        let buf = buf.freeze();
        let future = self.engine.push(fd, buf);
        self.rt.scheduler().insert(future).into_raw()
    }

    pub fn pushto(&mut self, fd: FileDescriptor, sga: &dmtr_sgarray_t, to: Endpoint) -> QToken {
        let _s = static_span!();
        let mut len = 0;
        for i in 0..sga.sga_numsegs as usize {
            len += sga.sga_segs[i].sgaseg_len;
        }
        let mut buf = BytesMut::zeroed(len as usize);
        let mut pos = 0;
        for i in 0..sga.sga_numsegs as usize {
            let seg = &sga.sga_segs[i];
            let seg_slice = unsafe {
                slice::from_raw_parts(seg.sgaseg_buf as *mut u8, seg.sgaseg_len as usize)
            };
            buf[pos..(pos + seg_slice.len())].copy_from_slice(seg_slice);
            pos += seg_slice.len();
        }
        let buf = buf.freeze();
        let future = self.engine.pushto(fd, buf, to);
        self.rt.scheduler().insert(future).into_raw()
    }

    pub fn drop_qtoken(&mut self, qt: QToken) {
        drop(self.rt.scheduler().from_raw_handle(qt).unwrap());
    }

    pub fn pop(&mut self, fd: FileDescriptor) -> QToken {
        let _s = static_span!();
        let future = self.engine.pop(fd);
        self.rt.scheduler().insert(future).into_raw()
    }

    // If this returns a result, `qt` is no longer valid.
    pub fn poll(&mut self, qt: QToken) -> Option<dmtr_qresult_t> {
        let handle = self.rt.scheduler().from_raw_handle(qt).unwrap();
        if !handle.has_completed() {
            handle.into_raw();
            return None;
        }
        Some(self.take_operation(handle, qt))
    }

    pub fn wait(&mut self, qt: QToken) -> dmtr_qresult_t {
        let handle = self.rt.scheduler().from_raw_handle(qt).unwrap();
        loop {
            self.poll_bg_work();
            if handle.has_completed() {
                return self.take_operation(handle, qt);
            }
        }
    }

    pub fn wait_any(&mut self, qts: &[QToken]) -> (usize, dmtr_qresult_t) {
        let _s = static_span!();
        loop {
            self.poll_bg_work();
            for (i, &qt) in qts.iter().enumerate() {
                let handle = self.rt.scheduler().from_raw_handle(qt).unwrap();
                if handle.has_completed() {
                    return (i, self.take_operation(handle, qt));
                }
                handle.into_raw();
            }
        }
    }

    fn take_operation(&mut self, handle: SchedulerHandle, qt: QToken) -> dmtr_qresult_t {
        let (qd, r) = match self.rt.scheduler().take(handle) {
            Operation::Tcp(f) => f.expect_result(),
            Operation::Udp(f) => f.expect_result(),
            Operation::Background(..) => panic!("Polled background operation"),
        };
        dmtr_qresult_t::pack(r, qd, qt)
    }

    fn poll_bg_work(&mut self) {
        let _s = static_span!();
        self.rt.scheduler().poll();
        while let Some(pkt) = self.rt.receive() {
            if let Err(e) = self.engine.receive(pkt) {
                warn!("Dropped packet: {:?}", e);
            }
        }
        if self.ts_iters == 0 {
            let _t = static_span!("advance_clock");
            self.rt.advance_clock(Instant::now());
        }
        self.ts_iters = (self.ts_iters + 1) % TIMER_RESOLUTION;
    }
}
