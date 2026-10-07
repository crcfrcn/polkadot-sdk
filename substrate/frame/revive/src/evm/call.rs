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

//! Functionality to decode an eth transaction into an dispatchable call.

use crate::{
	BalanceOf, CallOf, Config, GenericTransaction, LOG_TARGET, Pallet, RUNTIME_PALLETS_ADDR,
	Weight, Zero,
	evm::{
		TYPE_LEGACY,
		fees::{InfoT, compute_max_integer_quotient},
		runtime::SetWeightLimit,
	},
	extract_code_and_data,
};
use alloc::{boxed::Box, vec::Vec};
use codec::DecodeLimit;
use frame_support::MAX_EXTRINSIC_DEPTH;
use sp_core::{Get, U256};
use sp_runtime::{SaturatedConversion, transaction_validity::InvalidTransaction};

/// Result of decoding an eth transaction into a dispatchable call.
pub struct CallInfo<T: Config> {
	/// The dispatchable call with the correct weights assigned.
	///
	/// This will be either `eth_call` or `eth_instantiate_with_code`.
	pub call: CallOf<T>,
	/// The weight that was set inside [`Self::call`].
	pub weight_limit: Weight,
	/// The encoded length of the bare transaction carrying the ethereum payload.
	pub encoded_len: u32,
	/// The adjusted transaction fee of [`Self::call`].
	pub tx_fee: BalanceOf<T>,
	/// The additional storage deposit to be deposited into the txhold.
	pub storage_deposit: BalanceOf<T>,
	/// The ethereum gas limit of the transaction.
	pub eth_gas_limit: U256,
}

/// Mode for creating a call from an ethereum transaction.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CreateCallMode {
	/// Mode for extrinsic execution. Carries the encoding length of the extrinsic and the
	/// RLP-encoded Ethereum transaction
	ExtrinsicExecution(u32, Vec<u8>),
	/// Mode for dry running
	DryRun,
}

impl GenericTransaction {
	/// 固定兼容价格只用于钱包费用刻度；优先费为零，Legacy 必须使用准确价格。
	pub(crate) fn native_gas_price<T: Config>(&self) -> Result<U256, InvalidTransaction> {
		let price = super::fees::native_gas_price::<T>();
		if self.max_priority_fee_per_gas.is_some_and(|tip| !tip.is_zero()) {
			return Err(InvalidTransaction::Payment);
		}
		if let Some(cap) = self.max_fee_per_gas {
			if cap < price {
				return Err(InvalidTransaction::Payment);
			}
		} else if self.gas_price.is_some_and(|value| value != price) {
			return Err(InvalidTransaction::Payment);
		}
		Ok(price)
	}

