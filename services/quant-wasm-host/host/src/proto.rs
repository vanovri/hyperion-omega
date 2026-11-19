//! Generated Protobuf v3 & gRPC bindings (`hyperion.v1` and `hyperion.omega.v1`).

#![allow(clippy::result_large_err)]

pub mod v1 {
    tonic::include_proto!("hyperion.v1");
}

mod omega_pkg {
    pub mod v1 {
        tonic::include_proto!("hyperion.omega.v1");
    }
}

pub use omega_pkg::v1 as omega;

impl v1::U128 {
    #[inline]
    pub const fn to_u128(&self) -> u128 {
        ((self.hi as u128) << 64) | (self.lo as u128)
    }

    #[inline]
    pub const fn from_u128(val: u128) -> Self {
        Self {
            hi: (val >> 64) as u64,
            lo: val as u64,
        }
    }
}

impl v1::I128 {
    #[inline]
    pub const fn to_i128(&self) -> i128 {
        ((self.hi as i128) << 64) | (self.lo as i128)
    }

    #[inline]
    pub const fn from_i128(val: i128) -> Self {
        Self {
            hi: (val >> 64) as i64,
            lo: val as u64,
        }
    }
}
