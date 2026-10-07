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

mod block_hash;
mod deposit_payment;
mod pallet_dummy;
mod precompiles;
mod pvm;
mod sol;
mod stipends;

use std::collections::HashMap;

use crate::{
	self as pallet_revive, AccountId32Mapper, AddressMapper, BalanceOf, BalanceWithDust, Call,
	CodeInfoOf, Config, DelegateInfo, ExecOrigin as Origin, ExecReturnValue, GenesisConfig,
	OriginFor, Pallet, PristineCode,
	deposit_payment::PGasDeposit,
	evm::{
		fees::{BlockRatioFee, Info as FeeInfo},
		runtime::{EthExtra, SetWeightLimit},
	},
	genesis::{Account, ContractData},
	mock::MockHandler,
	test_utils::*,
};
use frame_support::{
	DefaultNoBound, assert_ok, derive_impl,
	pallet_prelude::EnsureOrigin,
	parameter_types,
	traits::{
		AsEnsureOriginWithArg, ConstU32, ConstU128, FindAuthor, OriginTrait, StorageVersion,
		tokens::imbalance::ResolveTo,
	},
	weights::{FixedFee, Weight, constants::WEIGHT_REF_TIME_PER_SECOND},
};
use pallet_revive_fixtures::compile_module;
use pallet_transaction_payment::{ChargeTransactionPayment, ConstFeeMultiplier, Multiplier};
use sp_core::{H160, U256};
use sp_keystore::{KeystoreExt, testing::MemoryKeystore};
use sp_runtime::{
	AccountId32, BuildStorage, FixedU128, MultiAddress, MultiSignature, Perbill, Storage,
	generic::Header,
	traits::{BlakeTwo256, Convert, IdentityLookup, One},
};

pub type Address = MultiAddress<AccountId32, u32>;
pub type Block = sp_runtime::generic::Block<Header<u64, BlakeTwo256>, UncheckedExtrinsic>;
pub type Signature = MultiSignature;
pub type SignedExtra = (
	frame_system::CheckNonce<Test>,
	ChargeTransactionPayment<Test>,
	crate::evm::tx_extension::SetOrigin<Test>,
);
pub type UncheckedExtrinsic =
	crate::evm::runtime::UncheckedExtrinsic<Address, Signature, EthExtraImpl>;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EthExtraImpl;

impl EthExtra for EthExtraImpl {
	type Config = Test;
	type ExtensionV0 = SignedExtra;
	type ExtensionOtherVersions = sp_runtime::traits::InvalidVersion;

	fn get_eth_extension(nonce: u32, tip: BalanceOf<Test>) -> Self::ExtensionV0 {
		(
			frame_system::CheckNonce::from(nonce),
			ChargeTransactionPayment::from(tip),
			crate::evm::tx_extension::SetOrigin::<Test>::new_from_eth_transaction(),
		)
	}
}

frame_support::construct_runtime!(
	pub enum Test
	{
		System: frame_system,
		Balances: pallet_balances,
		Timestamp: pallet_timestamp,
		Utility: pallet_utility,
		Contracts: pallet_revive,
		Proxy: pallet_proxy,
		TransactionPayment: pallet_transaction_payment,
		Assets: pallet_assets,
		AssetsHolder: pallet_assets_holder,
		AssetsFreezer: pallet_assets_freezer,
		Dummy: pallet_dummy
	}
);

#[macro_export]
macro_rules! assert_return_code {
	( $x:expr , $y:expr $(,)? ) => {{
		assert_eq!(u32::from_le_bytes($x.data[..].try_into().unwrap()), $y as u32);
	}};
}

#[macro_export]
macro_rules! assert_refcount {
	( $code_hash:expr , $should:expr $(,)? ) => {{
		let is = crate::CodeInfoOf::<Test>::get($code_hash).map(|m| m.refcount()).unwrap();
		assert_eq!(is, $should);
	}};
}

pub mod test_utils {
	use super::{
		BalanceWithDust, CodeHashLockupDepositPercent, Contracts, DepositPerByte, DepositPerItem,
		Test,
	};
	use crate::{
		AccountInfo, AccountInfoOf, BalanceOf, CodeInfo, CodeInfoOf, Config, ContractInfo,
		PristineCode, address::AddressMapper, exec::AccountIdOf,
	};
	use codec::{Encode, MaxEncodedLen};
	use frame_support::traits::fungible::{InspectHold, Mutate};
	use sp_core::H160;

	pub fn place_contract(address: &AccountIdOf<Test>, code_hash: sp_core::H256) {
		set_balance(address, Contracts::min_balance() * 10);
		<CodeInfoOf<Test>>::insert(code_hash, CodeInfo::new(address.clone()));
		let address =
			<<Test as Config>::AddressMapper as AddressMapper<Test>>::to_address(&address);
		let contract = <ContractInfo<Test>>::new(&address, 0, code_hash).unwrap();
		AccountInfo::<Test>::insert_contract(&address, contract);
	}
	pub fn set_balance(who: &AccountIdOf<Test>, amount: u128) {
		let _ = <Test as Config>::Currency::set_balance(who, amount);
	}
	pub fn get_balance(who: &AccountIdOf<Test>) -> u128 {
		<Test as Config>::Currency::free_balance(who)
	}
	pub fn get_balance_on_hold(
		reason: &<Test as Config>::RuntimeHoldReason,
		who: &AccountIdOf<Test>,
	) -> u128 {
		<Test as Config>::Currency::balance_on_hold(reason.into(), who)
	}
	pub fn get_contract(addr: &H160) -> ContractInfo<Test> {
		get_contract_checked(addr).unwrap()
	}
	pub fn get_contract_checked(addr: &H160) -> Option<ContractInfo<Test>> {
		AccountInfo::<Test>::load_contract(addr)
	}
	pub fn get_code_deposit(code_hash: &sp_core::H256) -> BalanceOf<Test> {
		crate::CodeInfoOf::<Test>::get(code_hash).unwrap().deposit()
	}
	pub fn lockup_deposit(code_hash: &sp_core::H256) -> BalanceOf<Test> {
		CodeHashLockupDepositPercent::get().mul_ceil(get_code_deposit(code_hash)).into()
	}
	pub fn contract_base_deposit(addr: &H160) -> BalanceOf<Test> {
		let contract_info = self::get_contract(&addr);
		let info_size = contract_info.encoded_size() as u128;
		let code_deposit = CodeHashLockupDepositPercent::get()
			.mul_ceil(get_code_deposit(&contract_info.code_hash));
		let deposit = DepositPerByte::get()
			.saturating_mul(info_size)
			.saturating_add(DepositPerItem::get())
			.saturating_add(code_deposit);
		let immutable_size = contract_info.immutable_data_len() as u128;
		if immutable_size > 0 {
			let immutable_deposit = DepositPerByte::get()
				.saturating_mul(immutable_size)
				.saturating_add(DepositPerItem::get());
			deposit.saturating_add(immutable_deposit)
		} else {
			deposit
		}
	}
	pub fn expected_deposit(code_len: usize) -> u128 {
		// For code_info, the deposit for max_encoded_len is taken.
		let code_info_len = CodeInfo::<Test>::max_encoded_len() as u128;
		// Calculate deposit to be reserved.
		// We add 2 storage items: one for code, other for code_info
		DepositPerByte::get().saturating_mul(code_len as u128 + code_info_len)
			+ DepositPerItem::get().saturating_mul(2)
	}
	pub fn ensure_stored(code_hash: sp_core::H256) -> usize {
		// Assert that code_info is stored
		assert!(CodeInfoOf::<Test>::contains_key(&code_hash));
		// Assert that contract code is stored, and get its size.
		PristineCode::<Test>::try_get(&code_hash).unwrap().len()
	}
	pub fn u256_bytes(u: u64) -> [u8; 32] {
		let mut buffer = [0u8; 32];
		let bytes = u.to_le_bytes();
		buffer[..8].copy_from_slice(&bytes);
		buffer
	}

	pub fn set_balance_with_dust(address: &H160, value: BalanceWithDust<BalanceOf<Test>>) {
		use frame_support::traits::Currency;
		let ed = <Test as Config>::Currency::minimum_balance();
		let (value, dust) = value.deconstruct();
		let account_id = <Test as Config>::AddressMapper::to_account_id(&address);
		<Test as Config>::Currency::set_balance(&account_id, ed + value);
		if dust > 0 {
			AccountInfoOf::<Test>::mutate(&address, |account| {
				if let Some(account) = account {
					account.dust = dust;
				} else {
					*account = Some(AccountInfo { dust, ..Default::default() });
				}
			});
		}
	}
}

pub(crate) mod builder {
	use super::Test;
	use crate::{
		Code,
		test_utils::{ALICE, builder::*},
		tests::RuntimeOrigin,
	};
	use sp_core::{H160, H256};

	pub fn bare_instantiate(code: Code) -> BareInstantiateBuilder<Test> {
		BareInstantiateBuilder::<Test>::bare_instantiate(RuntimeOrigin::signed(ALICE), code)
	}

	pub fn bare_call(dest: H160) -> BareCallBuilder<Test> {
		BareCallBuilder::<Test>::bare_call(RuntimeOrigin::signed(ALICE), dest)
	}

	pub fn instantiate_with_code(code: Vec<u8>) -> InstantiateWithCodeBuilder<Test> {
		InstantiateWithCodeBuilder::<Test>::instantiate_with_code(
			RuntimeOrigin::signed(ALICE),
			code,
		)
	}

	pub fn instantiate(code_hash: H256) -> InstantiateBuilder<Test> {
		InstantiateBuilder::<Test>::instantiate(RuntimeOrigin::signed(ALICE), code_hash)
	}

