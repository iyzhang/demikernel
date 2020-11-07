#![allow(non_camel_case_types, unused)]
#![feature(maybe_uninit_uninit_array)]
#![feature(try_blocks)]

use catnip::{
    file_table::FileDescriptor,
    interop::{
        dmtr_qresult_t,
        dmtr_qtoken_t,
        dmtr_sgarray_t,
    },
    libos::LibOS,
    logging,
    protocols::{
        ip,
        ipv4,
        ethernet2::MacAddress,
    },
    runtime::Runtime,
};
use clap::{
    App,
    Arg,
};
use libc::{
    c_char,
    c_int,
    sockaddr,
    socklen_t,
};
use hashbrown::HashMap;
use std::{
    cell::RefCell,
    convert::TryFrom,
    ffi::{
        CStr,
        CString,
    },
    fs::File,
    io::Read,
    mem,
    net::Ipv4Addr,
    slice,
};
use yaml_rust::{
    Yaml,
    YamlLoader,
};

mod bindings;
mod dpdk;
mod runtime;

use crate::runtime::DPDKRuntime;
use anyhow::{
    format_err,
    Error,
};

thread_local! {
    static LIBOS: RefCell<Option<LibOS<DPDKRuntime>>> = RefCell::new(None);
}
fn with_libos<T>(f: impl FnOnce(&mut LibOS<DPDKRuntime>) -> T) -> T {
    LIBOS.with(|l| {
        let mut tls_libos = l.borrow_mut();
        f(tls_libos.as_mut().expect("Uninitialized engine"))
    })
}

#[no_mangle]
pub extern "C" fn catnip_libos_noop() {
    println!("hey there!");
}

#[no_mangle]
pub extern "C" fn dmtr_init(argc: c_int, argv: *mut *mut c_char) -> c_int {
    let r: Result<_, Error> = try {
        if argc == 0 || argv.is_null() {
            Err(format_err!("Arguments not provided"))?;
        }
        let argument_ptrs = unsafe { slice::from_raw_parts(argv, argc as usize) };
        let arguments: Vec<_> = argument_ptrs
            .into_iter()
            .map(|&p| unsafe { CStr::from_ptr(p).to_str().expect("Non-UTF8 argument") })
            .collect();

        let matches = App::new("libos-catnip")
            .arg(
                Arg::with_name("config")
                    .short("c")
                    .long("config-path")
                    .value_name("FILE")
                    .help("YAML file for DPDK configuration")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("iterations")
                    .short("i")
                    .long("iterations")
                    .value_name("COUNT")
                    .help("Number of iterations")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("size")
                    .short("s")
                    .long("size")
                    .value_name("BYTES")
                    .help("Packet size")
                    .takes_value(true),
            )
            .get_matches_from(&arguments);

        let config_path = matches
            .value_of("config")
            .ok_or_else(|| format_err!("--config-path argument not provided"))?;

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
                let key_str = k.as_str()
                    .ok_or_else(|| format_err!("Couldn't find ARP table key in config"))?;
                let key = MacAddress::parse_str(key_str)?;
                let value: Ipv4Addr = v.as_str()
                    .ok_or_else(|| format_err!("Couldn't find ARP table key in config"))?
                    .parse()?;
                arp_table.insert(key, value);
            }
            println!("Pre-populating ARP table: {:?}", arp_table);
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

        let runtime = self::dpdk::initialize_dpdk(local_ipv4_addr, &eal_init_args, arp_table)?;
        logging::initialize();
        LibOS::new(runtime)?
    };
    let libos = match r {
        Ok(libos) => libos,
        Err(e) => {
            eprintln!("Initialization failure: {:?}", e);
            return libc::EINVAL;
        },
    };

    LIBOS.with(move |l| {
        let mut tls_libos = l.borrow_mut();
        assert!(tls_libos.is_none());
        *tls_libos = Some(libos);
    });

    0
}

#[no_mangle]
pub extern "C" fn dmtr_socket(
    qd_out: *mut c_int,
    domain: c_int,
    socket_type: c_int,
    protocol: c_int,
) -> c_int {
    with_libos(|libos| match libos.socket(domain, socket_type, protocol) {
        Ok(fd) => {
            unsafe { *qd_out = fd as c_int };
            0
        },
        Err(e) => {
            eprintln!("dmtr_socket failed: {:?}", e);
            e.errno()
        },
    })
}

#[no_mangle]
pub extern "C" fn dmtr_bind(qd: c_int, saddr: *const sockaddr, size: socklen_t) -> c_int {
    if saddr.is_null() {
        return libc::EINVAL;
    }
    if size as usize != mem::size_of::<libc::sockaddr_in>() {
        return libc::EINVAL;
    }
    let saddr_in = unsafe { *mem::transmute::<*const sockaddr, *const libc::sockaddr_in>(saddr) };
    let mut addr = Ipv4Addr::from(u32::from_be_bytes(saddr_in.sin_addr.s_addr.to_le_bytes()));
    let port = ip::Port::try_from(u16::from_be(saddr_in.sin_port)).unwrap();

    with_libos(|libos| {
        if addr.is_unspecified() {
            addr = libos.rt().local_ipv4_addr();
        }
        let endpoint = ipv4::Endpoint::new(addr, port);
        match libos.bind(qd as FileDescriptor, endpoint) {
            Ok(..) => 0,
            Err(e) => {
                eprintln!("dmtr_bind failed: {:?}", e);
                e.errno()
            },
        }
    })
}

