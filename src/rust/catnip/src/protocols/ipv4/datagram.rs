use crate::{
    fail::Fail,
    sync::Bytes,
};
use byteorder::{
    ByteOrder,
    NetworkEndian,
};
use num_traits::FromPrimitive;
use std::{
    convert::{
        TryFrom,
        TryInto,
    },
    net::Ipv4Addr,
};

pub const IPV4_HEADER2_SIZE: usize = 20;

// todo: need citation
pub const DEFAULT_IPV4_TTL: u8 = 64;
pub const IPV4_IHL_NO_OPTIONS: u8 = 5;
pub const IPV4_VERSION: u8 = 4;

#[repr(u8)]
#[derive(FromPrimitive, Copy, Clone, PartialEq, Eq, Debug)]
pub enum Ipv4Protocol2 {
    Icmpv4 = 0x01,
    Tcp = 0x06,
    Udp = 0x11,
}

impl TryFrom<u8> for Ipv4Protocol2 {
    type Error = Fail;

    fn try_from(n: u8) -> Result<Self, Fail> {
        match FromPrimitive::from_u8(n) {
            Some(n) => Ok(n),
            None => Err(Fail::Unsupported {
                details: "Unsupported IPv4 protocol",
            }),
        }
    }
}

pub struct Ipv4Header {
    // [ version 4 bits ] [ IHL 4 bits ]
    // The user shouldn't be able to mutate the version, so we parse it out but don't include it
    // here. Since we don't support IPv4 options, the same holds for the ihl field.
    // pub version: u8,
    // pub ihl: u8,

    // [ DSCP 6 bits ] [ ECN 2 bits ]
    pub dscp: u8,
    pub ecn: u8,

    // Omit the total_length since it's generated on serialization.
    // pub total_length: u16,
    pub identification: u16,

    // [ flags 3 bits ] [ fragment offset 13 bits ]
    pub flags: u8,
    pub fragment_offset: u16,

    pub time_to_live: u8,
    pub protocol: Ipv4Protocol2,

    // We omit the header checksum since it's checked when parsing and computed when serializing.
    // header_checksum: u16,
    pub src_addr: Ipv4Addr,
    pub dst_addr: Ipv4Addr,
}

fn ipv4_checksum(buf: &[u8]) -> u16 {
    let buf: &[u8; IPV4_HEADER2_SIZE] = buf.try_into().expect("Invalid header size");
    let mut state = 0xffffu32;
    for i in 0..5 {
        state += NetworkEndian::read_u16(&buf[(2 * i)..(2 * i + 2)]) as u32;
    }
    // Skip the 5th u16 since octets 10-12 are the header checksum, whose value should be zero when
    // computing a checksum.
    for i in 6..10 {
        state += NetworkEndian::read_u16(&buf[(2 * i)..(2 * i + 2)]) as u32;
    }
    while state > 0xffff {
        state -= 0xffff;
    }
    !state as u16
}

impl Ipv4Header {
    pub fn new(src_addr: Ipv4Addr, dst_addr: Ipv4Addr, protocol: Ipv4Protocol2) -> Self {
        Self {
            dscp: 0,
            ecn: 0,
            identification: 0,
            flags: 0,
            fragment_offset: 0,
            time_to_live: 0,
            protocol,
            src_addr,
            dst_addr,
        }
    }

    pub fn compute_size(&self) -> usize {
        // We don't support IPv4 options, so this is always 20.
        IPV4_HEADER2_SIZE
    }

