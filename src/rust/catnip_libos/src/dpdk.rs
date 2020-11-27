use hashbrown::HashMap;
use crate::{
    bindings::{
        rte_delay_us_block,
        rte_eal_init,
        rte_eth_conf,
        rte_eth_dev_configure,
        rte_eth_dev_count_avail,
        rte_eth_dev_flow_ctrl_get,
        rte_eth_dev_flow_ctrl_set,
        rte_eth_dev_info_get,
        rte_eth_dev_is_valid_port,
        rte_eth_dev_start,
        rte_eth_fc_mode_RTE_FC_NONE as RTE_FC_NONE,
        rte_eth_find_next_owned_by,
        rte_eth_link,
        rte_eth_link_get_nowait,
        rte_eth_macaddr_get,
        rte_eth_promiscuous_enable,
        rte_eth_rx_mq_mode_ETH_MQ_RX_RSS as ETH_MQ_RX_RSS,
        rte_eth_rx_queue_setup,
        rte_eth_rxconf,
        rte_eth_tx_mq_mode_ETH_MQ_TX_NONE as ETH_MQ_TX_NONE,
        rte_eth_tx_queue_setup,
        rte_eth_txconf,
        rte_ether_addr,
        rte_mbuf,
        rte_mempool,
        rte_pktmbuf_pool_create,
        rte_socket_id,
        ETH_LINK_FULL_DUPLEX,
        ETH_LINK_UP,
        ETH_RSS_IP,
        RTE_ETHER_MAX_LEN,
        RTE_ETH_DEV_NO_OWNER,
        RTE_MAX_ETHPORTS,
        RTE_MBUF_DEFAULT_BUF_SIZE,
    },
    runtime::DPDKRuntime,
};
use anyhow::{
    bail,
    format_err,
    Error,
};
use catnip::protocols::ethernet2::MacAddress;
use std::{
    ffi::CString,
    mem::MaybeUninit,
    net::Ipv4Addr,
    ptr,
    time::Duration,
};

macro_rules! expect_zero {
    ($name:ident ( $($arg: expr),* $(,)* )) => {{
        let ret = $name($($arg),*);
        if ret == 0 {
            Ok(0)
        } else {
            Err(format_err!("{} failed with {:?}", stringify!($name), ret))
        }
    }};
}

pub fn initialize_dpdk(
    local_ipv4_addr: Ipv4Addr,
    eal_init_args: &[CString],
    arp_table: HashMap<MacAddress, Ipv4Addr>,
    disable_arp: bool,
) -> Result<DPDKRuntime, Error> {
    std::env::set_var("MLX5_SHUT_UP_BF", "1");
    let eal_init_refs = eal_init_args
        .iter()
        .map(|s| s.as_ptr() as *mut u8)
        .collect::<Vec<_>>();
    unsafe {
        rte_eal_init(eal_init_refs.len() as i32, eal_init_refs.as_ptr() as *mut _);
    }
    let nb_ports = unsafe { rte_eth_dev_count_avail() };
    if nb_ports == 0 {
        bail!("No ethernet ports available");
    }
    eprintln!(
        "DPDK reports that {} ports (interfaces) are available.",
        nb_ports
    );

    let name = CString::new("default_mbuf_pool").unwrap();
    let num_mbufs = 8191;
    let mbuf_cache_size = 250;
    let mbuf_pool = unsafe {
        rte_pktmbuf_pool_create(
            name.as_ptr(),
            (num_mbufs * nb_ports) as u32,
            mbuf_cache_size,
            0,
            RTE_MBUF_DEFAULT_BUF_SIZE as u16,
            rte_socket_id() as i32,
        )
    };
    if mbuf_pool.is_null() {
        Err(format_err!("rte_pktmbuf_pool_create failed"))?;
    }
    let mut port_id = 0;
    {
        let owner = RTE_ETH_DEV_NO_OWNER as u64;
        let mut p = unsafe { rte_eth_find_next_owned_by(0, owner) as u16 };

        while p < RTE_MAX_ETHPORTS as u16 {
            // TODO: This is pretty hax, we clearly only support one port.
            port_id = p;
            initialize_dpdk_port(p, mbuf_pool)?;
            p = unsafe { rte_eth_find_next_owned_by(p + 1, owner) as u16 };
        }
    }

    // TODO: Where is this function?
    // if unsafe { rte_lcore_count() } > 1 {
    //     eprintln!("WARNING: Too many lcores enabled. Only 1 used.");
    // }

    let local_link_addr = unsafe {
        let mut m: MaybeUninit<rte_ether_addr> = MaybeUninit::zeroed();
        // TODO: Why does bindgen say this function doesn't return an int?
        rte_eth_macaddr_get(port_id, m.as_mut_ptr());
        MacAddress::new(m.assume_init().addr_bytes)
    };
    if local_link_addr.is_nil() || !local_link_addr.is_unicast() {
        Err(format_err!("Invalid mac address"))?;
    }

    Ok(DPDKRuntime::new(
        local_link_addr,
        local_ipv4_addr,
        port_id,
        mbuf_pool,
        arp_table,
        disable_arp,
    ))
}

