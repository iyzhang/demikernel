// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;
use crate::test_helpers;

#[test]
fn with_default_ttl() {
    // tests to ensure that an entry without an explicit TTL gets evicted at
    // the right time (using the default TTL).
    let now = Instant::now();
    let later = now + Duration::from_secs(1);

    let mut cache = ArpCache::new(now, Some(Duration::from_secs(1)), false);
    cache.insert(test_helpers::ALICE_IPV4, test_helpers::ALICE_MAC);
    assert!(cache.get_link_addr(test_helpers::ALICE_IPV4) == Some(&test_helpers::ALICE_MAC));
    assert!(cache.get_ipv4_addr(test_helpers::ALICE_MAC) == Some(&test_helpers::ALICE_IPV4));
    cache.advance_clock(later);
    let evicted = cache.try_evict(usize::max_value());
    assert_eq!(evicted.len(), 1);
    assert!(evicted.contains_key(&test_helpers::ALICE_IPV4));
    assert!(cache.get_link_addr(test_helpers::ALICE_IPV4).is_none());
}
