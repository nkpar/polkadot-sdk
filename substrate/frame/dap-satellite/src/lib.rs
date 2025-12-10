// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # DAP Satellite Pallet
//!
//! This pallet is meant to be used on **system chains other than AssetHub** (e.g., Coretime,
//! People, BridgeHub) or on the **Relay Chain**. It should NOT be deployed on AssetHub, which
//! hosts the central DAP pallet (`pallet-dap`).
//!
//! ## Purpose
//!
//! The DAP Satellite collects funds that would otherwise be burned (e.g., transaction fees,
//! coretime revenue, slashing) into a local satellite account. These funds are accumulated
//! locally and will eventually be transferred via XCM to the central DAP buffer on AssetHub.
//!
//! ## Architecture
//!
//! ```text
//! +-----------------+     +-----------------+     +-----------------+
//! |  Relay Chain    |     |  Coretime Chain |     |  People Chain   |
//! |  DAPSatellite   |     |  DAPSatellite   |     |  DAPSatellite   |
//! +--------+--------+     +--------+--------+     +--------+--------+
//!          |                       |                       |
//!          |     XCM (periodic)    |                       |
//!          +-----------------------+-----------------------+
//!                                  |
//!                                  v
//!                        +-----------------+
//!                        |   AssetHub      |
//!                        |   pallet-dap    |
//!                        |   (central)     |
//!                        +-----------------+
//! ```
//!
//! ## Implementation
//!
//! This is a minimal implementation that only accumulates funds locally. The periodic XCM
//! transfer to AssetHub is NOT yet implemented.
//!
//! In this first iteration, the pallet provides the following components:
//! - `AccumulateInSatellite`: Implementation of `FundingSink` that transfers funds to the satellite
//!   account instead of burning them.
//! - `SlashToSatellite`: Implementation of `OnUnbalanced` for the new `fungible::Balanced` trait,
//!   useful for staking slashes.
//!
//! **TODO:**
//! - Periodic XCM transfer to AssetHub DAP buffer
//! - Configuration for XCM period and destination
//! - Weight accounting for XCM operations
//!
//! ## Usage
//!
//! On system chains (not AssetHub) or Relay Chain, configure pallets to use the satellite:
//!
//! ```ignore
//! // In runtime configuration for Coretime/People/BridgeHub/RelayChain
//! impl pallet_balances::Config for Runtime {
//!     type BurnDestination = pallet_dap_satellite::AccumulateInSatellite<Runtime>;
//! }
//!
//! // For fee handlers using OnUnbalanced
//! type FeeDestination = pallet_dap_satellite::SlashToSatellite<Runtime>;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use frame_support::{
	pallet_prelude::*,
	traits::{
		fungible::{Balanced, Credit, Inspect, Mutate},
		tokens::{Fortitude, FundingSink, Precision, Preservation},
		Currency, Imbalance, OnUnbalanced,
	},
	PalletId,
};

pub use pallet::*;

const LOG_TARGET: &str = "runtime::dap-satellite";

/// Type alias for balance.
pub type BalanceOf<T> =
	<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

#[frame_support::pallet]
pub mod pallet {
	use super::*;
	use frame_support::sp_runtime::traits::AccountIdConversion;

	/// The in-code storage version.
	const STORAGE_VERSION: frame_support::traits::StorageVersion =
		frame_support::traits::StorageVersion::new(1);

	#[pallet::pallet]
	#[pallet::storage_version(STORAGE_VERSION)]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config {
		/// The currency type.
		type Currency: Inspect<Self::AccountId>
			+ Mutate<Self::AccountId>
			+ Balanced<Self::AccountId>;