	pub fn call(dest: H160) -> CallBuilder<Test> {
		CallBuilder::<Test>::call(RuntimeOrigin::signed(ALICE), dest)
	}

	pub fn eth_call(dest: H160) -> EthCallBuilder<Test> {
		EthCallBuilder::<Test>::eth_call(crate::Origin::<Test>::EthTransaction(ALICE).into(), dest)
	}

	pub fn eth_instantiate_with_code(code: Vec<u8>) -> EthInstantiateWithCodeBuilder<Test> {
		EthInstantiateWithCodeBuilder::<Test>::eth_instantiate_with_code(
			crate::Origin::<Test>::EthTransaction(ALICE).into(),
			code,
		)
	}
}

impl Test {
	pub fn set_allow_evm_bytecode(allow_evm_bytecode: bool) {
		ALLOW_EVM_BYTECODE.with(|v| *v.borrow_mut() = allow_evm_bytecode);
	}
}

parameter_types! {
	pub BlockWeights: frame_system::limits::BlockWeights =
		frame_system::limits::BlockWeights::simple_max(
			Weight::from_parts(2 * WEIGHT_REF_TIME_PER_SECOND, 10 * 1024 * 1024),
		);
	pub static ExistentialDeposit: u128 = 1;
}

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
	type Block = Block;
	type BlockWeights = BlockWeights;
	type AccountId = AccountId32;
	type Lookup = IdentityLookup<Self::AccountId>;
	type AccountData = pallet_balances::AccountData<u128>;
	type OnNewAccount = crate::AutoMapper<Test>;
	type OnKilledAccount = crate::AutoMapper<Test>;
}

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
	type Balance = u128;
	type ExistentialDeposit = ExistentialDeposit;
	type ReserveIdentifier = [u8; 8];
	type AccountStore = System;
	type RuntimeHoldReason = RuntimeHoldReason;
	type RuntimeFreezeReason = RuntimeFreezeReason;
	type FreezeIdentifier = RuntimeFreezeReason;
	type MaxFreezes = frame_support::traits::VariantCountOf<RuntimeFreezeReason>;
}

#[derive_impl(pallet_timestamp::config_preludes::TestDefaultConfig)]
impl pallet_timestamp::Config for Test {}

impl pallet_utility::Config for Test {
	type RuntimeEvent = RuntimeEvent;
	type RuntimeCall = RuntimeCall;
	type PalletsOrigin = OriginCaller;
	type WeightInfo = ();
}

impl pallet_proxy::Config for Test {
	type RuntimeEvent = RuntimeEvent;
	type RuntimeCall = RuntimeCall;
	type Currency = Balances;
	type ProxyType = ();
	type ProxyDepositBase = ConstU128<1>;
	type ProxyDepositFactor = ConstU128<1>;
	type MaxProxies = ConstU32<32>;
	type WeightInfo = ();
	type MaxPending = ConstU32<32>;
	type CallHasher = BlakeTwo256;
	type AnnouncementDepositBase = ConstU128<1>;
	type AnnouncementDepositFactor = ConstU128<1>;
	type BlockNumberProvider = frame_system::Pallet<Test>;
}

parameter_types! {
	pub FeeMultiplier: Multiplier = Multiplier::one();
}

#[derive_impl(pallet_transaction_payment::config_preludes::TestDefaultConfig)]
impl pallet_transaction_payment::Config for Test {
	type OnChargeTransaction = pallet_transaction_payment::FungibleAdapter<Balances, ()>;
	type WeightToFee = BlockRatioFee<2, 1, Self, u128>;
	type LengthToFee = FixedFee<100, <Self as pallet_balances::Config>::Balance>;
	type FeeMultiplierUpdate = ConstFeeMultiplier<FeeMultiplier>;
}

#[derive_impl(pallet_assets::config_preludes::TestDefaultConfig)]
impl pallet_assets::Config for Test {
	type Balance = u128;
	type Currency = Balances;
	type CreateOrigin = AsEnsureOriginWithArg<frame_system::EnsureSigned<AccountId32>>;
	type ForceOrigin = frame_system::EnsureRoot<AccountId32>;
	type Holder = AssetsHolder;
	type Freezer = AssetsFreezer;
}

impl pallet_assets_holder::Config for Test {
	type RuntimeHoldReason = RuntimeHoldReason;
	type RuntimeEvent = RuntimeEvent;
}

impl pallet_assets_freezer::Config for Test {
	type RuntimeFreezeReason = RuntimeFreezeReason;
	type RuntimeEvent = RuntimeEvent;
}

/// The PGAS asset id used by the test runtime.
pub const PGAS_ASSET_ID: u32 = 42;

parameter_types! {
	pub const PGasAssetId: u32 = PGAS_ASSET_ID;
	/// 10% of PGAS storage deposits are refunded, the rest is burned so users can't harvest
	/// free PGAS allowance from storage churn.
	pub const PGasRefundPercent: Perbill = Perbill::from_percent(10);
}

impl pallet_dummy::Config for Test {}

parameter_types! {
	pub static DepositPerByte: BalanceOf<Test> = 1;
	pub const DepositPerItem: BalanceOf<Test> = 2;
	pub const CodeHashLockupDepositPercent: Perbill = Perbill::from_percent(30);
	pub static ChainId: u64 = 448;
}

impl Convert<Weight, BalanceOf<Self>> for Test {
	fn convert(w: Weight) -> BalanceOf<Self> {
		w.ref_time().into()
	}
}

parameter_types! {
	pub static UploadAccount: Option<<Test as frame_system::Config>::AccountId> = None;
	pub static InstantiateAccount: Option<<Test as frame_system::Config>::AccountId> = None;
}

pub struct EnsureAccount<T, A>(core::marker::PhantomData<(T, A)>);
impl<T: Config, A: sp_core::Get<Option<crate::AccountIdOf<T>>>>
	EnsureOrigin<<T as frame_system::Config>::RuntimeOrigin> for EnsureAccount<T, A>
where
	<T as frame_system::Config>::AccountId: From<AccountId32>,
{
	type Success = T::AccountId;

	fn try_origin(o: OriginFor<T>) -> Result<Self::Success, OriginFor<T>> {
		let who = <frame_system::EnsureSigned<_> as EnsureOrigin<_>>::try_origin(o.clone())?;
		if matches!(A::get(), Some(a) if who != a) {
			return Err(o);
		}

		Ok(who)
	}

	#[cfg(feature = "runtime-benchmarks")]
	fn try_successful_origin() -> Result<OriginFor<T>, ()> {
		Err(())
	}
}
parameter_types! {
	pub static AllowEvmBytecode: bool = true;
	pub CheckingAccount: AccountId32 = BOB.clone();
	pub BurnDestination: AccountId32 = AccountId32::new([42u8; 32]);
	pub static DebugFlag: bool = false;
	pub static AutoMapFlag: bool = false;
}

impl FindAuthor<<Test as frame_system::Config>::AccountId> for Test {
	fn find_author<'a, I>(_digests: I) -> Option<<Test as frame_system::Config>::AccountId>
	where
		I: 'a + IntoIterator<Item = (frame_support::ConsensusEngineId, &'a [u8])>,
	{
		Some(EVE)
	}
}

#[derive_impl(crate::config_preludes::TestDefaultConfig)]
impl Config for Test {
	type Time = Timestamp;
	type AddressMapper = AccountId32Mapper<Self>;
	type Balance = u128;
	type Currency = Balances;
	type DepositPerByte = DepositPerByte;
	type DepositPerItem = DepositPerItem;
	type DepositPerChildTrieItem = DepositPerItem;
	type AllowEVMBytecode = AllowEvmBytecode;
	type UploadOrigin = EnsureAccount<Self, UploadAccount>;
	type InstantiateOrigin = EnsureAccount<Self, InstantiateAccount>;
	type CodeHashLockupDepositPercent = CodeHashLockupDepositPercent;
	type ChainId = ChainId;
	type FindAuthor = Test;
	type Precompiles = (precompiles::WithInfo<Self>, precompiles::NoInfo<Self>);
	type FeeInfo = FeeInfo<Address, Signature, EthExtraImpl>;
	type Deposit =
		PGasDeposit<Test, Assets, AssetsHolder, AssetsFreezer, PGasAssetId, PGasRefundPercent>;
	type DebugEnabled = DebugFlag;
	type AutoMap = AutoMapFlag;
	type OnBurn = ResolveTo<BurnDestination, Balances>;
}

impl TryFrom<RuntimeCall> for Call<Test> {
	type Error = ();

	fn try_from(value: RuntimeCall) -> Result<Self, Self::Error> {
		match value {
			RuntimeCall::Contracts(call) => Ok(call),
			_ => Err(()),
		}
	}
}

impl SetWeightLimit for RuntimeCall {
	fn set_weight_limit(&mut self, new_weight_limit: Weight) -> Weight {
		match self {
			Self::Contracts(
				Call::eth_call { weight_limit, .. }
				| Call::eth_instantiate_with_code { weight_limit, .. },
			) => {
				let old = *weight_limit;
				*weight_limit = new_weight_limit;
				old
			},
			_ => Default::default(),
		}
	}
}

pub struct ExtBuilder {
	existential_deposit: u128,
	storage_version: Option<StorageVersion>,
	code_hashes: Vec<sp_core::H256>,
	genesis_config: Option<crate::GenesisConfig<Test>>,
	genesis_state_overrides: Option<Storage>,
	next_fee_multiplier: Option<FixedU128>,
	pgas_balances: Vec<(AccountId32, u128)>,
	pgas_min_balance: u128,
}