#[no_mangle]
pub extern "C" fn dmtr_listen(fd: c_int, backlog: c_int) -> c_int {
    with_libos(
        |libos| match libos.listen(fd as FileDescriptor, backlog as usize) {
            Ok(..) => 0,
            Err(e) => {
                eprintln!("listen failed: {:?}", e);
                e.errno()
            },
        },
    )
}

#[no_mangle]
pub extern "C" fn dmtr_accept(qtok_out: *mut dmtr_qtoken_t, sockqd: c_int) -> c_int {
    with_libos(|libos| {
        unsafe { *qtok_out = libos.accept(sockqd as FileDescriptor) };
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_connect(
    qtok_out: *mut dmtr_qtoken_t,
    qd: c_int,
    saddr: *const sockaddr,
    size: socklen_t,
) -> c_int {
    if saddr.is_null() {
        return libc::EINVAL;
    }
    if size as usize != mem::size_of::<libc::sockaddr_in>() {
        return libc::EINVAL;
    }
    let saddr_in = unsafe { *mem::transmute::<*const sockaddr, *const libc::sockaddr_in>(saddr) };
    let addr = Ipv4Addr::from(u32::from_be_bytes(saddr_in.sin_addr.s_addr.to_le_bytes()));
    let port = ip::Port::try_from(u16::from_be(saddr_in.sin_port)).unwrap();
    let endpoint = ipv4::Endpoint::new(addr, port);

    with_libos(|libos| {
        unsafe { *qtok_out = libos.connect(qd as FileDescriptor, endpoint) };
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_close(qd: c_int) -> c_int {
    with_libos(|libos| match libos.close(qd as FileDescriptor) {
        Ok(..) => 0,
        Err(e) => {
            eprintln!("dmtr_close failed: {:?}", e);
            e.errno()
        },
    })
}

#[no_mangle]
pub extern "C" fn dmtr_push(
    qtok_out: *mut dmtr_qtoken_t,
    qd: c_int,
    sga: *const dmtr_sgarray_t,
) -> c_int {
    if sga.is_null() {
        return libc::EINVAL;
    }
    let sga = unsafe { &*sga };
    with_libos(|libos| {
        unsafe { *qtok_out = libos.push(qd as FileDescriptor, sga) };
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_pushto(
    qtok_out: *mut dmtr_qtoken_t,
    qd: c_int,
    sga: *const dmtr_sgarray_t,
    saddr: *const sockaddr,
    size: socklen_t,

) -> c_int {
    if sga.is_null() {
        return libc::EINVAL;
    }
    let sga = unsafe { &*sga };
    if saddr.is_null() {
        return libc::EINVAL;
    }
    if size as usize != mem::size_of::<libc::sockaddr_in>() {
        return libc::EINVAL;
    }
    let saddr_in = unsafe { *mem::transmute::<*const sockaddr, *const libc::sockaddr_in>(saddr) };
    let addr = Ipv4Addr::from(u32::from_be_bytes(saddr_in.sin_addr.s_addr.to_le_bytes()));
    let port = ip::Port::try_from(u16::from_be(saddr_in.sin_port)).unwrap();
    let endpoint = ipv4::Endpoint::new(addr, port);
    with_libos(|libos| {
        unsafe { *qtok_out = libos.pushto(qd as FileDescriptor, sga, endpoint) };
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_pop(qtok_out: *mut dmtr_qtoken_t, qd: c_int) -> c_int {
    with_libos(|libos| {
        unsafe { *qtok_out = libos.pop(qd as FileDescriptor) };
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_poll(qr_out: *mut dmtr_qresult_t, qt: dmtr_qtoken_t) -> c_int {
    with_libos(|libos| match libos.poll(qt) {
        None => libc::EAGAIN,
        Some(r) => {
            unsafe { *qr_out = r };
            0
        },
    })
}

#[no_mangle]
pub extern "C" fn dmtr_drop(qt: dmtr_qtoken_t) -> c_int {
    with_libos(|libos| {
        libos.drop_qtoken(qt);
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_wait(qr_out: *mut dmtr_qresult_t, qt: dmtr_qtoken_t) -> c_int {
    with_libos(|libos| {
        unsafe { *qr_out = libos.wait(qt) };
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_wait_any(
    qr_out: *mut dmtr_qresult_t,
    ready_offset: *mut c_int,
    qts: *mut dmtr_qtoken_t,
    num_qts: c_int,
) -> c_int {
    let qts = unsafe { slice::from_raw_parts(qts, num_qts as usize) };
    with_libos(|libos| {
        let (ix, qr) = libos.wait_any(qts);
        unsafe {
            *qr_out = qr;
            *ready_offset = ix as c_int;
        }
        0
    })
}

#[no_mangle]
pub extern "C" fn dmtr_sgafree(sga: *mut dmtr_sgarray_t) -> c_int {
    if sga.is_null() {
        return 0;
    }
    let sga = unsafe { *sga };
    for i in 0..sga.sga_numsegs as usize {
        let seg = &sga.sga_segs[i];
        let allocation = unsafe {
            Box::from_raw(slice::from_raw_parts_mut(
                seg.sgaseg_buf as *mut _,
                seg.sgaseg_len as usize,
            ))
        };
        drop(allocation);
    }

    0
}

// #[no_mangle]
// pub extern "C" fn dmtr_queue(qd_out: *mut c_int) -> c_int {
//     unimplemented!()
// }

// #[no_mangle]
// pub extern "C" fn dmtr_is_qd_valid(flag_out: *mut c_int, qd: c_int) -> c_int {
//     unimplemented!()
// }

// #[no_mangle]
// pub extern "C" fn dmtr_getsockname(qd: c_int, saddr: *mut sockaddr, size: *mut socklen_t) -> c_int {
//     unimplemented!();
// }