		/// The pallet ID used to derive the satellite account.
		///
		/// Each runtime should configure a unique ID to avoid collisions if multiple
		/// DAP satellite instances are used.
		#[pallet::constant]
		type PalletId: Get<PalletId>;
	}

	impl<T: Config> Pallet<T> {
		/// Get the satellite account derived from the pallet ID.
		///
		/// This account accumulates funds locally before they are sent to AssetHub.
		pub fn satellite_account() -> T::AccountId {
			T::PalletId::get().into_account_truncating()
		}

		/// Ensure the satellite account exists by incrementing its provider count.
		///
		/// This is called at genesis and on runtime upgrade.
		/// It's idempotent - calling it multiple times is safe.
		pub fn ensure_satellite_account_exists() {
			let satellite = Self::satellite_account();
			if !frame_system::Pallet::<T>::account_exists(&satellite) {
				frame_system::Pallet::<T>::inc_providers(&satellite);
				log::info!(
					target: LOG_TARGET,
					"Created DAP satellite account: {satellite:?}"
				);
			}
		}
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<frame_system::pallet_prelude::BlockNumberFor<T>> for Pallet<T> {
		fn on_runtime_upgrade() -> Weight {
			// Create the satellite account if it doesn't exist (for chains upgrading to DAP).
			Self::ensure_satellite_account_exists();
			// Weight: 1 read (account_exists) + potentially 1 write (inc_providers)
			T::DbWeight::get().reads_writes(1, 1)
		}
	}

	/// Genesis config for the DAP Satellite pallet.
	#[pallet::genesis_config]
	#[derive(frame_support::DefaultNoBound)]
	pub struct GenesisConfig<T: Config> {
		#[serde(skip)]
		_phantom: core::marker::PhantomData<T>,
	}

	#[pallet::genesis_build]
	impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
		fn build(&self) {
			// Create the satellite account at genesis so it can receive funds of any amount.
			Pallet::<T>::ensure_satellite_account_exists();
		}
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// Funds accumulated in satellite account.
		FundsAccumulated { from: T::AccountId, amount: BalanceOf<T> },
	}
}

/// Implementation of `FundingSink` that accumulates funds in the satellite account.
///
/// Use this on system chains (not AssetHub) or Relay Chain to collect funds that would
/// otherwise be burned. The funds will eventually be transferred to AssetHub DAP via XCM.
///
/// # Example
///
/// ```ignore
/// impl pallet_balances::Config for Runtime {
///     type BurnDestination = AccumulateInSatellite<Runtime>;
/// }
/// ```
pub struct AccumulateInSatellite<T>(core::marker::PhantomData<T>);

impl<T: Config> FundingSink<T::AccountId, BalanceOf<T>> for AccumulateInSatellite<T> {
	fn fill(source: &T::AccountId, amount: BalanceOf<T>, preservation: Preservation) {
		let satellite = Pallet::<T>::satellite_account();

		// Withdraw from source, resolve to satellite, emit event. If withdraw fails, nothing
		// happens. If resolve fails (should never happen - satellite pre-created at genesis or via
		// runtime upgrade), funds are burned.
		T::Currency::withdraw(source, amount, Precision::Exact, preservation, Fortitude::Polite)
			.ok()
			.map(|credit| T::Currency::resolve(&satellite, credit))
			.map(|_| {
				Pallet::<T>::deposit_event(Event::FundsAccumulated { from: source.clone(), amount })
			});
	}
}

/// Type alias for credit (negative imbalance - funds that were removed).
/// This is for the `fungible::Balanced` trait.
pub type CreditOf<T> = Credit<<T as frame_system::Config>::AccountId, <T as Config>::Currency>;

/// Implementation of `OnUnbalanced` for the `fungible::Balanced` trait.
///
/// Use this on system chains (not AssetHub) or Relay Chain to collect funds from
/// imbalances (e.g., slashing) that would otherwise be burned.
///
/// Note: This handler does NOT emit events because it can be called very frequently
/// (e.g., for every fee-paying transaction via `DealWithFeesSplit`).
///
/// # Example
///
/// ```ignore
/// impl pallet_staking::Config for Runtime {
///     type Slash = SlashToSatellite<Runtime>;
/// }
/// ```
pub struct SlashToSatellite<T>(core::marker::PhantomData<T>);

impl<T: Config> OnUnbalanced<CreditOf<T>> for SlashToSatellite<T> {
	fn on_nonzero_unbalanced(amount: CreditOf<T>) {
		let satellite = Pallet::<T>::satellite_account();
		let numeric_amount = amount.peek();

		// The satellite account is created at genesis or on_runtime_upgrade, so resolve should
		// always succeed. If it somehow fails, log the error.
		if let Err(remaining) = T::Currency::resolve(&satellite, amount) {
			let remaining_amount = remaining.peek();
			if !remaining_amount.is_zero() {
				log::error!(
					target: LOG_TARGET,
					"Failed to deposit to satellite account - {remaining_amount:?} will be burned!"
				);
			}
		}

		log::debug!(
			target: LOG_TARGET,
			"Deposited {numeric_amount:?} to satellite account (fungible)"
		);
	}
}