impl Default for ExtBuilder {
	fn default() -> Self {
		Self {
			existential_deposit: ExistentialDeposit::get(),
			storage_version: None,
			code_hashes: vec![],
			genesis_config: Some(crate::GenesisConfig::<Test>::default()),
			genesis_state_overrides: None,
			next_fee_multiplier: None,
			pgas_balances: vec![],
			pgas_min_balance: 1,
		}
	}
}

impl ExtBuilder {
	/// The pallet genesis config to use, or None if you don't want to include it.
	pub fn genesis_config(mut self, config: Option<crate::GenesisConfig<Test>>) -> Self {
		self.genesis_config = config;
		self
	}
	pub fn existential_deposit(mut self, existential_deposit: u128) -> Self {
		self.existential_deposit = existential_deposit;
		self
	}
	pub fn with_code_hashes(mut self, code_hashes: Vec<sp_core::H256>) -> Self {
		self.code_hashes = code_hashes;
		self
	}
	pub fn with_next_fee_multiplier(mut self, next_fee_multiplier: FixedU128) -> Self {
		self.next_fee_multiplier = Some(next_fee_multiplier);
		self
	}
	/// Endow the given accounts with PGAS at genesis. The PGAS asset is always
	/// created; this just seeds initial balances.
	pub fn with_pgas_balances(mut self, balances: Vec<(AccountId32, u128)>) -> Self {
		self.pgas_balances = balances;
		self
	}
	/// Override the PGAS asset's `min_balance` (existential deposit).
	pub fn with_pgas_min_balance(mut self, min_balance: u128) -> Self {
		self.pgas_min_balance = min_balance;
		self
	}
	pub fn set_associated_consts(&self) {
		EXISTENTIAL_DEPOSIT.with(|v| *v.borrow_mut() = self.existential_deposit);
	}
	pub fn with_genesis_state_overrides(mut self, storage: Storage) -> Self {
		self.genesis_state_overrides = Some(storage);
		self
	}
	pub fn build(self) -> sp_io::TestExternalities {
		sp_tracing::try_init_simple();
		self.set_associated_consts();
		let mut t = self.genesis_state_overrides.unwrap_or_default();

		frame_system::GenesisConfig::<Test>::default()
			.assimilate_storage(&mut t)
			.unwrap();

		let checking_account = Pallet::<Test>::checking_account();

		pallet_balances::GenesisConfig::<Test> {
			balances: vec![(checking_account.clone(), 1_000_000_000_000)],
			..Default::default()
		}
		.assimilate_storage(&mut t)
		.unwrap();

		pallet_assets::GenesisConfig::<Test> {
			assets: vec![(PGAS_ASSET_ID, ALICE, true, self.pgas_min_balance)],
			accounts: self
				.pgas_balances
				.iter()
				.map(|(who, bal)| (PGAS_ASSET_ID, who.clone(), *bal))
				.collect(),
			..Default::default()
		}
		.assimilate_storage(&mut t)
		.unwrap();

		if let Some(multiplier) = self.next_fee_multiplier {
			pallet_transaction_payment::GenesisConfig::<Test> { multiplier, ..Default::default() }
				.assimilate_storage(&mut t)
				.unwrap();
		}

		if let Some(genesis_config) = self.genesis_config {
			genesis_config.assimilate_storage(&mut t).unwrap();
		}
		let mut ext = sp_io::TestExternalities::new(t);
		ext.register_extension(KeystoreExt::new(MemoryKeystore::new()));
		ext.execute_with(|| {
			use frame_support::traits::OnGenesis;

			Pallet::<Test>::on_genesis();
			if let Some(storage_version) = self.storage_version {
				storage_version.put::<Pallet<Test>>();
			}
			System::set_block_number(1)
		});
		ext.execute_with(|| {
			for code_hash in self.code_hashes {
				CodeInfoOf::<Test>::insert(code_hash, crate::CodeInfo::new(ALICE));
			}
		});
		ext.execute_with(|| {
			assert_ok!(Pallet::<Test>::map_account(RuntimeOrigin::signed(checking_account)));
		});
		ext
	}
}

fn initialize_block(number: u64) {
	System::reset_events();
	System::initialize(&number, &[0u8; 32].into(), &Default::default());
}

impl Default for Origin<Test> {
	fn default() -> Self {
		Self::Signed(ALICE)
	}
}

/// Dummy EVM bytecode for mocked addresses.
/// This is minimal EVM bytecode (STOP) that terminates successfully.
pub const MOCK_CODE: [u8; 1] = [0x00];

/// A mock handler implementation for testing purposes.
#[derive(DefaultNoBound)]
pub struct MockHandlerImpl<T: crate::pallet::Config> {
	// Always return this caller if set.
	mock_caller: Option<H160>,
	// Map of callee address to mocked call return value.
	mock_call: HashMap<H160, ExecReturnValue>,
	// Map of input data to mocked delegated caller info.
	mock_delegate_caller: HashMap<Vec<u8>, DelegateInfo<T>>,
}

impl<T: crate::pallet::Config> MockHandler<T> for MockHandlerImpl<T> {
	fn mock_caller(&self, _frames_len: usize) -> Option<OriginFor<T>> {
		self.mock_caller.as_ref().map(|mock_caller| {
			OriginFor::<T>::signed(T::AddressMapper::to_fallback_account_id(mock_caller))
		})
	}

	fn mock_call(
		&self,
		_callee: H160,
		_call_data: &[u8],
		_value_transferred: U256,
	) -> Option<ExecReturnValue> {
		self.mock_call.get(&_callee).cloned()
	}

	fn mock_delegated_caller(&self, _dest: H160, input_data: &[u8]) -> Option<DelegateInfo<T>> {
		self.mock_delegate_caller.get(&input_data.to_vec()).cloned()
	}

	fn mocked_code(&self, address: H160) -> Option<&[u8]> {
		if self.mock_call.contains_key(&address) {
			Some(&MOCK_CODE)
		} else {
			None
		}
	}
}

#[test]
fn ext_builder_with_genesis_config_works() {
	let pvm_contract = Account {
		address: crate::H160::repeat_byte(0x42),
		balance: U256::from(100_000_100),
		nonce: 42,
		contract_data: Some(ContractData {
			code: compile_module("dummy").unwrap().0.into(),
			storage: [([1u8; 32].into(), [2u8; 32].into())].into_iter().collect(),
		}),
	};

	let evm_contract = Account {
		address: crate::H160::repeat_byte(0x43),
		balance: U256::from(1_000_00_100),
		nonce: 43,
		contract_data: Some(ContractData {
			code: vec![
				revm::bytecode::opcode::PUSH1,
				0x00,
				revm::bytecode::opcode::PUSH1,
				0x00,
				revm::bytecode::opcode::RETURN,
			]
			.into(),
			storage: [([3u8; 32].into(), [4u8; 32].into())].into_iter().collect(),
		}),
	};

	let eoa =
		Account { address: ALICE_ADDR, balance: U256::from(100), nonce: 44, contract_data: None };

	let config = GenesisConfig::<Test> {
		mapped_accounts: vec![EVE],
		accounts: vec![eoa.clone(), pvm_contract.clone(), evm_contract.clone()],
		..Default::default()
	};

	// Genesis serialization works
	let json = serde_json::to_string(&config).unwrap();
	assert_eq!(config, serde_json::from_str::<GenesisConfig<Test>>(&json).unwrap());

	ExtBuilder::default().genesis_config(Some(config)).build().execute_with(|| {
		// account is mapped
		assert!(<Test as Config>::AddressMapper::is_mapped(&EVE));

		// EOA is created
		assert_eq!(Pallet::<Test>::evm_balance(&eoa.address), eoa.balance);

		// Contract is created
		for contract in [pvm_contract, evm_contract] {
			let contract_data = contract.contract_data.unwrap();
			let contract_info = test_utils::get_contract(&contract.address);

			assert!(System::account_exists(&<Test as Config>::AddressMapper::to_account_id(
				&contract.address
			)));

			assert_eq!(
				PristineCode::<Test>::get(&contract_info.code_hash).unwrap(),
				contract_data.code.0
			);
			assert_eq!(Pallet::<Test>::evm_nonce(&contract.address), contract.nonce);
			assert_eq!(Pallet::<Test>::evm_balance(&contract.address), contract.balance);

			for (key, value) in contract_data.storage.iter() {
				assert_eq!(
					Pallet::<Test>::get_storage(contract.address, key.0),
					Ok(Some(value.0.to_vec()))
				);
			}

			// Check that we can call contract created at genesis
			let result = builder::bare_call(contract.address).build_and_unwrap_result();
			assert!(!result.did_revert());
		}
	});
}

/// 费用适配集成夹具：费用由独立 Runtime 路由决定，框架 Weight 费用为零、信用 hold 为 ()。
/// 37 只是此 SDK 测试的任意费用，不定义任何产品的收费制度。
mod native_fees {
	use super::*;
	use crate::evm::runtime::EthExtra;
	use crate::{
		ExecConfig, TransactionLimits,
		evm::fees::{InfoT, NativeFee, NativeInfo},
	};
	use codec::{Decode, Encode};
	use frame_support::{
		dispatch::{DispatchInfo, GetDispatchInfo, PostDispatchInfo},
		traits::{
			ConstBool, ConstU64,
			fungible::{Balanced, Credit, Inspect, Mutate},
			tokens::{Fortitude, Precision, Preservation},
		},
	};
	use pallet_transaction_payment::{OnChargeTransaction, TxCreditHold};
	use sp_core::Get;
	use sp_runtime::{
		traits::{Checkable, DispatchTransaction, Dispatchable, TransactionExtension},
		transaction_validity::{InvalidTransaction, TransactionValidityError},
	};

