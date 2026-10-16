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

//! Historic block syncing logic for the Ethereum JSON-RPC server.

use crate::{
	BlockInfoProvider,
	client::{Client, ClientError, GapFillRequest, SubstrateBlockNumber},
};
use pallet_revive::evm::H256;
use std::future::Future;
use subxt::{Error::Metadata, error::MetadataError, ext::subxt_rpcs::UserError};
use tokio::sync::mpsc;

const LOG_TARGET: &str = "eth-rpc::block-sync";

/// Trait for types that can be used as keys in the `sync_state` table.
pub trait SyncStateKey: std::fmt::Display {}

/// Labels used to track sync progress in the `sync_state` table.
#[derive(Debug, Clone, Copy, derive_more::Display)]
pub enum SyncLabel {
	/// Lowest synced block. Only decreases.
	#[display(fmt = "sync-tail")]
	Tail,
	/// Highest synced block. Absent means no sync has started.
	/// During backfill: upper boundary being filled.
	/// After backfill: advanced by the finalized-block subscription.
	#[display(fmt = "sync-head")]
	Head,
}

/// Chain metadata stored in the `sync_state` table.
#[derive(Debug, Clone, Copy, derive_more::Display)]
pub enum ChainMetadata {
	/// Genesis block hash — used for chain identity verification.
	#[display(fmt = "chain-genesis")]
	Genesis,
	/// Auto-discovered first EVM block on the chain.
	#[display(fmt = "chain-first-evm-block")]
	FirstEvmBlock,
}

impl SyncStateKey for SyncLabel {}
impl SyncStateKey for ChainMetadata {}

/// Sync checkpoint persisted in the `sync_state` table to allow resuming after a restart.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SyncCheckpoint {
	pub block_number: SubstrateBlockNumber,
	pub block_hash: Option<H256>,
}

impl SyncCheckpoint {
	/// Create a checkpoint with a known block hash.
	pub fn new(block_number: SubstrateBlockNumber, block_hash: H256) -> Self {
		Self { block_number, block_hash: Some(block_hash) }
	}

	/// Create a checkpoint with only a block number (no hash).
	pub fn from_number(block_number: SubstrateBlockNumber) -> Self {
		Self { block_number, block_hash: None }
	}
}

/// How often (in blocks) the backward sync checkpoints are persisted to the database.
const BLOCK_INTERVAL: u32 = 128;

/// 旧 Runtime 尚未导出 Revive API 时，只接受准确的缺方法错误作为历史边界。
/// 网络、状态和已支持 Revive 后的执行错误不能被当作空 Ethereum 区块。
fn missing_eth_block_api(error: &ClientError) -> bool {
	match error {
		ClientError::SubxtError(Metadata(MetadataError::RuntimeMethodNotFound(name))) => {
			name == "ReviveApi_eth_block"
		},
		ClientError::SubxtError(subxt::Error::Rpc(subxt::error::RpcError::ClientError(
			subxt::ext::subxt_rpcs::Error::User(UserError { code: 4003, message, data: None }),
		))) => matches!(message.as_str(),
			"Exported method ReviveApi_eth_block is not found" |
			"Client error: Execution failed: Other: Exported method ReviveApi_eth_block is not found"
		),
		_ => false,
	}
}

/// 只跳过近期窗口开头连续的旧 Runtime 区块；一旦发现 Ethereum 区块，后续错误即失败。
async fn scan_recent_evm_blocks<F, Fut>(
	first: SubstrateBlockNumber,
	best: SubstrateBlockNumber,
	mut visit: F,
) -> Result<Option<SubstrateBlockNumber>, ClientError>
where
	F: FnMut(SubstrateBlockNumber) -> Fut,
	Fut: Future<Output = Result<(), ClientError>>,
{
	let mut skipped_old_runtime = false;
	let mut found_evm = false;
	let mut boundary = None;
	for number in first..=best {
		match visit(number).await {
			Ok(()) => {
				if skipped_old_runtime && !found_evm {
					boundary = Some(number);
				}
				found_evm = true;
			},
			Err(error) if !found_evm && number < best && missing_eth_block_api(&error) => {
				skipped_old_runtime = true;
			},
			Err(error) => return Err(error),
		}
	}
	Ok(boundary)
}