    pub fn parse(buf: Bytes) -> Result<(Self, Bytes), Fail> {
        if buf.len() < IPV4_HEADER2_SIZE {
            return Err(Fail::Malformed {
                details: "Datagram too small",
            });
        }
        let (hdr_buf, mut payload_buf) = buf.split(IPV4_HEADER2_SIZE);

        let version = hdr_buf[0] >> 4;
        if version != IPV4_VERSION {
            return Err(Fail::Unsupported {
                details: "Unsupported IP version",
            });
        }

        let ihl = hdr_buf[0] & 0xF;
        if ihl < IPV4_IHL_NO_OPTIONS {
            return Err(Fail::Malformed {
                details: "IPv4 IHL is too small",
            });
        }
        if ihl > IPV4_IHL_NO_OPTIONS {
            return Err(Fail::Unsupported {
                details: "IPv4 options are unsupported",
            });
        }

        let dscp = hdr_buf[1] >> 2;
        let ecn = hdr_buf[1] & 3;

        let total_length = NetworkEndian::read_u16(&hdr_buf[2..4]) as usize;

        // The TOTALLEN is definitely malformed if it doesn't have room for our header.
        if total_length < IPV4_HEADER2_SIZE {
            return Err(Fail::Malformed { details: "IPv4 TOTALLEN smaller than header" });
        }
        if total_length - IPV4_HEADER2_SIZE > payload_buf.len() {
            return Err(Fail::Malformed { details: "IPv4 TOTALLEN greater than header + payload" });
        }
        // NB (sujayakar, 11/6/2020): I've noticed that Ethernet transmission is liable to add
        // padding zeros for small payloads, so we can't assert that the Ethernet payload we
        // receives exactly matches the header's TOTALLEN. Therefore, we may need to truncate off
        // padding bytes when they don't line up.
        let (payload, _padding) = payload_buf.split(total_length - IPV4_HEADER2_SIZE);
        payload_buf = payload;

        let identification = NetworkEndian::read_u16(&hdr_buf[4..6]);
        let flags = (NetworkEndian::read_u16(&hdr_buf[6..8]) >> 13) as u8;

        let fragment_offset = NetworkEndian::read_u16(&hdr_buf[6..8]) & 0x1fff;
        if fragment_offset != 0 {
            return Err(Fail::Unsupported {
                details: "IPv4 fragmentation is unsupported",
            });
        }

        let time_to_live = hdr_buf[8];
        let protocol = Ipv4Protocol2::try_from(hdr_buf[9])?;

        let header_checksum = NetworkEndian::read_u16(&hdr_buf[10..12]);
        if header_checksum == 0xffff {
            return Err(Fail::Malformed {
                details: "IPv4 checksum is 0xFFFF",
            });
        }
        if header_checksum != ipv4_checksum(&hdr_buf[..]) {
            return Err(Fail::Malformed {
                details: "Invalid IPv4 checksum",
            });
        }

        let src_addr = Ipv4Addr::from(NetworkEndian::read_u32(&hdr_buf[12..16]));
        let dst_addr = Ipv4Addr::from(NetworkEndian::read_u32(&hdr_buf[16..20]));

        let header = Self {
            dscp,
            ecn,
            identification,
            flags,
            fragment_offset,
            time_to_live,
            protocol,
            src_addr,
            dst_addr,
        };
        Ok((header, payload_buf))
    }

    pub fn serialize(&self, buf: &mut [u8], payload_len: usize) {
        let buf: &mut [u8; IPV4_HEADER2_SIZE] = buf.try_into().unwrap();
        buf[0] = (IPV4_VERSION << 4) | IPV4_IHL_NO_OPTIONS;
        buf[1] = (self.dscp << 2) | (self.ecn & 3);
        NetworkEndian::write_u16(&mut buf[2..4], (IPV4_HEADER2_SIZE + payload_len) as u16);
        NetworkEndian::write_u16(&mut buf[4..6], self.identification);
        NetworkEndian::write_u16(
            &mut buf[6..8],
            (self.flags as u16) << 13 | self.fragment_offset & 0x1fff,
        );
        buf[8] = self.time_to_live;
        buf[9] = self.protocol as u8;

        // Skip the checksum (bytes 10..12) until we finish writing the header.
        buf[12..16].copy_from_slice(&self.src_addr.octets());
        buf[16..20].copy_from_slice(&self.dst_addr.octets());

        let checksum = ipv4_checksum(buf);
        NetworkEndian::write_u16(&mut buf[10..12], checksum);
    }
}