	const FEE: u128 = 37;
	type Extra = (
		frame_system::CheckNonce<NativeTest>,
		ChargeTransactionPayment<NativeTest>,
		crate::evm::tx_extension::SetOrigin<NativeTest>,
	);
	type Uxt = crate::evm::runtime::UncheckedExtrinsic<Address, Signature, NativeExtra>;
	type NativeBlock = sp_runtime::generic::Block<Header<u64, BlakeTwo256>, Uxt>;

	#[derive(Clone, PartialEq, Eq, Debug)]
	pub struct NativeExtra;
	impl EthExtra for NativeExtra {
		type Config = NativeTest;
		type ExtensionV0 = Extra;
		type ExtensionOtherVersions = sp_runtime::traits::InvalidVersion;
		fn get_eth_extension(nonce: u32, tip: u128) -> Extra {
			(
				frame_system::CheckNonce::from(nonce),
				ChargeTransactionPayment::from(tip),
				crate::evm::tx_extension::SetOrigin::new_from_eth_transaction(),
			)
		}
	}
	frame_support::construct_runtime!(
		pub enum NativeTest {
			System: frame_system,
			Balances: pallet_balances,
			Contracts: crate,
			TransactionPayment: pallet_transaction_payment,
		}
	);
	#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
	impl frame_system::Config for NativeTest {
		type Block = NativeBlock;
		type AccountId = AccountId32;
		type Lookup = sp_runtime::traits::AccountIdLookup<Self::AccountId, u32>;
		type AccountData = pallet_balances::AccountData<u128>;
		type BlockWeights = super::BlockWeights;
	}
	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for NativeTest {
		type Balance = u128;
		type ExistentialDeposit = ConstU128<111>;
		type AccountStore = System;
		type RuntimeHoldReason = RuntimeHoldReason;
		type RuntimeFreezeReason = RuntimeFreezeReason;
		type MaxFreezes = frame_support::traits::VariantCountOf<RuntimeFreezeReason>;
	}
	#[derive_impl(pallet_transaction_payment::config_preludes::TestDefaultConfig)]
	impl pallet_transaction_payment::Config for NativeTest {
		type OnChargeTransaction = NativeCharge;
		type WeightToFee = FixedFee<0, u128>;
		type LengthToFee = FixedFee<0, u128>;
	}
	#[derive_impl(crate::config_preludes::TestDefaultConfig)]
	impl Config for NativeTest {
		type Balance = u128;
		type Currency = Balances;
		type AddressMapper = AccountId32Mapper<Self>;
		type NativeToEthRatio = ConstU64<{ native_monetary::SCALE }>;
		type StrictNativeBalance = ConstBool<true>;
		type ChainId = ConstU64<2027>;
		type Deposit = ();
		type FeeInfo = NativeInfo<Address, Signature, NativeExtra, NativePolicy>;
		type DepositPerItem = ConstU128<0>;
		type DepositPerByte = ConstU128<0>;
		type DepositPerChildTrieItem = ConstU128<0>;
		type UploadOrigin = frame_system::EnsureSigned<AccountId32>;
		type InstantiateOrigin = frame_system::EnsureSigned<AccountId32>;
	}
	impl SetWeightLimit for RuntimeCall {
		fn set_weight_limit(&mut self, new: Weight) -> Weight {
			match self {
				Self::Contracts(
					crate::Call::eth_call { weight_limit, .. }
					| crate::Call::eth_instantiate_with_code { weight_limit, .. },
				) => core::mem::replace(weight_limit, new),
				_ => Weight::zero(),
			}
		}
	}
	/// 故意使用与签名者不同的付款账户，验证 SDK 没有重新指定付款人。
	pub struct NativePolicy;
	impl NativeFee<NativeTest> for NativePolicy {
		type Charger = NativeCharge;
		fn quote(_: &AccountId32, call: &RuntimeCall) -> Result<u128, InvalidTransaction> {
			match call {
				RuntimeCall::Contracts(
					crate::Call::eth_call { .. } | crate::Call::eth_instantiate_with_code { .. },
				) => Ok(FEE),
				_ => Err(InvalidTransaction::Call),
			}
		}
		fn validate(who: &AccountId32, call: &RuntimeCall) -> Result<(), InvalidTransaction> {
			let amount = Self::quote(who, call)?;
			if Balances::reducible_balance(&BOB, Preservation::Preserve, Fortitude::Polite) < amount
			{
				return Err(InvalidTransaction::Payment);
			}
			Ok(())
		}
	}
	pub struct NativeCharge;
	impl TxCreditHold<NativeTest> for NativeCharge {
		type Credit = ();
	}
	impl OnChargeTransaction<NativeTest> for NativeCharge {
		type Balance = u128;
		type LiquidityInfo = Option<Credit<AccountId32, Balances>>;
		fn withdraw_fee(
			who: &AccountId32,
			call: &RuntimeCall,
			_: &DispatchInfo,
			_: u128,
			tip: u128,
		) -> Result<Self::LiquidityInfo, TransactionValidityError> {
			if tip != 0 {
				return Err(InvalidTransaction::Payment.into());
			}
			let amount = NativePolicy::quote(who, call)?;
			let credit = Balances::withdraw(
				&BOB,
				amount,
				Precision::Exact,
				Preservation::Preserve,
				Fortitude::Polite,
			)
			.map_err(|_| InvalidTransaction::Payment)?;
			Ok(Some(credit))
		}
		fn can_withdraw_fee(
			who: &AccountId32,
			call: &RuntimeCall,
			_: &DispatchInfo,
			_: u128,
			tip: u128,
		) -> Result<(), TransactionValidityError> {
			if tip != 0 {
				return Err(InvalidTransaction::Payment.into());
			}
			NativePolicy::validate(who, call).map_err(Into::into)
		}
		fn correct_and_deposit_fee(
			_: &AccountId32,
			_: &DispatchInfo,
			_: &PostDispatchInfo,
			_: u128,
			_: u128,
			credit: Self::LiquidityInfo,
		) -> Result<(), TransactionValidityError> {
			// 任意执行结果均保留既有已扣费用，不能以 corrected fee=0 退回。
			drop(credit);
			Ok(())
		}
		#[cfg(feature = "runtime-benchmarks")]
		fn endow_account(who: &AccountId32, amount: u128) {
			Balances::set_balance(who, amount);
		}
		#[cfg(feature = "runtime-benchmarks")]
		fn minimum_balance() -> u128 {
			111
		}
	}
	fn ext() -> sp_io::TestExternalities {
		let mut ext = sp_io::TestExternalities::new(
			frame_system::GenesisConfig::<NativeTest>::default().build_storage().unwrap(),
		);
		ext.execute_with(|| {
			System::set_block_number(1);
			Balances::set_balance(&crate::evm::Account::default().substrate_account(), 1_000_000);
			Balances::set_balance(&BOB, 10_000);
			Balances::set_balance(&Contracts::account_id(), 111);
		});
		ext
	}
	fn tx(dest: Option<H160>, input: Vec<u8>, gas: u64) -> crate::GenericTransaction {
		crate::GenericTransaction {
			from: Some(crate::evm::Account::default().address()),
			to: dest,
			input: crate::evm::Bytes(input).into(),
			chain_id: Some(<<NativeTest as Config>::ChainId as Get<u64>>::get().into()),
			gas: Some(gas.into()),
			gas_price: Some(crate::evm::fees::native_gas_price::<NativeTest>()),
			nonce: Some(0.into()),
			r#type: Some(crate::evm::TYPE_LEGACY.into()),
			..Default::default()
		}
	}
	fn checked(
		tx: crate::GenericTransaction,
	) -> sp_runtime::generic::CheckedExtrinsic<AccountId32, RuntimeCall, Extra> {
		let signed =
			crate::evm::Account::default().sign_transaction(tx.try_into_unsigned().unwrap());
		checked_payload(signed.signed_payload()).unwrap()
	}
	/// 使用生产UncheckedExtrinsic包装器核验真实RLP签名，不直接跳到转换助手。
	fn checked_payload(
		payload: Vec<u8>,
	) -> Result<
		sp_runtime::generic::CheckedExtrinsic<AccountId32, RuntimeCall, Extra>,
		TransactionValidityError,
	> {
		let unsigned: Uxt = sp_runtime::generic::UncheckedExtrinsic::new_bare(
			RuntimeCall::Contracts(crate::Call::eth_transact { payload }),
		)
		.into();
		unsigned.check(&frame_system::ChainContext::<NativeTest>::default())
	}
	fn execute(mut tx: crate::GenericTransaction) -> sp_runtime::DispatchResult {
		tx.nonce =
			Some(System::account_nonce(crate::evm::Account::default().substrate_account()).into());
		let checked = checked(tx);
		let sp_runtime::generic::ExtrinsicFormat::Signed(who, extra) = checked.format else {
			panic!("signed");
		};
		let call = checked.function;
		let info = call.get_dispatch_info();
		let len = match &call {
			RuntimeCall::Contracts(
				crate::Call::eth_call { encoded_len, .. }
				| crate::Call::eth_instantiate_with_code { encoded_len, .. },
			) => *encoded_len as usize,
			_ => panic!("Ethereum call"),
		};
		// 真实交易扩展先扣一次，随后真实 REVM 执行，再运行扩展的 post_dispatch。
		let (pre, origin) = extra
			.validate_and_prepare(RuntimeOrigin::signed(who), &call, &info, len, 0)
			.unwrap();
		let result = call.dispatch(origin);
		let mut post = match &result {
			Ok(info) => *info,
			Err(err) => err.post_info,
		};
		let outcome = result.map(|_| ()).map_err(|err| err.error);
		Extra::post_dispatch(pre, &info, &mut post, len, &outcome).unwrap();
		outcome
	}
	/// 验签和报价不写状态；gas 与框架报价都不能改变 Runtime 指定费用或付款者。
	#[test]
	fn checking_is_read_only_and_quote_is_independent_of_resource_budget() {
		ext().execute_with(|| {
			<NativeTest as Config>::FeeInfo::integrity_test();
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			for gas in [500_000_000, 1_000_000_000] {
				let generic = tx(Some(H160::repeat_byte(9)), vec![], gas);
				let info = generic
					.clone()
					.into_call::<NativeTest>(crate::evm::CreateCallMode::ExtrinsicExecution(
						1024,
						vec![],
					))
					.unwrap();
				assert_eq!(info.tx_fee, FEE);
				assert_eq!(info.storage_deposit, 0);
				assert!(ExecConfig::<NativeTest>::new_eth_tx(U256::one(), 1024, Weight::zero())
					.collect_deposit_from_hold
					.is_none());
				let _ = checked(generic);
				assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
			}
		});
	}
	/// 正常调用真实扣一次，确切付款者为 BOB；收据与 post_dispatch 不再补扣或退款。
	#[test]
	fn actual_execution_charges_existing_payer_once() {
		ext().execute_with(|| {
			let signer = crate::evm::Account::default().substrate_account();
			let before = Balances::balance(&signer);
			execute(tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000)).unwrap();
			assert_eq!(Balances::balance(&BOB), 10_000 - FEE);
			assert_eq!(Balances::balance(&signer), before);
			crate::block_storage::on_finalize_build_eth_block::<NativeTest>(1);
			let receipt = crate::ReceiptInfoData::<NativeTest>::get().pop().unwrap();
			assert_eq!(receipt.effective_gas_price, U256::from(1_000_000_000u64));
			assert_eq!(receipt.gas_used, U256::from(FEE * 10_000_000));
			assert_eq!(
				receipt.gas_used * receipt.effective_gas_price,
				U256::from(FEE) * U256::from(native_monetary::SCALE)
			);
		});
	}

	/// 钱包默认估算覆盖业务费；修改 gas 缓冲与 EIP-1559 上限不改变实际费。
	#[test]
	fn native_fee_estimate_and_eip1559_receipt_use_the_same_fixed_price() {
		ext().execute_with(|| {
			let price = crate::evm::fees::native_gas_price::<NativeTest>();
			assert_eq!(Contracts::evm_base_fee(), price);
			let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			generic.gas = None;
			generic.gas_price = None;
			generic.r#type = Some(crate::evm::TYPE_EIP1559.into());
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let estimate =
				Contracts::eth_estimate_gas(generic.clone(), Default::default()).unwrap();
			assert!(estimate >= U256::from(FEE * 10_000_000));
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
			generic.gas = Some(estimate * 2);
			generic.max_fee_per_gas = Some(price * 2);
			generic.max_priority_fee_per_gas = Some(U256::zero());
			execute(generic.clone()).unwrap();
			execute(generic).unwrap();
			crate::block_storage::on_finalize_build_eth_block::<NativeTest>(1);
			let receipts = crate::ReceiptInfoData::<NativeTest>::get();
			assert_eq!(receipts.len(), 2);
			for receipt in receipts {
				assert_eq!(receipt.effective_gas_price, price);
				assert_eq!(
					receipt.gas_used * price,
					U256::from(FEE) * U256::from(native_monetary::SCALE)
				);
			}
			let block = crate::EthereumBlock::<NativeTest>::get();
			assert_eq!(block.base_fee_per_gas, price);
			assert_eq!(block.gas_used, U256::from(FEE * 20_000_000));
			assert_eq!(Balances::balance(&BOB), 10_000 - 2 * FEE);
		});
	}

	/// 费用限额、价格、优先费及转换边界在验签阶段拒绝，不能先扣费再失败。
	#[test]
	fn native_fee_gas_price_and_signed_limit_boundaries_are_enforced() {
		ext().execute_with(|| {
			let price = crate::evm::fees::native_gas_price::<NativeTest>();
			assert_eq!(crate::evm::fees::native_fee_to_gas::<NativeTest>(0).unwrap(), 0);
			let largest = u64::MAX as u128 / 10_000_000;
			assert!(crate::evm::fees::native_fee_to_gas::<NativeTest>(largest).is_ok());
			assert_eq!(
				crate::evm::fees::native_fee_to_gas::<NativeTest>(largest + 1),
				Err(InvalidTransaction::ExhaustsResources)
			);
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let mut invalid = vec![];
			for value in [U256::zero(), price - 1, price + 1] {
				let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
				generic.gas_price = Some(value);
				invalid.push(generic);
			}
			for (cap, tip, gas) in [
				(price - 1, U256::zero(), 1_000_000_000u64),
				(price * 2, U256::one(), 1_000_000_000),
				(price * 2, U256::zero(), (FEE * 10_000_000 - 1) as u64),
			] {
				let mut generic = tx(Some(H160::repeat_byte(9)), vec![], gas);
				generic.r#type = Some(crate::evm::TYPE_EIP1559.into());
				generic.max_fee_per_gas = Some(cap);
				generic.max_priority_fee_per_gas = Some(tip);
				invalid.push(generic);
			}
			for generic in invalid {
				let signed = crate::evm::Account::default()
					.sign_transaction(generic.try_into_unsigned().unwrap());
				assert!(checked_payload(signed.signed_payload()).is_err());
				assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
			}
		});
	}
	/// 模拟使用相同收费器，余额、发行量、nonce、事件和全部存储最后都回滚。
	#[test]
	fn simulation_has_no_persistent_state_or_issuance_change() {
		ext().execute_with(|| {
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let result = Contracts::dry_run_eth_transact(
				tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000),
				crate::evm::DryRunConfig {
					perform_balance_checks: Some(true),
					..Default::default()
				},
			);
			assert!(result.is_ok(), "{result:?}");
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
		});
	}
	/// 资源换算同时限制时间和证明大小，极小预算不能进入执行。
	#[test]
	fn resource_conversion_limits_both_weight_dimensions_without_changing_fee() {
		ext().execute_with(|| {
			use crate::evm::fees::{resource_gas_to_weight, resource_weight_to_gas};
			assert_eq!(resource_weight_to_gas::<NativeTest>(Weight::zero()), 0);
			for gas in [1, 100, 1_000_000, u64::MAX] {
				let weight = resource_gas_to_weight::<NativeTest>(gas);
				assert!(resource_weight_to_gas::<NativeTest>(weight) <= gas);
			}
			assert!(resource_weight_to_gas::<NativeTest>(Weight::from_parts(0, 1)) > 0);
			let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			generic.gas = Some(1.into());
			assert!(generic
				.into_call::<NativeTest>(crate::evm::CreateCallMode::ExtrinsicExecution(
					1024,
					vec![]
				))
				.is_err());
		});
	}

	/// 钱包上限不足、金额精度非法、资源超界、付款余额不足及未分类业务均拒绝。
	#[test]
	fn invalid_payment_amount_resource_and_native_business_are_rejected() {
		ext().execute_with(|| {
			let mode = crate::evm::CreateCallMode::ExtrinsicExecution(1024, vec![]);
			let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			generic.gas_price = Some(U256::one());
			assert!(generic.into_call::<NativeTest>(mode.clone()).is_err());
			let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			generic.value = Some(U256::one());
			assert!(generic.into_call::<NativeTest>(mode.clone()).is_err());
			let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			generic.gas = Some(U256::MAX);
			assert!(generic.into_call::<NativeTest>(mode.clone()).is_err());
			let generic = tx(
				Some(crate::RUNTIME_PALLETS_ADDR),
				RuntimeCall::System(frame_system::Call::remark { remark: vec![] }).encode(),
				1_000_000_000,
			);
			assert!(generic.into_call::<NativeTest>(mode).is_err());
			Balances::set_balance(&BOB, 111);
			let generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			let signed = crate::evm::Account::default()
				.sign_transaction(generic.try_into_unsigned().unwrap());
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			assert!(
				NativeExtra::try_into_checked_extrinsic(&signed.signed_payload(), 1024).is_err()
			);
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
		});
	}
	/// Ethereum地址直接进入原32字节账本；查询不创建映射、账户或余额。
	#[test]
	fn ethereum_account_mapping_is_stateless_and_does_not_take_native_control() {
		ext().execute_with(|| {
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let address = H160::repeat_byte(0x2a);
			let account = AccountId32Mapper::<NativeTest>::to_fallback_account_id(&address);
			let bytes: &[u8; 32] = account.as_ref();
			assert_eq!(&bytes[..20], address.as_bytes());
			assert_eq!(&bytes[20..], &[0xee; 12]);
			assert_eq!(AccountId32Mapper::<NativeTest>::to_address(&account), address);
			assert_eq!(AccountId32Mapper::<NativeTest>::to_account_id(&address), account);
			assert!(AccountId32Mapper::<NativeTest>::is_mapped(&account));
			assert_eq!(Balances::balance(&account), 0);
			assert!(crate::OriginalAccount::<NativeTest>::get(address).is_none());
			let native_account = AccountId32::new([0x42; 32]);
			let native_address = AccountId32Mapper::<NativeTest>::to_address(&native_account);
			assert_ne!(
				AccountId32Mapper::<NativeTest>::to_fallback_account_id(&native_address),
				native_account
			);
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
		});
	}

	/// 三类真实签名均经生产包装器和交易扩展执行，不能用自报from接管账户。
	#[test]
	fn signed_transaction_types_use_recovered_sender_shared_nonce_and_one_fee() {
		ext().execute_with(|| {
			let signer = crate::evm::Account::default().substrate_account();
			let dest = H160::repeat_byte(9);
			let recipient = AccountId32Mapper::<NativeTest>::to_fallback_account_id(&dest);
			Balances::set_balance(&recipient, 111);
			for (nonce, kind) in
				[crate::evm::TYPE_LEGACY, crate::evm::TYPE_EIP2930, crate::evm::TYPE_EIP1559]
					.into_iter()
					.enumerate()
			{
				let mut generic = tx(Some(dest), vec![], 1_000_000_000);
				generic.r#type = Some(kind.into());
				generic.max_fee_per_gas =
					Some(crate::evm::fees::native_gas_price::<NativeTest>() * 2);
				generic.max_priority_fee_per_gas = Some(U256::zero());
				generic.value = Some(native_monetary::SCALE.into());
				generic.from = Some(AccountId32Mapper::<NativeTest>::to_address(&BOB));
				generic.nonce = Some((nonce as u32).into());
				let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
				let converted = checked(generic.clone());
				let sp_runtime::generic::ExtrinsicFormat::Signed(who, _) = converted.format else {
					panic!("signed")
				};
				assert_eq!(who, signer);
				assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
				execute(generic).unwrap();
				assert_eq!(System::account_nonce(&signer), nonce as u32 + 1);
				assert_eq!(Balances::balance(&BOB), 10_000 - FEE * (nonce as u128 + 1));
				assert_eq!(Balances::balance(&recipient), 111 + nonce as u128 + 1);
				assert_eq!(Balances::balance(&signer), 1_000_000 - nonce as u128 - 1);
			}
		});
	}

	/// 错链、无链、nonce超界及未授权原生业务在核验阶段拒绝，全部状态保持不变。
	#[test]
	fn invalid_chain_nonce_and_native_dispatch_leave_state_unchanged() {
		ext().execute_with(|| {
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let base = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			let mut invalid = vec![];
			for chain_id in [None, Some(U256::from(1)), Some(U256::from(2028))] {
				let mut generic = base.clone();
				generic.chain_id = chain_id;
				invalid.push(generic);
			}
			for nonce in [U256::from(u32::MAX), U256::from(u32::MAX) + U256::one()] {
				let mut generic = base.clone();
				generic.nonce = Some(nonce);
				invalid.push(generic);
			}
			invalid.push(tx(
				Some(crate::RUNTIME_PALLETS_ADDR),
				RuntimeCall::System(frame_system::Call::remark { remark: vec![] }).encode(),
				1_000_000_000,
			));
			for generic in invalid {
				let signed = crate::evm::Account::default()
					.sign_transaction(generic.try_into_unsigned().unwrap());
				assert!(checked_payload(signed.signed_payload()).is_err());
				assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
			}
		});
	}

	/// 高s变形仍能恢复同一地址，但交易入口必须拒绝；ECDSA恢复语义不变。
	#[test]
	fn invalid_signatures_encoding_and_unsupported_types_are_rejected() {
		ext().execute_with(|| {
			let mut generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			generic.r#type = Some(crate::evm::TYPE_EIP1559.into());
			generic.max_fee_per_gas = Some(crate::evm::fees::native_gas_price::<NativeTest>() * 2);
			let unsigned = generic.clone().try_into_unsigned().unwrap();
			let signed = crate::evm::Account::default().sign_transaction(unsigned.clone());
			assert!(checked_payload(signed.signed_payload()).is_ok());
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let signature = signed.raw_signature().unwrap();
			let mut bad_r = signature;
			bad_r[..32].fill(0);
			let mut bad_s = signature;
			bad_s[32..64].fill(0);
			let mut bad_parity = signature;
			bad_parity[64] = 2;
			let mut high_s = signature;
			let order =
				U256([0xbfd25e8cd0364141, 0xbaaedce6af48a03b, 0xfffffffffffffffe, u64::MAX]);
			(order - U256::from_big_endian(&signature[32..64]))
				.write_as_big_endian(&mut high_s[32..64]);
			high_s[64] ^= 1;
			let malleated = unsigned.clone().with_signature(high_s);
			assert_eq!(malleated.recover_eth_address(), signed.recover_eth_address());
			for signature in [bad_r, bad_s, bad_parity, high_s] {
				assert!(checked_payload(
					unsigned.clone().with_signature(signature).signed_payload()
				)
				.is_err());
			}
			let mut trailing = signed.signed_payload();
			trailing.push(0);
			assert!(checked_payload(trailing).is_err());
			let payload = signed.signed_payload();
			let encoded = rlp::Rlp::new(&payload[1..]);
			let mut noncanonical = rlp::RlpStream::new_list(encoded.item_count().unwrap());
			for index in 0..encoded.item_count().unwrap() {
				if index == 1 {
					noncanonical.append_raw(&[0x82, 0, 0], 1);
				} else {
					noncanonical.append_raw(encoded.at(index).unwrap().as_raw(), 1);
				}
			}
			let mut payload = vec![crate::evm::TYPE_EIP1559];
			payload.extend_from_slice(&noncanonical.out());
			assert!(checked_payload(payload).is_err());
			for kind in [crate::evm::TYPE_EIP4844, crate::evm::TYPE_EIP7702] {
				generic.r#type = Some(kind.into());
				let signed = crate::evm::Account::default()
					.sign_transaction(generic.clone().try_into_unsigned().unwrap());
				assert!(checked_payload(signed.signed_payload()).is_err());
			}
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
		});
	}

	/// 已执行nonce不能再次付款或执行；未来nonce也不能提前prepare。
	#[test]
	fn replay_and_future_nonce_cannot_enter_prepare_or_charge_again() {
		ext().execute_with(|| {
			let generic = tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000);
			let future = checked({
				let mut tx = generic.clone();
				tx.nonce = Some(1.into());
				tx
			});
			let sp_runtime::generic::ExtrinsicFormat::Signed(who, extra) = future.format else {
				panic!("signed")
			};
			let call = future.function;
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			assert!(extra
				.validate_and_prepare(
					RuntimeOrigin::signed(who),
					&call,
					&call.get_dispatch_info(),
					1024,
					0
				)
				.is_err());
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
			execute(generic.clone()).unwrap();
			let stale = checked(generic);
			let sp_runtime::generic::ExtrinsicFormat::Signed(who, extra) = stale.format else {
				panic!("signed")
			};
			let call = stale.function;
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			assert!(extra
				.validate_and_prepare(
					RuntimeOrigin::signed(who),
					&call,
					&call.get_dispatch_info(),
					1024,
					0
				)
				.is_err());
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
			assert_eq!(Balances::balance(&BOB), 10_000 - FEE);
		});
	}

	/// 官方来源扩展零字节编码；现有原生签名载荷和完整交易字节必须保持一致。
	#[test]
	fn native_signed_payload_and_extrinsic_encoding_are_preserved() {
		use sp_core::Pair;
		ext().execute_with(|| {
			type OriginalExtra =
				(frame_system::CheckNonce<NativeTest>, ChargeTransactionPayment<NativeTest>);
			let pair = sp_core::sr25519::Pair::from_seed(&[19; 32]);
			let account = AccountId32::new(pair.public().0);
			let call = RuntimeCall::System(frame_system::Call::remark { remark: vec![7; 3] });
			let original_extra: OriginalExtra =
				(frame_system::CheckNonce::from(0), ChargeTransactionPayment::from(0));
			let extra: Extra =
				(original_extra.0.clone(), original_extra.1.clone(), Default::default());
			let original_payload = sp_runtime::generic::SignedPayload::from_raw(
				call.clone(),
				original_extra.clone(),
				((), ()),
			);
			let payload = sp_runtime::generic::SignedPayload::from_raw(
				call.clone(),
				extra.clone(),
				((), (), ()),
			);
			assert_eq!(original_payload.encode(), payload.encode());
			let original_signing_bytes = original_payload.using_encoded(|bytes| bytes.to_vec());
			let signing_bytes = payload.using_encoded(|bytes| bytes.to_vec());
			assert_eq!(original_signing_bytes, signing_bytes);
			let signature = MultiSignature::Sr25519(pair.sign(&signing_bytes));
			let original = sp_runtime::generic::UncheckedExtrinsic::<
				Address,
				RuntimeCall,
				Signature,
				OriginalExtra,
			>::new_signed(
				call.clone(),
				MultiAddress::Id(account.clone()),
				signature.clone(),
				original_extra,
			);
			let wrapped: Uxt = sp_runtime::generic::UncheckedExtrinsic::new_signed(
				call,
				MultiAddress::Id(account.clone()),
				signature,
				extra,
			)
			.into();
			assert_eq!(original.encode(), wrapped.encode());
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let decoded = Uxt::decode(&mut &wrapped.encode()[..]).unwrap();
			let checked =
				decoded.check(&frame_system::ChainContext::<NativeTest>::default()).unwrap();
			let sp_runtime::generic::ExtrinsicFormat::Signed(who, _) = checked.format else {
				panic!("signed")
			};
			assert_eq!(who, account);
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
		});
	}

	/// 来源标记由Runtime代码设置，编码后重解码不能伪造EthTransaction权限。
	#[test]
	fn ethereum_origin_marker_cannot_be_forged_by_encoding() {
		ext().execute_with(|| {
			type Marker = crate::evm::tx_extension::SetOrigin<NativeTest>;
			let encoded = Marker::new_from_eth_transaction().encode();
			assert!(encoded.is_empty());
			let decoded = Marker::decode(&mut &encoded[..]).unwrap();
			let call = checked(tx(Some(H160::repeat_byte(9)), vec![], 1_000_000_000)).function;
			let before = sp_io::storage::root(sp_runtime::StateVersion::V1);
			let (_, _, origin) = decoded
				.validate(
					RuntimeOrigin::signed(BOB),
					&call,
					&call.get_dispatch_info(),
					1024,
					(),
					&sp_runtime::traits::TxBaseImplication(&call),
					frame_support::pallet_prelude::TransactionSource::External,
				)
				.unwrap();
			assert_eq!(frame_system::ensure_signed(origin.clone()).unwrap(), BOB);
			assert!(call.dispatch(origin).is_err());
			assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), before);
		});
	}

	/// 真实合约 REVERT 与耗尽资源均回滚业务状态，交易费用仍只扣一次。
	#[test]
	fn revert_and_resource_exhaustion_keep_one_fee() {
		for (runtime, expected_error) in [
			(vec![0x60, 0, 0x60, 0, 0xfd], crate::Error::<NativeTest>::ContractReverted),
			(vec![0x5b, 0x60, 0, 0x56], crate::Error::<NativeTest>::OutOfGas),
		] {
			let expected_error: sp_runtime::DispatchError = expected_error.into();
			ext().execute_with(|| {
				let signer = crate::evm::Account::default().substrate_account();
				let init = native_monetary::initcode(&runtime);
				let deployed = Contracts::bare_instantiate(
					RuntimeOrigin::signed(signer.clone()),
					U256::zero(),
					TransactionLimits::WeightAndDeposit {
						weight_limit: Weight::from_parts(1_000_000_000_000, 10_000_000),
						deposit_limit: 100_000,
					},
					crate::Code::Upload(init),
					vec![],
					Some([3; 32]),
					&ExecConfig::new_substrate_tx(),
				);
				let dest = deployed.result.unwrap().addr;
				let before = Balances::balance(&signer);
				let mut generic = tx(Some(dest), vec![], 1_000_000_000);
				generic.value = Some(native_monetary::SCALE.into());
				// Ethereum外层保留成功以写收据，合约失败以确切事件记录；value必须回滚。
				execute(generic).unwrap();
				assert_eq!(Balances::balance(&BOB), 10_000 - FEE);
				assert_eq!(Balances::balance(&signer), before);
				crate::block_storage::on_finalize_build_eth_block::<NativeTest>(1);
				let receipt = crate::ReceiptInfoData::<NativeTest>::get().pop().unwrap();
				assert_eq!(
					receipt.gas_used * receipt.effective_gas_price,
					U256::from(FEE) * U256::from(native_monetary::SCALE)
				);
				assert!(System::events().iter().any(|event| matches!(
					event.event,
					RuntimeEvent::Contracts(crate::Event::EthExtrinsicRevert { dispatch_error })
						if dispatch_error == expected_error
				)));
			});
		}
	}
	/// 计时充足时，日志及不可变数据也必须受空间预算约束，拒绝不能留下状态或扣款。
	#[test]
	fn state_growth_exhausts_space_before_time() {
		use crate::{
			metering::{Token, TransactionMeter},
			vm::RuntimeCosts,
		};
		ext().execute_with(|| {
			for token in [
				RuntimeCosts::DepositEvent { num_topic: 0, len: 0 },
				RuntimeCosts::DepositEvent { num_topic: 4, len: crate::limits::EVENT_BYTES },
				RuntimeCosts::SetImmutableData(0),
				RuntimeCosts::SetImmutableData(crate::limits::IMMUTABLE_BYTES),
			] {
				let cost = <RuntimeCosts as Token<NativeTest>>::weight(&token);
				assert!(cost.proof_size() > 0);
				let root = sp_io::storage::root(sp_runtime::StateVersion::V1);
				let mut meter =
					TransactionMeter::<NativeTest>::new(TransactionLimits::WeightAndDeposit {
						weight_limit: Weight::from_parts(u64::MAX, cost.proof_size()),
						deposit_limit: 0,
					})
					.unwrap();
				assert_ok!(meter.charge_weight_token(token));
				assert_eq!(
					meter.charge_weight_token(token),
					Err(crate::Error::<NativeTest>::OutOfGas.into())
				);
				let mut too_small =
					TransactionMeter::<NativeTest>::new(TransactionLimits::WeightAndDeposit {
						weight_limit: Weight::from_parts(u64::MAX, cost.proof_size() - 1),
						deposit_limit: 0,
					})
					.unwrap();
				assert_eq!(
					too_small.charge_weight_token(token),
					Err(crate::Error::<NativeTest>::OutOfGas.into())
				);
				assert_eq!(sp_io::storage::root(sp_runtime::StateVersion::V1), root);
			}
		});
	}

	/// 空间限额随真实数据增长；上游非严格模式的资源值与时间测量值均保持原样。
	#[test]
	fn native_space_meter_preserves_non_native_and_measured_time() {
		use crate::{metering::Token, vm::RuntimeCosts};
		for (small, large) in [
			(
				RuntimeCosts::DepositEvent { num_topic: 0, len: 0 },
				RuntimeCosts::DepositEvent { num_topic: 0, len: crate::limits::EVENT_BYTES },
			),
			(
				RuntimeCosts::SetImmutableData(0),
				RuntimeCosts::SetImmutableData(crate::limits::IMMUTABLE_BYTES),
			),
		] {
			let native_small = <RuntimeCosts as Token<NativeTest>>::weight(&small);
			let native_large = <RuntimeCosts as Token<NativeTest>>::weight(&large);
			let standard_small = <RuntimeCosts as Token<super::Test>>::weight(&small);
			let standard_large = <RuntimeCosts as Token<super::Test>>::weight(&large);
			assert_eq!(native_small.ref_time(), standard_small.ref_time());
			assert_eq!(native_large.ref_time(), standard_large.ref_time());
			assert_eq!(standard_small.proof_size(), 0);
			assert_eq!(standard_large.proof_size(), 0);
			assert!(native_large.proof_size() > native_small.proof_size());
		}
	}
}