/// Options for [`Client::sync_backward_range`].
struct BackwardSyncRange {
	from: SubstrateBlockNumber,
	to: SubstrateBlockNumber,
	/// Set `Head` label after syncing the first block.
	set_head: bool,
	/// Checkpoint `Tail` label periodically and at end.
	checkpoint_tail: bool,
	/// When true, persist the first EVM block boundary if a non-EVM block is encountered.
	persist_first_evm_block: bool,
}

impl Client {
	/// 重启后按最佳链重新建立有限近期索引，链状态不可用时停止初始化。
	/// 不改变节点修剪策略，不在空索引上宣称历史交易不存在。
	pub(crate) async fn rebuild_recent_index(&self, keep_latest: usize) -> Result<(), ClientError> {
		let best = self.block_provider().latest_block_number().await;
		let first = best.saturating_sub((keep_latest - 1) as u32);
		let boundary = scan_recent_evm_blocks(first, best, |number| async move {
			let block = self
				.block_provider()
				.block_by_number(number)
				.await?
				.ok_or(ClientError::BlockNotFound)?;
			self.process_block(&block).await?;
			Ok(())
		})
		.await?;
		if let Some(first_evm) = boundary {
			self.receipt_provider().set_first_evm_block(first_evm).await?;
		}
		Ok(())
	}

	/// Verify that the stored genesis hash matches the connected chain.
	async fn validate_chain_identity(&self) -> Result<H256, ClientError> {
		let genesis_hash: H256 = self.api().genesis_hash();

		if let Some(checkpoint) =
			self.receipt_provider().get_sync_label(ChainMetadata::Genesis).await?
		{
			if let Some(stored) = checkpoint.block_hash {
				if stored != genesis_hash {
					return Err(ClientError::ChainMismatch);
				}
			}
		}

		Ok(genesis_hash)
	}

	/// Verify that a stored boundary block still exists on the finalized chain.
	async fn verify_boundary(&self, checkpoint: &SyncCheckpoint) -> Result<(), ClientError> {
		let num = checkpoint.block_number;
		let hash = checkpoint.block_hash;
		match (num, hash) {
			(_, None) => {
				log::error!(target: LOG_TARGET,
					"Boundary #{num}: missing stored hash");
				Err(ClientError::SyncBoundaryMismatch)
			},
			(_, Some(stored_hash)) => {
				let block = self.block_provider().block_by_number(num).await?.ok_or_else(|| {
					log::error!(target: LOG_TARGET,
						"Boundary #{num}: block not found on chain \
						 (node may have pruned it — use an archive node with --eth-pruning archive)");
					ClientError::SyncBoundaryMismatch
				})?;
				if block.hash() != stored_hash {
					log::error!(target: LOG_TARGET,
						"Boundary #{num}: hash mismatch — stored {stored_hash:?}, \
						 chain {:?}", block.hash());
					return Err(ClientError::SyncBoundaryMismatch);
				}
				Ok(())
			},
		}
	}

	/// Checkpoint the given sync label to the DB.
	async fn checkpoint_sync_label(&self, label: SyncLabel, num: SubstrateBlockNumber, hash: H256) {
		let cp = SyncCheckpoint::new(num, hash);
		let result = match label {
			SyncLabel::Head => self.receipt_provider().advance_sync_label(label, cp).await,
			SyncLabel::Tail => self.receipt_provider().recede_sync_label(label, cp).await,
		};
		if let Err(err) = result {
			log::warn!(target: LOG_TARGET, "Failed to update sync_label[{label}]: {err:?}");
		}
	}

