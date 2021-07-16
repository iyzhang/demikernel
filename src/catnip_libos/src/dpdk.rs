use dpdk_rs::{
        rte_delay_us_block,
        rte_eal_init,
        rte_eth_conf,
        rte_eth_dev_configure,
        rte_eth_dev_count_avail,
        rte_eth_dev_flow_ctrl_get,
        rte_eth_dev_flow_ctrl_set,
        rte_eth_dev_get_mtu,
        rte_eth_dev_info_get,
        rte_eth_dev_is_valid_port,
        rte_eth_dev_set_mtu,
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
        DEV_TX_OFFLOAD_MULTI_SEGS,
        DEV_RX_OFFLOAD_JUMBO_FRAME,
        DEV_RX_OFFLOAD_TCP_CKSUM,
        DEV_TX_OFFLOAD_TCP_CKSUM,
        DEV_RX_OFFLOAD_UDP_CKSUM,
        DEV_TX_OFFLOAD_UDP_CKSUM,
        ETH_LINK_FULL_DUPLEX,
        ETH_LINK_UP,
        ETH_RSS_IP,
        RTE_ETHER_MAX_JUMBO_FRAME_LEN,
        RTE_ETHER_MAX_LEN,
        RTE_ETH_DEV_NO_OWNER,
        RTE_MAX_ETHPORTS,
        RTE_MBUF_DEFAULT_BUF_SIZE,
        RTE_PKTMBUF_HEADROOM,
};
use crate::memory::{MemoryConfig, MemoryManager};
use crate::runtime::DPDKRuntime;
use anyhow::{
    bail,
    format_err,
    Error,
};
use catnip::protocols::ethernet2::MacAddress;
use std::collections::HashMap;
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
    arp_table: HashMap<Ipv4Addr, MacAddress>,
    disable_arp: bool,
    use_jumbo_frames: bool,
    mtu: u16,
    mss: usize,
    tcp_checksum_offload: bool,
    udp_checksum_offload: bool,
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
//    eprintln!(
//        "DPDK reports that {} ports (interfaces) are available.",
//        nb_ports
//    );

    let mut memory_config = MemoryConfig::default();
    if use_jumbo_frames {
        memory_config.max_body_size = (RTE_ETHER_MAX_JUMBO_FRAME_LEN + RTE_PKTMBUF_HEADROOM) as usize;
    }
    let memory_manager = MemoryManager::new(memory_config)?;

    let owner = RTE_ETH_DEV_NO_OWNER as u64;
    let port_id = unsafe { rte_eth_find_next_owned_by(0, owner) as u16 };
    initialize_dpdk_port(port_id, &memory_manager, use_jumbo_frames, mtu, tcp_checksum_offload, udp_checksum_offload)?;

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
        memory_manager,
        arp_table,
        disable_arp,
        mss,
        tcp_checksum_offload,
        udp_checksum_offload,
    ))
}

fn initialize_dpdk_port(
    port_id: u16,
    memory_manager: &MemoryManager,
    use_jumbo_frames: bool,
    mtu: u16,
    tcp_checksum_offload: bool,
    udp_checksum_offload: bool,
) -> Result<(), Error> {
    let rx_rings = 1;
    let tx_rings = 1;
    let rx_ring_size = 128;
    let tx_ring_size = 512;
    let nb_rxd = rx_ring_size;
    let nb_txd = tx_ring_size;

    let rx_pthresh = 8;
    let rx_hthresh = 8;
    let rx_wthresh = 0;

    let tx_pthresh = 0;
    let tx_hthresh = 0;
    let tx_wthresh = 0;

    let dev_info = unsafe {
        let mut d = MaybeUninit::zeroed();
        rte_eth_dev_info_get(port_id, d.as_mut_ptr());
        d.assume_init()
    };

    println!("dev_info: {:?}", dev_info);
    unsafe {
        expect_zero!(rte_eth_dev_set_mtu(port_id, mtu))?;
        let mut dpdk_mtu = 0u16;
        expect_zero!(rte_eth_dev_get_mtu(port_id, &mut dpdk_mtu as *mut _))?;
        if dpdk_mtu != mtu {
            bail!("Failed to set MTU to {}, got back {}", mtu, dpdk_mtu);
        }
    }

    let mut port_conf: rte_eth_conf = unsafe { MaybeUninit::zeroed().assume_init() };
    port_conf.rxmode.max_rx_pkt_len = if use_jumbo_frames {
        RTE_ETHER_MAX_JUMBO_FRAME_LEN
    } else {
        RTE_ETHER_MAX_LEN
    };
    if tcp_checksum_offload {
        port_conf.rxmode.offloads |= DEV_RX_OFFLOAD_TCP_CKSUM as u64;
    }
    if udp_checksum_offload {
        port_conf.rxmode.offloads |= DEV_RX_OFFLOAD_UDP_CKSUM as u64;
    }
    if use_jumbo_frames {
        port_conf.rxmode.offloads |= DEV_RX_OFFLOAD_JUMBO_FRAME as u64;
    }
    port_conf.rxmode.mq_mode = ETH_MQ_RX_RSS;
    port_conf.rx_adv_conf.rss_conf.rss_hf = ETH_RSS_IP as u64 | dev_info.flow_type_rss_offloads;

    port_conf.txmode.mq_mode = ETH_MQ_TX_NONE;
    if tcp_checksum_offload {
        port_conf.txmode.offloads |= DEV_TX_OFFLOAD_TCP_CKSUM as u64;
    }
    if udp_checksum_offload {
        port_conf.txmode.offloads |= DEV_TX_OFFLOAD_UDP_CKSUM as u64;
    }
    port_conf.txmode.offloads |= DEV_TX_OFFLOAD_MULTI_SEGS as u64;

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
                memory_manager.body_pool(),
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
//                eprintln!(
//                    "Port {} Link Up - speed {} Mbps - {} duplex",
//                    port_id, link.link_speed, duplex
//                );
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