/// 与上游 dust/PGAS 夹具隔离的 u128 整单位 Runtime；只测试金额，Ethereum 收费入口关闭。
mod native_monetary {
	use crate::{
		AccountId32Mapper, AddressMapper, BalanceWithDust, Config, Error, ExecConfig,
		TransactionLimits,
	};
	use frame_support::{
		assert_noop, assert_ok, derive_impl,
		traits::{
			fungible::{Inspect, Mutate},
			tokens::Preservation,
			ConstBool, ConstU128, ConstU64,
		},
		weights::Weight,
	};
	use sp_core::{H160, U256};
	use sp_runtime::{AccountId32, BuildStorage};

	pub(super) const SCALE: u64 = 10_000_000_000_000_000;
	frame_support::parameter_types! { pub static NativeDepositPerItem: u128 = 0; }
	frame_support::construct_runtime!(
		pub enum NativeTest {
			System: frame_system,
			Balances: pallet_balances,
			Contracts: crate,
		}
	);
	#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
	impl frame_system::Config for NativeTest {
		type Block = frame_system::mocking::MockBlock<Self>;
		type AccountId = AccountId32;
		type Lookup = sp_runtime::traits::IdentityLookup<Self::AccountId>;
		type AccountData = pallet_balances::AccountData<u128>;
	}
	#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
	impl pallet_balances::Config for NativeTest {
		type Balance = u128;
		type ExistentialDeposit = ConstU128<111>;
		type AccountStore = System;
		type RuntimeHoldReason = RuntimeHoldReason;
		type RuntimeFreezeReason = RuntimeFreezeReason;
		type MaxFreezes = frame_support::traits::VariantCountOf<RuntimeFreezeReason>;
	}
	#[derive_impl(crate::config_preludes::TestDefaultConfig)]
	impl Config for NativeTest {
		type Balance = u128;
		type Currency = Balances;
		type AddressMapper = AccountId32Mapper<Self>;
		type NativeToEthRatio = ConstU64<SCALE>;
		type StrictNativeBalance = ConstBool<true>;
		type Deposit = ();
		type FeeInfo = ();
		type DepositPerItem = NativeDepositPerItem;
		type DepositPerByte = ConstU128<0>;
		type DepositPerChildTrieItem = ConstU128<0>;
		type UploadOrigin = frame_system::EnsureSigned<AccountId32>;
		type InstantiateOrigin = frame_system::EnsureSigned<AccountId32>;
		type Precompiles = (super::precompiles::WithInfo<Self>,);
	}
	impl crate::evm::runtime::SetWeightLimit for RuntimeCall {
		fn set_weight_limit(&mut self, _: Weight) -> Weight {
			Weight::zero()
		}
	}
	pub(super) fn address(who: &AccountId32) -> H160 {
		<NativeTest as Config>::AddressMapper::to_address(who)
	}
	pub(super) fn externalities() -> sp_io::TestExternalities {
		let storage = frame_system::GenesisConfig::<NativeTest>::default().build_storage().unwrap();
		let mut ext = sp_io::TestExternalities::new(storage);
		ext.execute_with(|| {
			NativeDepositPerItem::set(0);
			System::set_block_number(1);
			Balances::set_balance(&super::ALICE, 1_000_000);
			Balances::set_balance(&super::BOB, 1_000);
			Balances::set_balance(&Contracts::account_id(), 111);
			assert!(<NativeTest as Config>::AddressMapper::is_mapped(&super::ALICE));
			assert!(<NativeTest as Config>::AddressMapper::is_mapped(&super::BOB));
		});
		ext
	}
	pub(super) fn root() -> Vec<u8> {
		sp_io::storage::root(sp_runtime::StateVersion::V1)
	}
	pub(super) fn call(
		dest: H160,
		value: U256,
	) -> crate::ContractResult<crate::ExecReturnValue, u128> {
		Contracts::bare_call(
			RuntimeOrigin::signed(super::ALICE),
			dest,
			value,
			TransactionLimits::WeightAndDeposit {
				weight_limit: Weight::from_parts(1_000_000_000_000, 10_000_000),
				deposit_limit: 100_000,
			},
			Vec::new(),
			&ExecConfig::new_substrate_tx(),
		)
	}
	pub(super) fn instantiate(
		code: Vec<u8>,
		value: U256,
	) -> crate::ContractResult<crate::InstantiateReturnValue, u128> {
		Contracts::bare_instantiate(
			RuntimeOrigin::signed(super::ALICE),
			value,
			TransactionLimits::WeightAndDeposit {
				weight_limit: Weight::from_parts(1_000_000_000_000, 10_000_000),
				deposit_limit: 100_000,
			},
			crate::Code::Upload(code),
			Vec::new(),
			Some([7; 32]),
			&ExecConfig::new_substrate_tx(),
		)
	}
	pub(super) fn initcode(runtime: &[u8]) -> Vec<u8> {
		assert!(runtime.len() < 256);
		let n = runtime.len() as u8;
		let mut code = vec![0x60, n, 0x60, 12, 0x60, 0, 0x39, 0x60, n, 0x60, 0, 0xf3];
		code.extend_from_slice(runtime);
		code
	}