	/// Backward sync historical blocks from the latest finalized block to the first EVM block.
	/// Resumes from the last checkpoint if a previous sync was interrupted.
	/// Fatal errors (chain/DB mismatch) are propagated; transient errors are swallowed
	/// to avoid taking down the RPC server.
	pub async fn sync_backward(&self) -> Result<(), ClientError> {
		log::info!(target: LOG_TARGET,
			"🔄 Historical block sync enabled. \
			 For a complete sync, the connected node should be an archive node.");
		match self.sync_backward_inner().await {
			Ok(()) => Ok(()),
			Err(err) if err.is_chain_validation_error() => Err(err),
			Err(err) => {
				log::error!(target: LOG_TARGET, "🗄️ Sync stopped due to {err}.");
				Ok(())
			},
		}
	}

	async fn sync_backward_inner(&self) -> Result<(), ClientError> {
		let genesis_hash = self.validate_chain_identity().await?;
		let latest_finalized_block = self.latest_finalized_block().await;
		let latest_finalized =
			SyncCheckpoint::new(latest_finalized_block.number(), latest_finalized_block.hash());

		// Store genesis (idempotent).
		self.receipt_provider()
			.set_sync_label(ChainMetadata::Genesis, SyncCheckpoint::new(0, genesis_hash))
			.await?;

		let (head, tail) = tokio::try_join!(
			self.receipt_provider().get_sync_label(SyncLabel::Head),
			self.receipt_provider().get_sync_label(SyncLabel::Tail),
		)?;

		match (tail, head) {
			(Some(tail), Some(head)) => {
				// Verify boundary hashes still match the finalized chain.
				tokio::try_join!(self.verify_boundary(&tail), self.verify_boundary(&head),)?;
				self.sync_backward_resume(tail, head, latest_finalized).await?;
			},
			(Some(_), None) => {
				log::warn!(target: LOG_TARGET,
					"🗄️ Tail exists without Head — possible partial corruption, \
					 starting fresh sync from #{}", latest_finalized.block_number);
				self.sync_backward_fresh(latest_finalized.block_number).await?;
			},
			_ => {
				log::info!(target: LOG_TARGET,
					"🗄️ Fresh sync: syncing backward from #{}", latest_finalized.block_number);
				self.sync_backward_fresh(latest_finalized.block_number).await?;
			},
		}

		self.mark_backfill_complete();

		log::info!(target: LOG_TARGET, "🗄️ Historic sync complete");
		Ok(())
	}

	/// Backward sync from `latest_finalized` down to the first EVM block.
	async fn sync_backward_fresh(
		&self,
		latest_finalized: SubstrateBlockNumber,
	) -> Result<(), ClientError> {
		let first_evm = self.receipt_provider().first_evm_block().unwrap_or(0);
		self.sync_backward_range(BackwardSyncRange {
			from: latest_finalized,
			to: first_evm,
			set_head: true,
			checkpoint_tail: true,
			persist_first_evm_block: true,
		})
		.await
	}

	/// Resume backward sync by filling the top gap (new blocks) and bottom gap (backfill).
	async fn sync_backward_resume(
		&self,
		tail: SyncCheckpoint,
		head: SyncCheckpoint,
		latest_finalized: SyncCheckpoint,
	) -> Result<(), ClientError> {
		log::info!(target: LOG_TARGET,
			"🗄️ Resuming sync: DB has blocks #{}..#{}, chain head is #{}",
			tail.block_number, head.block_number, latest_finalized.block_number);

		let top_gap = async {
			// Top gap: sync from latest_finalized down to head + 1.
			if head.block_number < latest_finalized.block_number {
				self.sync_backward_range(BackwardSyncRange {
					from: latest_finalized.block_number,
					to: head.block_number.saturating_add(1),
					set_head: false,
					checkpoint_tail: false,
					persist_first_evm_block: false,
				})
				.await?;

				// Mark top gap complete so a restart won't redo it.
				self.receipt_provider()
					.advance_sync_label(SyncLabel::Head, latest_finalized)
					.await?;
			}
			Ok::<_, ClientError>(())
		};

		let bottom_gap = async {
			// Bottom gap: sync from tail - 1 down to the first EVM block.
			let first_evm = self.receipt_provider().first_evm_block().unwrap_or(0);
			if tail.block_number > first_evm {
				self.sync_backward_range(BackwardSyncRange {
					from: tail.block_number.saturating_sub(1),
					to: first_evm,
					set_head: false,
					checkpoint_tail: true,
					persist_first_evm_block: true,
				})
				.await?;
			} else {
				log::debug!(target: LOG_TARGET, "🗄️ No backward gap to fill");
			}
			Ok::<_, ClientError>(())
		};

		tokio::try_join!(top_gap, bottom_gap)?;

		Ok(())
	}

