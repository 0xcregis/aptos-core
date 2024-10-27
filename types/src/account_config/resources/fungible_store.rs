// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use super::aggregator::AggregatorResource;
use crate::{account_address::create_derived_object_address, contract_event::ContractEvent};
use move_core_types::{
    account_address::AccountAddress, ident_str, identifier::IdentStr, language_storage::TypeTag, move_resource::{MoveResource, MoveStructType}
};
#[cfg(any(test, feature = "fuzzing"))]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

pub fn primary_apt_store(address: AccountAddress) -> AccountAddress {
    create_derived_object_address(address, AccountAddress::TEN)
}

/// The balance resource held under an account.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "fuzzing"), derive(Arbitrary))]
pub struct FungibleStoreResource {
    pub metadata: AccountAddress,
    pub balance: u64,
    pub frozen: bool,
}

impl FungibleStoreResource {
    pub fn new(metadata: AccountAddress, balance: u64, frozen: bool) -> Self {
        Self {
            metadata,
            balance,
            frozen,
        }
    }

    pub fn metadata(&self) -> AccountAddress {
        self.metadata
    }

    pub fn balance(&self) -> u64 {
        self.balance
    }

    pub fn frozen(&self) -> bool {
        self.frozen
    }
}

impl MoveStructType for FungibleStoreResource {
    const MODULE_NAME: &'static IdentStr = ident_str!("fungible_asset");
    const STRUCT_NAME: &'static IdentStr = ident_str!("FungibleStore");
}

impl MoveResource for FungibleStoreResource {}

/// The balance resource held under an account.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConcurrentFungibleBalanceResource {
    /// The balance of the fungible metadata.
    balance: AggregatorResource<u64>,
}

impl ConcurrentFungibleBalanceResource {
    pub fn new(balance: u64) -> Self {
        Self {
            balance: AggregatorResource::new(balance, u64::MAX),
        }
    }

    pub fn balance(&self) -> u64 {
        *self.balance.get()
    }
}

impl MoveStructType for ConcurrentFungibleBalanceResource {
    const MODULE_NAME: &'static IdentStr = ident_str!("fungible_asset");
    const STRUCT_NAME: &'static IdentStr = ident_str!("ConcurrentFungibleBalance");
}

impl MoveResource for ConcurrentFungibleBalanceResource {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(any(test, feature = "fuzzing"), derive(Arbitrary))]
pub struct MigrationFlag {
    // Thanks to the "feature" of 1-byte empty struct
    dummy: bool,
}

impl MoveStructType for MigrationFlag {
    const MODULE_NAME: &'static IdentStr = ident_str!("coin");
    const STRUCT_NAME: &'static IdentStr = ident_str!("MigrationFlag");
}

impl MoveResource for MigrationFlag {}

pub trait MoveEventV2: MoveStructType + Serialize {
    fn create_event_v2(&self) -> ContractEvent {
        ContractEvent::new_v2(
            TypeTag::Struct(Box::new(Self::struct_tag())),
            bcs::to_bytes(self).unwrap()
        )
    }
}

/// Struct that represents a Withdraw event.
#[derive(Debug, Serialize, Deserialize)]
pub struct WithdrawFAEvent {
    pub store: AccountAddress,
    pub amount: u64,
}

impl MoveEventV2 for WithdrawFAEvent {}

impl MoveStructType for WithdrawFAEvent {
    const MODULE_NAME: &'static IdentStr = ident_str!("fungble_asset");
    const STRUCT_NAME: &'static IdentStr = ident_str!("Withdraw");
}

/// Struct that represents a Deposit event.
#[derive(Debug, Serialize, Deserialize)]
pub struct DepositFAEvent {
    pub store: AccountAddress,
    pub amount: u64,
}

impl MoveEventV2 for DepositFAEvent {}

impl MoveStructType for DepositFAEvent {
    const MODULE_NAME: &'static IdentStr = ident_str!("fungble_asset");
    const STRUCT_NAME: &'static IdentStr = ident_str!("Deposit");
}
