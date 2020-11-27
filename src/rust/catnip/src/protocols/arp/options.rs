// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use crate::protocols::ethernet2::MacAddress;
use hashbrown::HashMap;
use std::{
    net::Ipv4Addr,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct ArpOptions {
    pub cache_ttl: Duration,
    pub request_timeout: Duration,
    pub retry_count: usize,

    pub initial_values: HashMap<MacAddress, Ipv4Addr>,
    pub disable_arp: bool,
}

impl Default for ArpOptions {
    fn default() -> Self {
        ArpOptions {
            cache_ttl: Duration::from_secs(15),
            request_timeout: Duration::from_secs(20),
            retry_count: 5,
            initial_values: HashMap::new(),
            disable_arp: false,
        }
    }
}

impl ArpOptions {
    pub fn cache_ttl(mut self, value: Duration) -> Self {
        assert!(value > Duration::new(0, 0));
        self.cache_ttl = value;
        self
    }

    pub fn request_timeout(mut self, value: Duration) -> Self {
        assert!(value > Duration::new(0, 0));
        self.request_timeout = value;
        self
    }

    pub fn retry_count(mut self, value: usize) -> Self {
        assert!(value > 0);
        self.retry_count = value;
        self
    }
}
