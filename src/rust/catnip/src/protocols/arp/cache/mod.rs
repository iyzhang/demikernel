// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

// todo: remove once all functions are referenced.
#![allow(dead_code)]

#[cfg(test)]
mod tests;

use crate::{collections::HashTtlCache, protocols::ethernet2::MacAddress};
use std::{
    collections::HashMap,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
struct Record {
    link_addr: MacAddress,
    ipv4_addr: Ipv4Addr,
}

pub struct ArpCache {
    cache: HashTtlCache<Ipv4Addr, Record>,
    rmap: HashMap<MacAddress, Ipv4Addr>,
}

impl ArpCache {
    pub fn new(now: Instant, default_ttl: Option<Duration>) -> ArpCache {
        ArpCache {
            cache: HashTtlCache::new(now, default_ttl),
            rmap: HashMap::new(),
        }
    }

    pub fn insert_with_ttl(
        &mut self,
        ipv4_addr: Ipv4Addr,
        link_addr: MacAddress,
        ttl: Option<Duration>,
    ) -> Option<MacAddress> {
        let record = Record {
            ipv4_addr,
            link_addr,
        };

        let result = self
            .cache
            .insert_with_ttl(ipv4_addr, record, ttl)
            .map(|r| r.link_addr);
        self.rmap.insert(link_addr, ipv4_addr);
        result
    }

    pub fn insert(
        &mut self,
        ipv4_addr: Ipv4Addr,
        link_addr: MacAddress,
    ) -> Option<MacAddress> {
        let record = Record {
            ipv4_addr,
            link_addr,
        };

        let result = self.cache.insert(ipv4_addr, record).map(|r| r.link_addr);
        self.rmap.insert(link_addr, ipv4_addr);
        result
    }

    pub fn remove(&mut self, ipv4_addr: Ipv4Addr) {
        if let Some(record) = self.cache.remove(&ipv4_addr) {
            assert!(self.rmap.remove(&record.link_addr).is_some());
        } else {
            panic!(
                "attempt to remove unrecognized engine (`{}`) from ARP cache",
                ipv4_addr
            );
        }
    }

    pub fn get_link_addr(&self, ipv4_addr: Ipv4Addr) -> Option<&MacAddress> {
        let result = self.cache.get(&ipv4_addr).map(|r| &r.link_addr);
        debug!("`{:?}` -> `{:?}`", ipv4_addr, result);
        result
    }

    pub fn get_ipv4_addr(&self, link_addr: MacAddress) -> Option<&Ipv4Addr> {
        self.rmap.get(&link_addr)
    }

    pub fn advance_clock(&mut self, now: Instant) {
        self.cache.advance_clock(now)
    }

    pub fn try_evict(
        &mut self,
        count: usize,
    ) -> HashMap<Ipv4Addr, MacAddress> {
        let evicted = self.cache.try_evict(count);
        let mut result = HashMap::new();
        for (k, v) in &evicted {
            self.rmap.remove(&v.link_addr);
            assert!(result.insert(*k, v.link_addr).is_none());
        }

        result
    }

    pub fn clear(&mut self) {
        self.cache.clear();
        self.rmap.clear();
    }

    pub fn export(&self) -> HashMap<Ipv4Addr, MacAddress> {
        let mut map = HashMap::new();
        for (k, v) in self.cache.iter() {
            map.insert(*k, v.link_addr);
        }

        map
    }

    pub fn import(&mut self, cache: HashMap<Ipv4Addr, MacAddress>) {
        self.clear();
        for (&k, &v) in &cache {
            self.insert(k, v);
        }
    }
}
