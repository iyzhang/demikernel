// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use crate::prelude::*;
use serde::ser::{Serialize, Serializer};
use std::fmt;

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct MacAddress(eui48::MacAddress);

impl MacAddress {
    pub fn new(bytes: [u8; 6]) -> Self {
        MacAddress(eui48::MacAddress::new(bytes))
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        MacAddress(eui48::MacAddress::from_bytes(bytes).unwrap())
    }

    pub fn broadcast() -> MacAddress {
        MacAddress(eui48::MacAddress::broadcast())
    }

    pub fn nil() -> MacAddress {
        MacAddress(eui48::MacAddress::nil())
    }

    pub fn is_nil(self) -> bool {
        self.0.is_nil()
    }

    pub fn is_broadcast(self) -> bool {
        self.0.is_broadcast()
    }

    pub fn is_unicast(self) -> bool {
        self.0.is_unicast()
    }

    pub fn to_canonical(self) -> String {
        self.0.to_canonical()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn to_array(self) -> [u8; 6] {
        self.0.to_array()
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MacAddress({})", &self.to_canonical())
    }
}

impl Serialize for MacAddress {
    fn serialize<S>(&self, serializer: S) -> StdResult<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = self.0.to_canonical();
        serializer.serialize_str(&s)
    }
}
