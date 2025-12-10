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

//! Traits for returning funds to an issuance system.
//!
//! This module provides abstractions for returning funds (burns, slashing) in a way that can be
//! configured differently per runtime.
//!
//! Two main patterns:
//! - **Direct burn**: Traditional approach where funds are destroyed on demand
//! - **Buffer-based**: Funds are returned to a buffer for reuse

use crate::traits::tokens::{fungible, Fortitude, Precision, Preservation};
use core::marker::PhantomData;
use sp_runtime::Saturating;

/// Trait for handling burned funds.
///
/// This trait is used by `pallet_balances::burn_from` to handle funds after they have been
/// removed from the source account. Implementations can either:
/// - Reduce total issuance (traditional burning)
/// - Credit to a buffer account (DAP-style systems)
///
/// The key distinction from `FundingSink::fill` is that `on_burned` is called AFTER
/// the funds have already been removed from the source account via `decrease_balance`.
pub trait BurnHandler<AccountId, Balance> {
	/// Handle funds that have been burned from an account.
	///
	/// Called by `burn_from` after the source account's balance has been decreased.
	/// The implementation should either:
	/// - Reduce total issuance (for actual burning)
	/// - Credit the amount to a buffer account (for DAP systems)
	///
	/// This operation is infallible.
	fn on_burned(who: &AccountId, amount: Balance);
}

/// Trait for moving funds into an issuance buffer or burning them.
///
/// Implementations can either burn directly or transfer to a buffer for reuse.
/// This trait is infallible - implementations must handle any errors internally.
///
/// Pairs with future `FundingSource::drain()` for withdrawing from the buffer.
pub trait FundingSink<AccountId, Balance>: BurnHandler<AccountId, Balance> {
	/// Fill the sink with funds from the given account.
	///
	/// This could mean burning the funds or transferring them to a buffer account.
	/// The operation is infallible - any errors are handled internally.
	///
	/// # Parameters
	/// - `from`: The account to take funds from
	/// - `amount`: The amount to fill
	/// - `preservation`: Whether to preserve the source account (Preserve = keep alive, Expendable
	///   = allow death)
	fn fill(from: &AccountId, amount: Balance, preservation: Preservation);
}

/// Direct burning implementation of `BurnHandler` and `FundingSink`.
///
/// This implementation burns tokens directly, reducing total issuance.
/// Used for traditional burn systems (e.g., Kusama).
///
/// # Type Parameters
///
/// * `Currency` - The currency type that implements `Mutate` and `Unbalanced`
/// * `AccountId` - The account identifier type
pub struct DirectBurn<Currency, AccountId>(PhantomData<(Currency, AccountId)>);

impl<Currency, AccountId> BurnHandler<AccountId, Currency::Balance>
	for DirectBurn<Currency, AccountId>
where
	Currency: fungible::Unbalanced<AccountId>,
	AccountId: Eq,
{
	fn on_burned(_who: &AccountId, amount: Currency::Balance) {
		// Reduce total issuance - funds are permanently destroyed
		Currency::set_total_issuance(Currency::total_issuance().saturating_sub(amount));
	}
}

impl<Currency, AccountId> FundingSink<AccountId, Currency::Balance>
	for DirectBurn<Currency, AccountId>
where
	Currency: fungible::Mutate<AccountId> + fungible::Unbalanced<AccountId>,
	AccountId: Eq,
{
	fn fill(from: &AccountId, amount: Currency::Balance, preservation: Preservation) {
		// Best-effort burn. If it fails (e.g., insufficient funds), the funds remain with the
		// account.
		let _ =
			Currency::burn_from(from, amount, preservation, Precision::Exact, Fortitude::Polite);
	}
}

/// No-op implementation of `BurnHandler` for unit type.
impl<AccountId, Balance> BurnHandler<AccountId, Balance> for () {
	fn on_burned(_who: &AccountId, _amount: Balance) {}
}

/// No-op implementation of `FundingSink` for unit type.
/// Used for testing or when no sink behavior is needed.
impl<AccountId, Balance> FundingSink<AccountId, Balance> for () {
	fn fill(_from: &AccountId, _amount: Balance, _preservation: Preservation) {}
}