	/// Backward sync from block `from` down to block `to` (inclusive).
	/// Stops early if a non-EVM block is discovered (auto-discovery of first EVM block).
	async fn sync_backward_range(
		&self,
		BackwardSyncRange {
			from,
			to,
			set_head,
			checkpoint_tail,
			persist_first_evm_block,
		}: BackwardSyncRange,
	) -> Result<(), ClientError> {
		if from < to {
			log::debug!(target: LOG_TARGET,	"⬇️ Backward sync: nothing to sync (#{from}..#{to})");
			return Ok(());
		}

		log::info!(target: LOG_TARGET, "⬇️ Backward sync: #{from} down to #{to}");

		let mut block = self
			.block_provider()
			.block_by_number(from)
			.await?
			.ok_or(ClientError::BlockNotFound)?;

		let mut blocks_synced = 0u64;
		let mut last_synced: Option<(SubstrateBlockNumber, H256)> = None;
		let at_checkpoint =
			|synced: u64| synced <= 1 || synced.is_multiple_of(u64::from(BLOCK_INTERVAL));

		let loop_result: Result<(), ClientError> = loop {
			let block_number = block.number();
			let block_hash = block.hash();

			let ethereum_hash = match self
				.runtime_api(block_hash)
				.eth_block_hash(pallet_revive::evm::U256::from(block_number))
				.await
			{
				Ok(h) => h,
				Err(err) => {
					log::error!(target: LOG_TARGET,	"⚠️ eth_block_hash failed for #{block_number}: {err:?}, stopping");
					break Err(err.into());
				},
			};

			match ethereum_hash {
				Some(hash) => {
					if let Err(err) =
						self.receipt_provider().insert_block_receipts_past(&block, &hash).await
					{
						log::error!(target: LOG_TARGET,
							"⚠️ Insert failed for #{block_number}: {err:?}, stopping");
						break Err(err);
					}

					last_synced = Some((block_number, block_hash));
					blocks_synced += 1;

					if blocks_synced == 1 && set_head {
						self.checkpoint_sync_label(SyncLabel::Head, block_number, block_hash).await;
					}

					if at_checkpoint(blocks_synced) {
						log::debug!(target: LOG_TARGET,
							"⬇️ Backward sync progress: #{block_number} ({blocks_synced} blocks synced)");
						if checkpoint_tail {
							self.checkpoint_sync_label(SyncLabel::Tail, block_number, block_hash)
								.await;
						}
					}
				},
				None => {
					if persist_first_evm_block {
						let first_evm_block = block_number.saturating_add(1);
						log::debug!(target: LOG_TARGET,
							"🔍 No EVM hash at #{block_number}, setting first_evm_block to #{first_evm_block}");
						if let Err(err) =
							self.receipt_provider().set_first_evm_block(first_evm_block).await
						{
							log::warn!(target: LOG_TARGET, "Failed to persist first-evm-block: {err:?}");
						}
					} else {
						log::debug!(target: LOG_TARGET,
							"🔍 No EVM hash at #{block_number}, skipping first EVM block update");
					}

					break Ok(());
				},
			}

			if block_number > to {
				let parent_hash = block.header().parent_hash;
				match self
					.block_provider()
					.block_by_hash(&parent_hash)
					.await
					.map_err(Into::into)
					.and_then(|opt| opt.ok_or(ClientError::BlockNotFound))
				{
					Ok(b) => block = b,
					Err(err) => {
						log::error!(target: LOG_TARGET,
							"⚠️ Could not fetch parent of #{block_number}: {err:?}, stopping");
						break Err(err);
					},
				}
			} else {
				break Ok(());
			}
		};

		// Checkpoint the last synced block if it wasn't already at a checkpoint interval.
		if loop_result.is_ok() && checkpoint_tail && !at_checkpoint(blocks_synced) {
			if let Some((num, hash)) = last_synced {
				self.checkpoint_sync_label(SyncLabel::Tail, num, hash).await;
			}
		}

		log::info!(target: LOG_TARGET,
			"⬇️ Backward sync: {blocks_synced} blocks synced \
			 (requested #{from}..#{to})");

		loop_result
	}

