// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#![feature(try_blocks)]

use anyhow::Error;
use catnip::{libos::LibOS, operations::OperationResult};
use dpdk_rs::load_mlx_driver;
use must_let::must_let;
use std::env;

use anyhow::format_err;
use catnip::protocols::{ethernet2::MacAddress, ip::Port, ipv4::Endpoint};
use catnip_libos::memory::DPDKBuf;
use catnip_libos::runtime::DPDKRuntime;
use histogram::Histogram;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;
use yaml_rust::{Yaml, YamlLoader};

struct Latency {
    name: &'static str,
    samples: Vec<Duration>,
}

pub struct Recorder<'a> {
    start: Instant,
    latency: &'a mut Latency,
}

impl<'a> Drop for Recorder<'a> {
    fn drop(&mut self) {
        self.latency.samples.push(self.start.elapsed());
    }
}

impl Latency {
    pub fn new(name: &'static str, n: usize) -> Self {
        Self {
            name,
            samples: Vec::with_capacity(n),
        }
    }

    pub fn record(&mut self) -> Recorder<'_> {
        Recorder {
            start: Instant::now(),
            latency: self,
        }
    }

    pub fn print(self) {
        let mut h = Histogram::configure().precision(4).build().unwrap();

        // Drop the first 10% of samples.
        for s in &self.samples[(self.samples.len() / 10)..] {
            h.increment(s.as_nanos() as u64).unwrap();
        }
        println!(
            "rtt {}: {:?}",
            self.name,
            Duration::from_nanos(h.percentile(0.50).unwrap())
        );
    }
}

pub struct Stats {
    start: Instant,
    num_bytes: usize,
}

impl Stats {
    pub fn report_bytes(&mut self, n: usize) {
        self.num_bytes += n;
    }
}

pub fn run(niters: usize, label: &'static str, mut f: impl FnMut(&mut Stats)) {
    let throughput_gbps = |num_bytes: usize, duration: Duration| {
        let bps = (num_bytes as f64) / duration.as_secs_f64();
        bps / 1024. / 1024. / 1024. * 8.
    };
    let mut stats = Stats {
        start: Instant::now(),
        num_bytes: 0,
    };
    let mut latency = Latency::new(label, niters);
    for _ in 0..niters {
        let _s = latency.record();
        f(&mut stats);
    }
    let exp_duration = stats.start.elapsed();
    if stats.num_bytes > 0 {
        let throughput = throughput_gbps(stats.num_bytes, exp_duration);
        println!("Finished ({} samples, {} Gbps)", niters, throughput);
    }
    latency.print();
}

#[derive(Debug)]
pub struct Config {
    pub buffer_size: usize,
    pub config_obj: Yaml,
    pub mss: usize,
    pub strict: bool,
    pub udp_checksum_offload: bool,
}

impl Config {
    pub fn initialize() -> Result<(Self, DPDKRuntime), Error> {
        let config_path = env::args()
            .nth(1)
            .ok_or(format_err!("Config path is first argument"))?;
        let mut config_s = String::new();
        File::open(config_path)?.read_to_string(&mut config_s)?;
        let config = YamlLoader::load_from_str(&config_s)?;
        let config_obj = match &config[..] {
            &[ref c] => c,
            _ => Err(format_err!("Wrong number of config objects"))?,
        };
        let local_ipv4_addr: Ipv4Addr = config_obj["catnip"]["my_ipv4_addr"]
            .as_str()
            .ok_or_else(|| format_err!("Couldn't find my_ipv4_addr in config"))?
            .parse()?;
        if local_ipv4_addr.is_unspecified() || local_ipv4_addr.is_broadcast() {
            Err(format_err!("Invalid IPv4 address"))?;
        }

        let mut arp_table = HashMap::new();
        if let Some(arp_table_obj) = config_obj["catnip"]["arp_table"].as_hash() {
            for (k, v) in arp_table_obj {
                let link_addr_str = k
                    .as_str()
                    .ok_or_else(|| format_err!("Couldn't find ARP table link_addr in config"))?;
                let link_addr = MacAddress::parse_str(link_addr_str)?;
                let ipv4_addr: Ipv4Addr = v
                    .as_str()
                    .ok_or_else(|| format_err!("Couldn't find ARP table link_addr in config"))?
                    .parse()?;
                arp_table.insert(ipv4_addr, link_addr);
            }
        }
        let mut disable_arp = false;
        if let Some(arp_disabled) = config_obj["catnip"]["disable_arp"].as_bool() {
            disable_arp = arp_disabled;
        }

        let eal_init_args = match config_obj["dpdk"]["eal_init"] {
            Yaml::Array(ref arr) => arr
                .iter()
                .map(|a| {
                    a.as_str()
                        .ok_or_else(|| format_err!("Non string argument"))
                        .and_then(|s| CString::new(s).map_err(|e| e.into()))
                })
                .collect::<Result<Vec<_>, Error>>()?,
            _ => Err(format_err!("Malformed YAML config"))?,
        };

        let use_jumbo_frames = env::var("USE_JUMBO").is_ok();
        let mtu: u16 = env::var("MTU")?.parse()?;
        let mss: usize = env::var("MSS")?.parse()?;
        let udp_checksum_offload = env::var("UDP_CHECKSUM_OFFLOAD").is_ok();
        let strict = env::var("STRICT").is_ok();

        let buffer_size: usize = env::var("BUFFER_SIZE")?.parse()?;

        let runtime = catnip_libos::dpdk::initialize_dpdk(
            local_ipv4_addr,
            &eal_init_args,
            arp_table,
            disable_arp,
            use_jumbo_frames,
            mtu,
            mss,
            udp_checksum_offload,
            false,
        )?;

        let config = Self {
            buffer_size,
            mss,
            strict,
            udp_checksum_offload,
            config_obj: config_obj.clone(),
        };
        Ok((config, runtime))
    }