/// A configurable fee handler that splits fees between DAP satellite and another destination.
///
/// - `DapPercent`: Percentage of fees (0-100) to send to DAP satellite
/// - `OtherHandler`: Where to send the remaining fees (e.g., `ToAuthor`, `DealWithFees`)
///
/// Tips always go 100% to `OtherHandler`.
///
/// # Example
///
/// ```ignore
/// parameter_types! {
///     pub const DapSatelliteFeePercent: u32 = 0; // 0% to DAP, 100% to staking
/// }
///
/// type DealWithFeesSatellite = pallet_dap_satellite::DealWithFeesSplit<
///     Runtime,
///     DapSatelliteFeePercent,
///     DealWithFees<Runtime>, // Or ToAuthor<Runtime> for relay chain
/// >;
///
/// impl pallet_transaction_payment::Config for Runtime {
///     type OnChargeTransaction = FungibleAdapter<Balances, DealWithFeesSatellite>;
/// }
/// ```
pub struct DealWithFeesSplit<T, DapPercent, OtherHandler>(
	core::marker::PhantomData<(T, DapPercent, OtherHandler)>,
);

impl<T, DapPercent, OtherHandler> OnUnbalanced<CreditOf<T>>
	for DealWithFeesSplit<T, DapPercent, OtherHandler>
where
	T: Config,
	DapPercent: Get<u32>,
	OtherHandler: OnUnbalanced<CreditOf<T>>,
{
	fn on_unbalanceds(mut fees_then_tips: impl Iterator<Item = CreditOf<T>>) {
		if let Some(fees) = fees_then_tips.next() {
			let dap_percent = DapPercent::get();
			let other_percent = 100u32.saturating_sub(dap_percent);
			let mut split = fees.ration(dap_percent, other_percent);
			if let Some(tips) = fees_then_tips.next() {
				// Tips go 100% to other handler.
				tips.merge_into(&mut split.1);
			}
			if dap_percent > 0 {
				<SlashToSatellite<T> as OnUnbalanced<_>>::on_unbalanced(split.0);
			}
			OtherHandler::on_unbalanced(split.1);
		}
	}
}

/// Implementation of `OnUnbalanced` for the old `Currency` trait.
///
/// Use this on system chains (not AssetHub) or Relay Chain for pallets that still use
/// the legacy `Currency` trait (e.g., fee handlers, treasury burns).
///
/// Note: This handler does NOT emit events because it can be called very frequently
/// (e.g., for every fee-paying transaction).
///
/// # Example
///
/// ```ignore
/// // For fee handling
/// type FeeDestination = SinkToSatellite<Runtime, Balances>;
///
/// // For treasury burns
/// impl pallet_treasury::Config for Runtime {
///     type BurnDestination = SinkToSatellite<Runtime, Balances>;
/// }
/// ```
pub struct SinkToSatellite<T, C>(core::marker::PhantomData<(T, C)>);

impl<T, C> OnUnbalanced<C::NegativeImbalance> for SinkToSatellite<T, C>
where
	T: Config,
	C: Currency<T::AccountId>,
{
	fn on_nonzero_unbalanced(amount: C::NegativeImbalance) {
		let satellite = Pallet::<T>::satellite_account();
		let numeric_amount = amount.peek();

		// Resolve the imbalance by depositing into the satellite account
		C::resolve_creating(&satellite, amount);

		log::debug!(
			target: LOG_TARGET,
			"Deposited {numeric_amount:?} to satellite account (Currency trait)"
		);
	}
}

// TODO: Implement periodic XCM transfer to AssetHub DAP buffer
//
// Future implementation will add:
// 1. `on_initialize` hook to mark XCM as pending at configured intervals
// 2. `on_poll` hook to execute XCM transfer when pending and weight available
// 3. Configuration for:
//    - `XcmPeriod`: How often to send accumulated funds (e.g., every 14400 blocks = ~1 day)
//    - `AssetHubLocation`: XCM destination for AssetHub
//    - `DapBufferBeneficiary`: The DAP buffer account on AssetHub
// 4. XCM message construction:
//    - Burn from local satellite account
//    - Teleport to AssetHub
//    - Deposit into DAP buffer account