	/// Run the background subscription gap filler, processing requests sequentially.
	pub(crate) async fn run_subscription_gap_filler(&self, mut rx: mpsc::Receiver<GapFillRequest>) {
		log::info!(target: LOG_TARGET, "🔄 Subscription gap filler started");

		while let Some(GapFillRequest { from_inclusive, to_inclusive }) = rx.recv().await {
			log::info!(target: LOG_TARGET, "🔄 Subscription gap filler: processing #{from_inclusive} down to #{to_inclusive}");
			if let Err(err) = self
				.sync_backward_range(BackwardSyncRange {
					from: from_inclusive,
					to: to_inclusive,
					set_head: false,
					checkpoint_tail: false,
					persist_first_evm_block: false,
				})
				.await
			{
				log::error!(target: LOG_TARGET, "🔄 Subscription gap fill failed for #{from_inclusive}..#{to_inclusive}: {err:?}");
			} else {
				log::info!(target: LOG_TARGET, "🔄 Subscription gap filler: done with #{from_inclusive}..#{to_inclusive}");
			}
			// Mark done unconditionally — mirrors how subscribe_new_blocks handles
			// callback errors: log and move on.
			self.subscription_gap_queue().mark_done();
		}

		log::info!(target: LOG_TARGET, "🔄 Subscription gap filler stopped");
	}
}

#[cfg(test)]
mod recent_runtime_upgrade_tests {
	use super::{missing_eth_block_api, scan_recent_evm_blocks};
	use crate::client::ClientError;
	use subxt::{Error::Metadata, error::MetadataError, ext::subxt_rpcs::UserError};

	fn missing_api(name: &str) -> ClientError {
		ClientError::SubxtError(Metadata(MetadataError::RuntimeMethodNotFound(name.into())))
	}

	fn rpc_missing_api(message: &str) -> ClientError {
		rpc_error(UserError { code: 4003, message: message.into(), data: None })
	}

	fn rpc_error(error: UserError) -> ClientError {
		ClientError::SubxtError(subxt::Error::Rpc(subxt::error::RpcError::ClientError(
			subxt::ext::subxt_rpcs::Error::User(error),
		)))
	}

	// 使用真实 state RPC 错误转换，覆盖框架包装后的消息、代码与 data。
	fn wrapped_missing_api() -> ClientError {
		let original = std::io::Error::other(
			"Execution failed: Other: Exported method ReviveApi_eth_block is not found",
		);
		let error: jsonrpsee::types::ErrorObjectOwned =
			sc_rpc_api::state::error::Error::Client(Box::new(original)).into();
		rpc_error(UserError {
			code: error.code().into(),
			message: error.message().into(),
			data: error.data().map(ToOwned::to_owned),
		})
	}

	#[test]
	fn rpc_missing_error_requires_exact_message_code_and_no_data() {
		let plain = "Exported method ReviveApi_eth_block is not found";
		let wrapped = "Client error: Execution failed: Other: Exported method ReviveApi_eth_block is not found";
		assert!(missing_eth_block_api(&rpc_missing_api(plain)));
		assert!(missing_eth_block_api(&wrapped_missing_api()));
		for message in [
			"Client error: Execution failed: Other: Exported method ReviveApi_balance is not found",
			"database unavailable: Exported method ReviveApi_eth_block is not found",
			"Client error: Execution failed: Other: Exported method ReviveApi_eth_block is not found extra",
			" Exported method ReviveApi_eth_block is not found",
			"Exported method ReviveApi_eth_block is not found\n",
		] {
			assert!(!missing_eth_block_api(&rpc_missing_api(message)), "{message}");
		}
		for message in [plain, wrapped] {
			for code in [4004, -32603] {
				assert!(!missing_eth_block_api(&rpc_error(UserError {
					code, message: message.into(), data: None,
				})));
			}
			assert!(!missing_eth_block_api(&rpc_error(UserError {
				code: 4003, message: message.into(),
				data: Some(serde_json::value::RawValue::from_string("null".into()).unwrap()),
			})));
		}
	}