	/// 覆盖零、分、元、ED 及 u128 上界，非法金额不得截断或舍入。
	#[test]
	fn strict_conversion_is_exact_at_all_native_boundaries() {
		externalities().execute_with(|| {
			for cents in [0, 1, 100, 111, u128::MAX] {
				let evm = Contracts::convert_native_to_evm(cents);
				assert_eq!(evm, U256::from(cents) * U256::from(SCALE));
				let native = BalanceWithDust::<u128>::from_value::<NativeTest>(evm).unwrap();
				assert_eq!(native.deconstruct(), (cents, 0));
			}
			for amount in [U256::from(1), U256::from(SCALE - 1), U256::from(SCALE + 1), U256::MAX] {
				assert!(BalanceWithDust::<u128>::from_value::<NativeTest>(amount).is_err());
			}
			let overflow = (U256::from(u128::MAX) + U256::from(1)) * U256::from(SCALE);
			assert_eq!(
				BalanceWithDust::<u128>::from_value::<NativeTest>(overflow),
				Err(crate::BalanceConversionError::Value)
			);
			assert_eq!(
				Contracts::new_balance_with_dust(U256::from(u128::MAX) * U256::from(SCALE)),
				Err(crate::BalanceConversionError::Value)
			);
			assert_eq!(
				Contracts::new_balance_with_dust(U256::from(u128::MAX - 111) * U256::from(SCALE)),
				Ok((u128::MAX, 0))
			);
		});
	}