fn initialize_dpdk_port(port_id: u16, mbuf_pool: *mut rte_mempool) -> Result<(), Error> {
    let rx_rings = 1;
    let tx_rings = 1;
    let rx_ring_size = 128;
    let tx_ring_size = 512;
    let nb_rxd = rx_ring_size;
    let nb_txd = tx_ring_size;

    let rx_pthresh = 0;
    let rx_hthresh = 0;
    let rx_wthresh = 0;

    let tx_pthresh = 0;
    let tx_hthresh = 0;
    let tx_wthresh = 0;

    let dev_info = unsafe {
        let mut d = MaybeUninit::zeroed();
        rte_eth_dev_info_get(port_id, d.as_mut_ptr());
        d.assume_init()
    };

    let mut port_conf: rte_eth_conf = unsafe { MaybeUninit::zeroed().assume_init() };
    port_conf.rxmode.max_rx_pkt_len = RTE_ETHER_MAX_LEN;
    port_conf.rxmode.mq_mode = ETH_MQ_RX_RSS;
    port_conf.rx_adv_conf.rss_conf.rss_hf = ETH_RSS_IP as u64 | dev_info.flow_type_rss_offloads;
    port_conf.txmode.mq_mode = ETH_MQ_TX_NONE;

    let mut rx_conf: rte_eth_rxconf = unsafe { MaybeUninit::zeroed().assume_init() };
    rx_conf.rx_thresh.pthresh = rx_pthresh;
    rx_conf.rx_thresh.hthresh = rx_hthresh;
    rx_conf.rx_thresh.wthresh = rx_wthresh;
    rx_conf.rx_free_thresh = 32;

    let mut tx_conf: rte_eth_txconf = unsafe { MaybeUninit::zeroed().assume_init() };
    tx_conf.tx_thresh.pthresh = tx_pthresh;
    tx_conf.tx_thresh.hthresh = tx_hthresh;
    tx_conf.tx_thresh.wthresh = tx_wthresh;
    tx_conf.tx_free_thresh = 32;

    unsafe {
        expect_zero!(rte_eth_dev_configure(
            port_id,
            rx_rings,
            tx_rings,
            &port_conf as *const _,
        ))?;
    }

    let socket_id = 0;

    unsafe {
        for i in 0..rx_rings {
            expect_zero!(rte_eth_rx_queue_setup(
                port_id,
                i,
                nb_rxd,
                socket_id,
                &rx_conf as *const _,
                mbuf_pool
            ))?;
        }
        for i in 0..tx_rings {
            expect_zero!(rte_eth_tx_queue_setup(
                port_id,
                i,
                nb_txd,
                socket_id,
                &tx_conf as *const _
            ))?;
        }
        expect_zero!(rte_eth_dev_start(port_id))?;
        rte_eth_promiscuous_enable(port_id);
    }

    if unsafe { rte_eth_dev_is_valid_port(port_id) } == 0 {
        bail!("Invalid port");
    }

    let sleep_duration = Duration::from_millis(100);
    let mut retry_count = 90;

    loop {
        unsafe {
            let mut link: MaybeUninit<rte_eth_link> = MaybeUninit::zeroed();
            rte_eth_link_get_nowait(port_id, link.as_mut_ptr());
            let link = link.assume_init();
            if link.link_status() as u32 == ETH_LINK_UP {
                let duplex = if link.link_duplex() as u32 == ETH_LINK_FULL_DUPLEX {
                    "full"
                } else {
                    "half"
                };
                eprintln!(
                    "Port {} Link Up - speed {} Mbps - {} duplex",
                    port_id, link.link_speed, duplex
                );
                break;
            }
            rte_delay_us_block(sleep_duration.as_micros() as u32);
        }
        if retry_count == 0 {
            bail!("Link never came up");
        }
        retry_count -= 1;
    }

    Ok(())
}

// pub unsafe fn rte_pktmbuf_free(mut m: *mut rte_mbuf) {
//     let mut m_next = ptr::null_mut();

//     while !m.is_null() {
//         m_next = (*m).next;
//         rte_pktmbuf_free_seg(m);
//         m = m_next;
//     }
// }

// unsafe fn rte_pktmbuf_free_seg(mut m: *mut rte_mbuf) {
//     m = rte_pktmbuf_prefree_seg(m);
//     if !m.is_null() {
//         rte_mbuf_raw_free(m);
//     }
// }

// unsafe fn rte_pktmbuf_prefree_seg(m: *mut rte_mbuf) -> *mut rte_mbuf {
//     if rte_mbuf_refcnt_read(m) == 1 {
//         if !rte_mbuf_direct(m) {
//             rte_pktmbuf_detach(m);
//         }
//         if !(*m).next.is_null() {
//             (*m).next = ptr::null_mut();
//             (*m).nb_segs = 1;
//         }

//         return m;
//     } else if rte_mbuf_refcnt_update(m, -1) == 0 {
//         if !rte_mbuf_direct(m) {
//             rte_pktmbuf_detach(m);
//         }
//         if !(*m).next.is_null() {
//             (*m).next = ptr::null_mut();
//             (*m).nb_segs = 1;
//         }
// 		rte_mbuf_refcnt_set(m, 1);
//         return m;
//     }
//     ptr::null_mut()
// }

// unsafe fn rte_mbuf_refcnt_read(m: *mut rte_buf) -> u16 {
//     rte_atomic16_read((*m).refcnt_atomic) as u16
// }

// unsafe fn rte_mbuf_raw_free(m: *mut rte_mbuf) {
//     todo!();
// }