	#[tokio::test]
	async fn wrapped_runtime_prefix_discovers_boundary_without_fixed_height() {
		let mut visited = Vec::new();
		let boundary = scan_recent_evm_blocks(9, 13, |number| {
			visited.push(number);
			async move { if number < 12 { Err(wrapped_missing_api()) } else { Ok(()) } }
		}).await.expect("准确框架错误只允许跨过连续旧前缀");
		assert_eq!(boundary, Some(12));
		assert_eq!(visited, vec![9, 10, 11, 12, 13]);
	}

	#[tokio::test]
	async fn wrapped_missing_after_evm_or_at_best_is_returned_unchanged() {
		let expected = wrapped_missing_api().to_string();
		let result = scan_recent_evm_blocks(9, 11, |number| async move {
			if number == 9 { Ok(()) } else { Err(wrapped_missing_api()) }
		}).await;
		assert_eq!(result.unwrap_err().to_string(), expected);
		for first in [9, 11] {
			let result = scan_recent_evm_blocks(first, 11, |_| async { Err(wrapped_missing_api()) }).await;
			assert_eq!(result.unwrap_err().to_string(), expected);
		}
	}

	#[tokio::test]
	async fn unrelated_errors_stop_old_prefix_without_replacement() {
		for error in [
			ClientError::BlockNotFound,
			missing_api("ReviveApi_balance"),
			rpc_missing_api("Client error: Execution failed: Other: state unavailable"),
		] {
			let expected = error.to_string();
			let mut error = Some(error);
			let result = scan_recent_evm_blocks(0, 2, |_| std::future::ready(
				Err(error.take().expect("其它错误必须在首块立即返回")),
			)).await;
			assert_eq!(result.unwrap_err().to_string(), expected);
		}
	}

	#[tokio::test]
	async fn old_runtime_prefix_has_one_first_evm_boundary() {
		let boundary = scan_recent_evm_blocks(0, 80, |number| async move {
			if number < 79 {
				Err(rpc_missing_api("Exported method ReviveApi_eth_block is not found"))
			} else {
				Ok(())
			}
		})
		.await
		.expect("升级前历史应允许重建");
		assert_eq!(boundary, Some(79));
	}

	#[tokio::test]
	async fn evm_only_window_needs_no_new_boundary() {
		let boundary = scan_recent_evm_blocks(79, 80, |_| async { Ok(()) })
			.await
			.expect("全部区块均支持 Revive");
		assert_eq!(boundary, None);
	}

	#[tokio::test]
	async fn missing_api_after_first_evm_block_stops_rebuild() {
		let result = scan_recent_evm_blocks(78, 81, |number| async move {
			if number == 78 || number == 81 {
				Err(missing_api("ReviveApi_eth_block"))
			} else {
				Ok(())
			}
		})
		.await;
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn wrong_method_or_no_current_evm_block_stops_rebuild() {
		assert!(!missing_eth_block_api(&missing_api("ReviveApi_balance")));
		assert!(!missing_eth_block_api(&rpc_missing_api(
			"Exported method ReviveApi_balance is not found"
		)));
		assert!(!missing_eth_block_api(&ClientError::BlockNotFound));
		assert!(scan_recent_evm_blocks(0, 1, |_| async {
			Err(missing_api("ReviveApi_balance"))
		})
		.await
		.is_err());
		assert!(scan_recent_evm_blocks(0, 1, |_| async {
			Err(missing_api("ReviveApi_eth_block"))
		})
		.await
		.is_err());
	}
}