#[cfg(test)]
mod tests {
	use super::*;
	use frame_support::{
		derive_impl, parameter_types,
		sp_runtime::traits::AccountIdConversion,
		traits::{
			fungible::Balanced, tokens::FundingSink, Currency as CurrencyT, ExistenceRequirement,
			OnUnbalanced, WithdrawReasons,
		},
	};
	use sp_runtime::BuildStorage;
	use std::cell::Cell;

	type Block = frame_system::mocking::MockBlock<Test>;

	frame_support::construct_runtime!(
		pub enum Test {
			System: frame_system,
			Balances: pallet_balances,
			DapSatellite: crate,
		}
	);

	#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
	impl frame_system::Config for Test {
		type Block = Block;
		type AccountData = pallet_balances::AccountData<u64>;
	}

	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for Test {
		type AccountStore = System;
	}

	parameter_types! {
		pub const DapSatellitePalletId: PalletId = PalletId(*b"dap/satl");
	}

	impl Config for Test {
		type Currency = Balances;
		type PalletId = DapSatellitePalletId;
	}

	fn new_test_ext() -> sp_io::TestExternalities {
		let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();
		pallet_balances::GenesisConfig::<Test> {
			balances: vec![(1, 100), (2, 200), (3, 300)],
			..Default::default()
		}
		.assimilate_storage(&mut t)
		.unwrap();
		crate::pallet::GenesisConfig::<Test>::default()
			.assimilate_storage(&mut t)
			.unwrap();
		t.into()
	}