    pub fn addr(&self, k1: &str, k2: &str) -> Result<Endpoint, Error> {
        let addr = &self.config_obj[k1][k2];
        let host_s = addr["host"].as_str().ok_or(format_err!("Missing host"))?;
        let host = Ipv4Addr::from_str(host_s)?;
        let port_i = addr["port"].as_i64().ok_or(format_err!("Missing port"))?;
        let port = Port::try_from(port_i as u16)?;
        Ok(Endpoint::new(host, port))
    }

    pub fn body_buffers(&self, rt: &DPDKRuntime, fill_char: char) -> Vec<DPDKBuf> {
        let num_bufs = (self.buffer_size - 1) / self.mss + 1;
        let mut bufs = Vec::with_capacity(num_bufs);
        for i in 0..num_bufs {
            let start = i * self.mss;
            let end = std::cmp::min(start + self.mss, self.buffer_size);
            let len = end - start;

            let mut pktbuf = rt.alloc_body_mbuf();
            assert!(
                len <= pktbuf.len(),
                "len {} (from mss {}), pktbuf len {}",
                len,
                self.mss,
                pktbuf.len()
            );

            let pktbuf_slice = unsafe { pktbuf.slice_mut() };
            for j in 0..len {
                pktbuf_slice[j] = fill_char as u8;
            }
            drop(pktbuf_slice);
            pktbuf.trim(pktbuf.len() - len);
            bufs.push(DPDKBuf::Managed(pktbuf));
        }
        bufs
    }
}

fn main() -> Result<(), Error> {
    let niters: usize = env::var("NUM_ITERS")?.parse()?;
    let (config, runtime) = Config::initialize()?;
    let mut libos = LibOS::new(runtime)?;

    load_mlx_driver();

    if env::var("ECHO_SERVER").is_ok() {
        let bind_addr = config.addr("server", "bind")?;

        let sockfd = libos.socket(libc::AF_INET, libc::SOCK_STREAM, 0)?;
        libos.bind(sockfd, bind_addr)?;
        libos.listen(sockfd, 10)?;

        let qtoken = libos.accept(sockfd).unwrap();
        must_let!(let (_, OperationResult::Accept(fd)) = libos.wait2(qtoken));
        println!("Accepted connection!");

        let mut push_tokens = Vec::with_capacity(config.buffer_size / niters + 1);

        let (mut push, mut pop) = {
            let push = Latency::new("push", niters);
            let pop = Latency::new("pop", niters);
            (Some(push), Some(pop))
        };
        run(niters, "server", |stats| {
            let mut bytes_received = 0;
            while bytes_received < config.buffer_size {
                let r = pop.as_mut().map(|p| p.record());
                let qtoken = libos.pop(fd).unwrap();
                drop(r);
                must_let!(let (_, OperationResult::Pop(_, buf)) = libos.wait2(qtoken));
                bytes_received += buf.len();
                let r = push.as_mut().map(|p| p.record());
                let qtoken = libos.push2(fd, buf).unwrap();
                drop(r);
                push_tokens.push(qtoken);
            }
            assert_eq!(bytes_received, config.buffer_size);
            libos.wait_all_pushes(&mut push_tokens);
            assert_eq!(push_tokens.len(), 0);
            stats.report_bytes(bytes_received);
        });
        if let Some(push) = push {
            push.print();
        }
        if let Some(pop) = pop {
            pop.print();
        }
    } else if env::var("ECHO_CLIENT").is_ok() {
        let connect_addr = config.addr("client", "connect_to")?;

        let sockfd = libos.socket(libc::AF_INET, libc::SOCK_STREAM, 0)?;
        let qtoken = libos.connect(sockfd, connect_addr)?;
        must_let!(let (_, OperationResult::Connect) = libos.wait2(qtoken));

        let bufs = config.body_buffers(libos.rt(), 'a');
        let mut push_tokens = Vec::with_capacity(bufs.len());

        run(niters, "client", |stats| {
            assert!(push_tokens.is_empty());
            for b in &bufs {
                let qtoken = libos.push2(sockfd, b.clone()).unwrap();
                push_tokens.push(qtoken);
            }
            libos.wait_all_pushes(&mut push_tokens);

            let mut bytes_popped = 0;
            while bytes_popped < config.buffer_size {
                let qtoken = libos.pop(sockfd).unwrap();
                must_let!(let (_, OperationResult::Pop(_, popped_buf)) = libos.wait2(qtoken));
                bytes_popped += popped_buf.len();
                if config.strict {
                    for &byte in &popped_buf[..] {
                        assert_eq!(byte, 'a' as u8);
                    }
                }
            }
            assert_eq!(bytes_popped, config.buffer_size);
            stats.report_bytes(bytes_popped);
        });
    } else {
        panic!("Set either ECHO_SERVER or ECHO_CLIENT");
    }

    Ok(())
}