	/// Decode `tx` into a dispatchable call.
	pub fn into_call<T>(self, mode: CreateCallMode) -> Result<CallInfo<T>, InvalidTransaction>
	where
		T: Config,
		CallOf<T>: SetWeightLimit,
	{
		// 原生路径必须绑定真实费用路由，测试空实现不能打开入口。
		if T::StrictNativeBalance::get() && !T::FeeInfo::native_fee_enabled() {
			return Err(InvalidTransaction::Payment);
		}
		crate::BalanceWithDust::<crate::BalanceOf<T>>::ensure_policy::<T>()
			.map_err(|_| InvalidTransaction::Payment)?;
		let is_dry_run = matches!(mode, CreateCallMode::DryRun);
		let base_fee = <Pallet<T>>::evm_base_fee();
		if T::StrictNativeBalance::get() {
			self.native_gas_price::<T>()?;
		}

		// 原生整单位策略必须携带准确ChainId，禁止无链签名跨链重放。
		// 其他SDK链仍保留上游允许未保护Legacy交易的原行为。
		match (self.chain_id, self.r#type.as_ref()) {
			(None, Some(super::Byte(TYPE_LEGACY))) if !T::StrictNativeBalance::get() => {},
			(Some(chain_id), ..) => {
				if chain_id != <T as Config>::ChainId::get().into() {
					log::debug!(target: LOG_TARGET, "Invalid chain_id {chain_id:?}");
					return Err(InvalidTransaction::Call);
				}
			},
			(None, ..) => {
				log::debug!(target: LOG_TARGET, "Invalid chain_id None");
				return Err(InvalidTransaction::Call);
			},
		}

		let Some(gas) = self.gas else {
			log::debug!(target: LOG_TARGET, "No gas provided");
			return Err(InvalidTransaction::Call);
		};

		// 两种收费路径都提供非零价格；原生路径使用固定钱包费用刻度。
		let Some(effective_gas_price) = self.gas_price else {
			log::debug!(target: LOG_TARGET, "No gas_price provided.");
			return Err(InvalidTransaction::Payment);
		};

		if effective_gas_price < base_fee {
			log::debug!(
				target: LOG_TARGET,
				"Specified gas_price is too low. effective_gas_price={effective_gas_price} base_fee={base_fee}"
			);
			return Err(InvalidTransaction::Payment);
		}

		let (encoded_len, transaction_encoded) =
			if let CreateCallMode::ExtrinsicExecution(encoded_len, transaction_encoded) = mode {
				(encoded_len, transaction_encoded)
			} else {
				// For dry runs, we need to ensure that the RLP encoding length is at least the
				// length of the encoding of the actual transaction submitted later
				let mut maximized_tx = self.clone();
				// 原生查询也为签名费用上限字段预留最大编码长度。
				let maximized_base_fee = if T::StrictNativeBalance::get() {
					U256::MAX
				} else {
					base_fee.saturating_mul(256.into())
				};
				maximized_tx.gas = Some(u64::MAX.into());
				maximized_tx.gas_price = Some(maximized_base_fee);
				maximized_tx.max_priority_fee_per_gas = Some(maximized_base_fee);
				if T::StrictNativeBalance::get() {
					maximized_tx.max_fee_per_gas = Some(U256::MAX);
				}

				let unsigned_tx = maximized_tx.try_into_unsigned().map_err(|_| {
					log::debug!(target: LOG_TARGET, "Invalid transaction type.");
					InvalidTransaction::Call
				})?;
				let transaction_encoded = unsigned_tx.dummy_signed_payload();

				let eth_transact_call =
					crate::Call::<T>::eth_transact { payload: transaction_encoded.clone() };
				(<T as Config>::FeeInfo::encoded_len(eth_transact_call.into()), transaction_encoded)
			};

		let value = self.value.unwrap_or_default();
		let native_signer = if T::StrictNativeBalance::get() {
			crate::BalanceWithDust::<BalanceOf<T>>::from_value::<T>(value)
				.map_err(|_| InvalidTransaction::Payment)?;
			let from = self.from.ok_or(InvalidTransaction::BadSigner)?;
			Some(if is_dry_run {
				<T::AddressMapper as crate::AddressMapper<T>>::to_account_id(&from)
			} else {
				<T::AddressMapper as crate::AddressMapper<T>>::to_fallback_account_id(&from)
			})
		} else {
			None
		};
		let data = self.input.to_vec();

		let mut call = if let Some(dest) = self.to {
			if dest == RUNTIME_PALLETS_ADDR {
				// 原生制度禁止未经业务授权与唯一收费路由的包装派发。
				if T::StrictNativeBalance::get() {
					return Err(InvalidTransaction::Call);
				}
				let call =
					CallOf::<T>::decode_all_with_depth_limit(MAX_EXTRINSIC_DEPTH, &mut &data[..])
						.map_err(|_| {
						log::debug!(target: LOG_TARGET, "Failed to decode data as Call");
						InvalidTransaction::Call
					})?;

				if !value.is_zero() {
					log::debug!(target: LOG_TARGET, "Runtime pallets address cannot be called with value");
					return Err(InvalidTransaction::Call);
				}

				crate::Call::eth_substrate_call::<T> { call: Box::new(call), transaction_encoded }
					.into()
			} else {
				let call = crate::Call::eth_call::<T> {
					dest,
					value,
					weight_limit: Zero::zero(),
					eth_gas_limit: gas,
					data,
					transaction_encoded,
					effective_gas_price,
					encoded_len,
				}
				.into();
				call
			}
		} else {
			let (code, data) = if data.starts_with(&polkavm_common::program::BLOB_MAGIC) {
				let Some((code, data)) = extract_code_and_data(&data) else {
					log::debug!(target: LOG_TARGET, "Failed to extract polkavm code & data");
					return Err(InvalidTransaction::Call);
				};
				(code, data)
			} else {
				(data, Default::default())
			};

			let call = crate::Call::eth_instantiate_with_code::<T> {
				value,
				weight_limit: Zero::zero(),
				eth_gas_limit: gas,
				code,
				data,
				transaction_encoded,
				effective_gas_price,
				encoded_len,
			}
			.into();

			call
		};

		if let Some(who) = native_signer {
			// 资源预算仍受双维 Weight 约束；费用只由 Runtime 现有路由报价。
			let resource_gas: u64 =
				gas.try_into().map_err(|_| InvalidTransaction::ExhaustsResources)?;
			let budget = super::fees::resource_gas_to_weight::<T>(resource_gas);
			let info = T::FeeInfo::dispatch_info(&call);
			let base = T::BlockWeights::get().get(info.class).base_extrinsic;
			let overhead = info
				.total_weight()
				.saturating_add(base)
				.saturating_add(Weight::from_parts(0, encoded_len as u64));
			let available =
				budget.checked_sub(&overhead).ok_or(InvalidTransaction::ExhaustsResources)?;
			let max = Pallet::<T>::evm_max_extrinsic_weight()
				.checked_sub(&overhead)
				.ok_or(InvalidTransaction::ExhaustsResources)?;
			let weight_limit = available.min(max);
			call.set_weight_limit(weight_limit);
			let tx_fee = T::FeeInfo::native_quote(&who, &call)?;
			let fee_gas = super::fees::native_fee_to_gas::<T>(tx_fee)?;
			if U256::from(fee_gas) > gas {
				return Err(InvalidTransaction::Payment);
			}
			let ceiling = self
				.max_fee_per_gas
				.unwrap_or(effective_gas_price)
				.checked_mul(gas)
				.ok_or(InvalidTransaction::Payment)?;
			let required: U256 = tx_fee.into();
			let required = required
				.checked_mul(T::NativeToEthRatio::get().into())
				.ok_or(InvalidTransaction::Payment)?;
			// 签名费用上限不能被估算或查询路径绕过。
			if required > ceiling {
				return Err(InvalidTransaction::Payment);
			}
			return Ok(CallInfo {
				call,
				weight_limit,
				encoded_len,
				tx_fee,
				storage_deposit: Zero::zero(),
				eth_gas_limit: gas,
			});
		}

		// the fee as signed off by the eth wallet. we cannot consume more.
		let eth_fee = effective_gas_price.checked_mul(gas).ok_or(InvalidTransaction::Payment)?
			/ <T as Config>::NativeToEthRatio::get();

		let weight_limit = {
			let fixed_fee = <T as Config>::FeeInfo::fixed_fee(encoded_len as u32);
			let info = <T as Config>::FeeInfo::dispatch_info(&call);

			let remaining_fee = {
				let adjusted = eth_fee.checked_sub(fixed_fee.into()).ok_or_else(|| {
				log::debug!(target: LOG_TARGET, "Not enough gas supplied to cover base and len fee. eth_fee={eth_fee:?} fixed_fee={fixed_fee:?}");
				InvalidTransaction::Payment
			})?;

				let unadjusted = compute_max_integer_quotient(
					<T as Config>::FeeInfo::next_fee_multiplier(),
					<BalanceOf<T>>::saturated_from(adjusted),
				);

				unadjusted
			};
			let remaining_fee_weight = <T as Config>::FeeInfo::fee_to_weight(remaining_fee);
			let weight_limit = remaining_fee_weight
			.checked_sub(&info.total_weight()).ok_or_else(|| {
			log::debug!(target: LOG_TARGET, "Not enough gas supplied to cover the weight ({:?}) of the extrinsic. remaining_fee_weight: {remaining_fee_weight:?}", info.total_weight(),);
			InvalidTransaction::Payment
		})?;

			call.set_weight_limit(weight_limit);

			if !is_dry_run {
				let max_weight = <Pallet<T>>::evm_max_extrinsic_weight();
				let info = <T as Config>::FeeInfo::dispatch_info(&call);
				let overweight_by = info.total_weight().saturating_sub(max_weight);
				let capped_weight = weight_limit.saturating_sub(overweight_by);
				call.set_weight_limit(capped_weight);
				capped_weight
			} else {
				weight_limit
			}
		};

		// the overall fee of the extrinsic including the gas limit
		let tx_fee = <T as Config>::FeeInfo::tx_fee(encoded_len, &call);

		// the leftover we make available to the deposit collection system
		let storage_deposit = eth_fee.checked_sub(tx_fee.into()).ok_or_else(|| {
			log::error!(target: LOG_TARGET, "The eth_fee={eth_fee:?} is smaller than the tx_fee={tx_fee:?}. This is a bug.");
			InvalidTransaction::Payment
		})?.saturated_into();

		Ok(CallInfo {
			call,
			weight_limit,
			encoded_len,
			tx_fee,
			storage_deposit,
			eth_gas_limit: gas,
		})
	}
}