	#[test]
	fn satellite_account_is_derived_from_pallet_id() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();
			let expected: u64 = DapSatellitePalletId::get().into_account_truncating();
			assert_eq!(satellite, expected);
		});
	}

	#[test]
	fn genesis_creates_satellite_account() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();
			// Satellite account should exist after genesis (created via inc_providers)
			assert!(System::account_exists(&satellite));
		});
	}

	// ===== fill tests =====

	#[test]
	fn fill_accumulates_from_multiple_sources() {
		new_test_ext().execute_with(|| {
			System::set_block_number(1);
			let satellite = DapSatellite::satellite_account();

			// Given: accounts have balances, satellite is empty
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: fill from multiple accounts
			AccumulateInSatellite::<Test>::fill(&1, 20, Preservation::Preserve);
			AccumulateInSatellite::<Test>::fill(&2, 50, Preservation::Preserve);
			AccumulateInSatellite::<Test>::fill(&3, 100, Preservation::Preserve);

			// Then: satellite has accumulated all funds
			assert_eq!(Balances::free_balance(satellite), 170);
			// ... accounts have their balance correctly updated
			assert_eq!(Balances::free_balance(1), 80);
			assert_eq!(Balances::free_balance(2), 150);
			assert_eq!(Balances::free_balance(3), 200);
			// ... and events are emitted
			System::assert_has_event(
				Event::<Test>::FundsAccumulated { from: 1, amount: 20 }.into(),
			);
			System::assert_has_event(
				Event::<Test>::FundsAccumulated { from: 2, amount: 50 }.into(),
			);
			System::assert_has_event(
				Event::<Test>::FundsAccumulated { from: 3, amount: 100 }.into(),
			);
		});
	}

	#[test]
	fn fill_with_insufficient_balance_is_noop() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();

			// Given: account 1 has 100, satellite has 0
			assert_eq!(Balances::free_balance(1), 100);
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: try to fill 150 (more than balance)
			AccumulateInSatellite::<Test>::fill(&1, 150, Preservation::Preserve);

			// Then: balances unchanged (infallible no-op)
			assert_eq!(Balances::free_balance(1), 100);
			assert_eq!(Balances::free_balance(satellite), 0);
		});
	}

	#[test]
	fn fill_with_zero_amount_succeeds() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();

			// Given: account 1 has 100, satellite has 0
			assert_eq!(Balances::free_balance(1), 100);
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: fill 0 from account 1
			AccumulateInSatellite::<Test>::fill(&1, 0, Preservation::Preserve);

			// Then: balances unchanged (no-op)
			assert_eq!(Balances::free_balance(1), 100);
			assert_eq!(Balances::free_balance(satellite), 0);
		});
	}

	#[test]
	fn fill_with_expendable_allows_full_drain() {
		new_test_ext().execute_with(|| {
			System::set_block_number(1);
			let satellite = DapSatellite::satellite_account();

			// Given: account 1 has 100
			assert_eq!(Balances::free_balance(1), 100);

			// When: fill full balance with Expendable (allows going to 0)
			AccumulateInSatellite::<Test>::fill(&1, 100, Preservation::Expendable);

			// Then: account 1 is empty, satellite has 100
			assert_eq!(Balances::free_balance(1), 0);
			assert_eq!(Balances::free_balance(satellite), 100);
		});
	}

	#[test]
	fn fill_with_preserve_respects_existential_deposit() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();

			// Given: account 1 has 100, ED is 1 (from TestDefaultConfig)
			assert_eq!(Balances::free_balance(1), 100);
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: try to fill 100 with Preserve (would go below ED)
			AccumulateInSatellite::<Test>::fill(&1, 100, Preservation::Preserve);

			// Then: balances unchanged (infallible - would have killed account)
			assert_eq!(Balances::free_balance(1), 100);
			assert_eq!(Balances::free_balance(satellite), 0);

			// But filling 99 works (leaves 1 for ED)
			AccumulateInSatellite::<Test>::fill(&1, 99, Preservation::Preserve);
			assert_eq!(Balances::free_balance(1), 1);
			assert_eq!(Balances::free_balance(satellite), 99);
		});
	}

	// ===== SlashToSatellite tests =====

	#[test]
	fn slash_to_satellite_deposits_to_satellite() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();

			// Given: satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: multiple slashes occur
			let credit1 = <Balances as Balanced<u64>>::issue(30);
			SlashToSatellite::<Test>::on_unbalanced(credit1);

			let credit2 = <Balances as Balanced<u64>>::issue(20);
			SlashToSatellite::<Test>::on_unbalanced(credit2);

			let credit3 = <Balances as Balanced<u64>>::issue(50);
			SlashToSatellite::<Test>::on_unbalanced(credit3);

			// Then: satellite has accumulated all slashes (30 + 20 + 50 = 100)
			assert_eq!(Balances::free_balance(satellite), 100);
		});
	}

	#[test]
	fn slash_to_satellite_handles_zero_amount() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();

			// Given: satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: slash with zero amount
			let credit = <Balances as Balanced<u64>>::issue(0);
			SlashToSatellite::<Test>::on_unbalanced(credit);

			// Then: satellite still has 0 (no-op)
			assert_eq!(Balances::free_balance(satellite), 0);
		});
	}

	// ===== SinkToSatellite tests =====

	#[test]
	fn sink_to_satellite_deposits_to_satellite() {
		new_test_ext().execute_with(|| {
			let satellite = DapSatellite::satellite_account();

			// Given: accounts have balances, satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: multiple sinks occur from different accounts
			let imbalance1 = <Balances as CurrencyT<u64>>::withdraw(
				&1,
				30,
				WithdrawReasons::FEE,
				ExistenceRequirement::KeepAlive,
			)
			.unwrap();
			SinkToSatellite::<Test, Balances>::on_unbalanced(imbalance1);

			let imbalance2 = <Balances as CurrencyT<u64>>::withdraw(
				&2,
				50,
				WithdrawReasons::FEE,
				ExistenceRequirement::KeepAlive,
			)
			.unwrap();
			SinkToSatellite::<Test, Balances>::on_unbalanced(imbalance2);

			// Then: satellite has accumulated all sinks (30 + 50 = 80)
			assert_eq!(Balances::free_balance(satellite), 80);
			assert_eq!(Balances::free_balance(1), 70);
			assert_eq!(Balances::free_balance(2), 150);
		});
	}

	// ===== DealWithFeesSplit tests =====

	// Thread-local storage for tracking what OtherHandler receives
	thread_local! {
		static OTHER_HANDLER_RECEIVED: Cell<u64> = const { Cell::new(0) };
	}

	/// Mock handler that tracks how much it receives
	struct MockOtherHandler;
	impl OnUnbalanced<CreditOf<Test>> for MockOtherHandler {
		fn on_unbalanced(amount: CreditOf<Test>) {
			OTHER_HANDLER_RECEIVED.with(|r| r.set(r.get() + amount.peek()));
			// Drop the credit (it would normally be handled by the real handler)
			drop(amount);
		}
	}

	fn reset_other_handler() {
		OTHER_HANDLER_RECEIVED.with(|r| r.set(0));
	}

	fn get_other_handler_received() -> u64 {
		OTHER_HANDLER_RECEIVED.with(|r| r.get())
	}

	parameter_types! {
		pub const ZeroPercent: u32 = 0;
		pub const FiftyPercent: u32 = 50;
		pub const HundredPercent: u32 = 100;
	}

	#[test]
	fn deal_with_fees_split_zero_percent_to_dap() {
		new_test_ext().execute_with(|| {
			reset_other_handler();
			let satellite = DapSatellite::satellite_account();

			// Given: satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: fees of 100 with 0% to DAP (all to other handler) + tips of 50
			// Tips should ALWAYS go to other handler, regardless of DAP percent
			let fees = <Balances as Balanced<u64>>::issue(100);
			let tips = <Balances as Balanced<u64>>::issue(50);
			<DealWithFeesSplit<Test, ZeroPercent, MockOtherHandler> as OnUnbalanced<_>>::on_unbalanceds(
				[fees, tips].into_iter(),
			);

			// Then: satellite gets 0, other handler gets 150 (100% fees + tips)
			assert_eq!(Balances::free_balance(satellite), 0);
			assert_eq!(get_other_handler_received(), 150);
		});
	}

	#[test]
	fn deal_with_fees_split_hundred_percent_to_dap() {
		new_test_ext().execute_with(|| {
			reset_other_handler();
			let satellite = DapSatellite::satellite_account();

			// Given: satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: fees of 100 with 100% to DAP + tips of 50
			// Tips should ALWAYS go to other handler, regardless of DAP percent
			let fees = <Balances as Balanced<u64>>::issue(100);
			let tips = <Balances as Balanced<u64>>::issue(50);
			<DealWithFeesSplit<Test, HundredPercent, MockOtherHandler> as OnUnbalanced<_>>::on_unbalanceds(
				[fees, tips].into_iter(),
			);

			// Then: satellite gets 100 (fees), other handler gets 50 (tips)
			assert_eq!(Balances::free_balance(satellite), 100);
			assert_eq!(get_other_handler_received(), 50);
		});
	}

	#[test]
	fn deal_with_fees_split_fifty_percent() {
		new_test_ext().execute_with(|| {
			reset_other_handler();
			let satellite = DapSatellite::satellite_account();

			// Given: satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: fees of 100 with 50% to DAP + tips of 40
			// Fees split 50/50, tips 100% to other handler
			let fees = <Balances as Balanced<u64>>::issue(100);
			let tips = <Balances as Balanced<u64>>::issue(40);
			<DealWithFeesSplit<Test, FiftyPercent, MockOtherHandler> as OnUnbalanced<_>>::on_unbalanceds(
				[fees, tips].into_iter(),
			);

			// Then: satellite gets 50 (half of fees), other handler gets 90 (half of fees + tips)
			assert_eq!(Balances::free_balance(satellite), 50);
			assert_eq!(get_other_handler_received(), 90);
		});
	}

	#[test]
	fn deal_with_fees_split_handles_empty_iterator() {
		new_test_ext().execute_with(|| {
			reset_other_handler();
			let satellite = DapSatellite::satellite_account();

			// Given: satellite has 0
			assert_eq!(Balances::free_balance(satellite), 0);

			// When: no fees, no tips (empty iterator)
			<DealWithFeesSplit<Test, FiftyPercent, MockOtherHandler> as OnUnbalanced<_>>::on_unbalanceds(
				core::iter::empty(),
			);

			// Then: nothing happens
			assert_eq!(Balances::free_balance(satellite), 0);
			assert_eq!(get_other_handler_received(), 0);
		});
	}
}