	/// 已有 dust、手工构造、整数支付、自转账、零值及 setter 均不得绕过底层检查。
	#[test]
	fn strict_sinks_reject_dust_before_any_state_change() {
		externalities().execute_with(|| {
			let fractional = BalanceWithDust::new_unchecked::<super::Test>(1u128, 1);
			assert_noop!(
				crate::evm::transfer_with_dust::<NativeTest>(
					&super::ALICE,
					&super::BOB,
					fractional,
					Preservation::Preserve
				),
				Error::<NativeTest>::BalanceConversionFailed
			);
			assert_noop!(
				crate::evm::burn_with_dust::<NativeTest>(&super::ALICE, fractional),
				Error::<NativeTest>::BalanceConversionFailed
			);
			for who in [&super::ALICE, &super::BOB] {
				crate::AccountInfoOf::<NativeTest>::insert(
					address(who),
					crate::AccountInfo { dust: 1, ..Default::default() },
				);
				for value in [0u128, 1] {
					assert_noop!(
						crate::evm::transfer_with_dust::<NativeTest>(
							&super::ALICE,
							&super::BOB,
							value.into(),
							Preservation::Preserve
						),
						Error::<NativeTest>::BalanceConversionFailed
					);
				}
				assert_noop!(
					crate::evm::transfer_with_dust::<NativeTest>(
						who,
						who,
						0u128.into(),
						Preservation::Preserve
					),
					Error::<NativeTest>::BalanceConversionFailed
				);
				assert_noop!(
					crate::evm::burn_with_dust::<NativeTest>(who, 0u128.into()),
					Error::<NativeTest>::BalanceConversionFailed
				);
				assert_noop!(
					Contracts::set_evm_balance(&address(who), U256::zero())
						.map_err(sp_runtime::DispatchError::from),
					Error::<NativeTest>::BalanceConversionFailed
				);
				crate::AccountInfoOf::<NativeTest>::remove(address(who));
			}
		});
	}

	/// 不足一分在 bare 执行前拒绝，且源余额、发行量及完整状态均保持原值。
	#[test]
	fn bare_calls_validate_even_when_value_would_not_move() {
		externalities().execute_with(|| {
			for dest in [address(&super::ALICE), address(&super::BOB)] {
				let before = root();
				assert_eq!(
					call(dest, U256::from(1)).result.unwrap_err(),
					Error::<NativeTest>::BalanceConversionFailed.into()
				);
				assert_eq!(root(), before);
			}
			let before = root();
			assert_eq!(
				instantiate(initcode(&[0]), U256::from(SCALE + 1)).result.unwrap_err(),
				Error::<NativeTest>::BalanceConversionFailed.into()
			);
			assert_eq!(root(), before);
			let issuance = Balances::total_issuance();
			assert!(call(address(&super::BOB), U256::from(SCALE)).result.is_ok());
			assert_eq!(Balances::balance(&super::BOB), 1_001);
			assert_eq!(Balances::total_issuance(), issuance);
		});
	}

	/// RPC 覆盖是必定恢复的模拟；部分覆盖失败不能留下前一个账户的改动。
	#[test]
	fn overrides_are_exact_and_always_ephemeral() {
		use crate::evm::{StateOverride, StateOverrideSet};
		externalities().execute_with(|| {
			let before = root();
			let override_set = StateOverrideSet(
				[(
					address(&super::ALICE),
					StateOverride { balance: Some(U256::from(100 * SCALE)), ..Default::default() },
				)]
				.into_iter()
				.collect(),
			);
			assert_ok!(crate::state_overrides::with_state_overrides::<NativeTest, _>(
				override_set,
				|| {
					assert_eq!(Balances::balance(&super::ALICE), 211);
					Ok(())
				}
			));
			assert_eq!(root(), before);
			let mut addresses = [address(&super::ALICE), address(&super::BOB)];
			addresses.sort();
			let override_set = StateOverrideSet(
				[
					(
						addresses[0],
						StateOverride { balance: Some(U256::from(SCALE)), ..Default::default() },
					),
					(
						addresses[1],
						StateOverride { balance: Some(U256::from(1)), ..Default::default() },
					),
				]
				.into_iter()
				.collect(),
			);
			assert!(crate::state_overrides::with_state_overrides::<NativeTest, _>(
				override_set,
				|| Ok(())
			)
			.is_err());
			assert_eq!(root(), before);
		});
	}

	/// 制度收费未接入前，直接派发及交易解码不能绕过门禁，也不能补扣舍入差额。
	#[test]
	fn ethereum_execution_fails_closed_without_native_fee_adapter() {
		externalities().execute_with(|| {
			let before = root();
			assert!(crate::evm::GenericTransaction::default()
				.into_call::<NativeTest>(crate::evm::CreateCallMode::DryRun)
				.is_err());
			let result = Contracts::eth_call(
				crate::Origin::<NativeTest>::EthTransaction(super::ALICE).into(),
				address(&super::BOB),
				U256::zero(),
				Weight::zero(),
				U256::from(100),
				Vec::new(),
				Vec::new(),
				U256::from(3),
				0,
			);
			assert_eq!(
				result.unwrap_err().error,
				Error::<NativeTest>::NativeFeeNotConfigured.into()
			);
			let inner = RuntimeCall::Balances(pallet_balances::Call::transfer_allow_death {
				dest: super::BOB,
				value: 1,
			});
			let result = Contracts::eth_substrate_call(
				crate::Origin::<NativeTest>::EthTransaction(super::ALICE).into(),
				Box::new(inner),
				Vec::new(),
			);
			assert_eq!(
				result.unwrap_err().error,
				Error::<NativeTest>::NativeFeeNotConfigured.into()
			);
			let result = crate::evm::block_storage::EthereumCallResult::new::<NativeTest>(
				super::ALICE,
				Default::default(),
				Weight::zero(),
				0,
				&Default::default(),
				U256::from(3),
				None,
			);
			assert_eq!(
				result.result.unwrap_err().error,
				Error::<NativeTest>::NativeFeeNotConfigured.into()
			);
			assert_eq!(root(), before);
		});
	}

	/// Genesis 只能核对已由原生账本出资的余额，不能追加分配或忽略非法金额。
	#[test]
	fn genesis_neither_mints_ed_nor_ignores_invalid_amounts() {
		use frame_support::traits::BuildGenesisConfig;
		externalities().execute_with(|| {
			let issuance = Balances::total_issuance();
			let account = crate::genesis::Account {
				address: address(&super::ALICE),
				balance: U256::from(1_000_000u128 - 111) * U256::from(SCALE),
				nonce: 0,
				contract_data: None,
			};
			crate::GenesisConfig::<NativeTest> { accounts: vec![account], ..Default::default() }
				.build();
			assert_eq!(Balances::total_issuance(), issuance);
			let before = root();
			let invalid = crate::GenesisConfig::<NativeTest> {
				accounts: vec![crate::genesis::Account {
					address: address(&super::ALICE),
					balance: U256::from(1),
					nonce: 0,
					contract_data: None,
				}],
				..Default::default()
			};
			assert!(
				std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| invalid.build())).is_err()
			);
			assert_eq!(root(), before);
		});
	}
}
