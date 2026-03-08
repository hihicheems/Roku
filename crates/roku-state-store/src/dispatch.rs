// Copyright 2025 itscheems
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::{HashMap, VecDeque};

use roku_common_types::{NodeId, TaskId};

use crate::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchEnvelope {
	pub entry_id: String,
	pub task_id: TaskId,
	pub node_id: NodeId,
	pub attempt: u32,
	pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchLease {
	pub entry_id: String,
	pub consumer_id: String,
	pub lease_token: String,
	pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchClaim {
	pub envelope: DispatchEnvelope,
	pub lease: DispatchLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryClaim {
	pub next_attempt: u32,
	pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackpressureSnapshot {
	pub queued: usize,
	pub leased: usize,
	pub max_in_flight: usize,
	pub available_slots: usize,
}

pub trait DispatchQueue {
	fn publish(&mut self, envelope: DispatchEnvelope) -> Result<(), StoreError>;
	fn claim(
		&mut self,
		consumer_id: &str,
		now_unix_ms: u64,
	) -> Result<Option<DispatchClaim>, StoreError>;
	fn ack(&mut self, lease: &DispatchLease) -> Result<(), StoreError>;
	fn nack(&mut self, lease: &DispatchLease, retry: RetryClaim) -> Result<(), StoreError>;
	fn renew_lease(
		&mut self,
		lease: &DispatchLease,
		now_unix_ms: u64,
	) -> Result<Option<DispatchLease>, StoreError>;
	fn backpressure(&self) -> BackpressureSnapshot;
}

#[derive(Debug, Clone)]
struct LeasedEntry {
	envelope: DispatchEnvelope,
	lease: DispatchLease,
}

#[derive(Debug, Clone)]
pub struct InMemoryDispatchQueue {
	queued: VecDeque<DispatchEnvelope>,
	leased: HashMap<String, LeasedEntry>,
	lease_duration_ms: u64,
	max_in_flight: usize,
	lease_sequence: u64,
}

impl Default for InMemoryDispatchQueue {
	fn default() -> Self {
		Self {
			queued: VecDeque::new(),
			leased: HashMap::new(),
			lease_duration_ms: 30_000,
			max_in_flight: 32,
			lease_sequence: 0,
		}
	}
}

impl InMemoryDispatchQueue {
	pub fn with_limits(max_in_flight: usize, lease_duration_ms: u64) -> Self {
		Self {
			max_in_flight,
			lease_duration_ms,
			..Self::default()
		}
	}

	fn requeue_expired(&mut self, now_unix_ms: u64) {
		let expired_ids = self
			.leased
			.values()
			.filter(|entry| entry.lease.expires_at_unix_ms <= now_unix_ms)
			.map(|entry| entry.envelope.entry_id.clone())
			.collect::<Vec<_>>();

		for entry_id in expired_ids {
			if let Some(entry) = self.leased.remove(&entry_id) {
				self.queued.push_front(entry.envelope);
			}
		}
	}

	fn next_lease(&mut self, entry_id: &str, consumer_id: &str, now_unix_ms: u64) -> DispatchLease {
		self.lease_sequence = self.lease_sequence.saturating_add(1);
		DispatchLease {
			entry_id: entry_id.to_string(),
			consumer_id: consumer_id.to_string(),
			lease_token: format!("lease-{entry_id}-{}", self.lease_sequence),
			expires_at_unix_ms: now_unix_ms.saturating_add(self.lease_duration_ms),
		}
	}

	fn verify_lease(&self, lease: &DispatchLease) -> Result<(), StoreError> {
		let Some(entry) = self.leased.get(&lease.entry_id) else {
			return Err(StoreError::Postgres(format!(
				"dispatch lease not found for {}",
				lease.entry_id
			)));
		};

		if entry.lease != *lease {
			return Err(StoreError::Postgres(format!(
				"dispatch lease mismatch for {}",
				lease.entry_id
			)));
		}

		Ok(())
	}
}

impl DispatchQueue for InMemoryDispatchQueue {
	fn publish(&mut self, envelope: DispatchEnvelope) -> Result<(), StoreError> {
		self.queued.push_back(envelope);
		Ok(())
	}

	fn claim(
		&mut self,
		consumer_id: &str,
		now_unix_ms: u64,
	) -> Result<Option<DispatchClaim>, StoreError> {
		self.requeue_expired(now_unix_ms);
		if self.leased.len() >= self.max_in_flight {
			return Ok(None);
		}

		let Some(envelope) = self.queued.pop_front() else {
			return Ok(None);
		};
		let lease = self.next_lease(&envelope.entry_id, consumer_id, now_unix_ms);
		self.leased.insert(
			envelope.entry_id.clone(),
			LeasedEntry {
				envelope: envelope.clone(),
				lease: lease.clone(),
			},
		);

		Ok(Some(DispatchClaim { envelope, lease }))
	}

	fn ack(&mut self, lease: &DispatchLease) -> Result<(), StoreError> {
		self.verify_lease(lease)?;
		self.leased.remove(&lease.entry_id);
		Ok(())
	}

	fn nack(&mut self, lease: &DispatchLease, retry: RetryClaim) -> Result<(), StoreError> {
		self.verify_lease(lease)?;
		let Some(mut entry) = self.leased.remove(&lease.entry_id) else {
			return Err(StoreError::Postgres(format!(
				"dispatch lease not found for {}",
				lease.entry_id
			)));
		};
		entry.envelope.attempt = retry.next_attempt;
		self.queued.push_back(entry.envelope);
		Ok(())
	}

	fn renew_lease(
		&mut self,
		lease: &DispatchLease,
		now_unix_ms: u64,
	) -> Result<Option<DispatchLease>, StoreError> {
		self.requeue_expired(now_unix_ms);
		self.verify_lease(lease)?;
		let renewed = self.next_lease(&lease.entry_id, &lease.consumer_id, now_unix_ms);
		if let Some(entry) = self.leased.get_mut(&lease.entry_id) {
			entry.lease = renewed.clone();
		}
		Ok(Some(renewed))
	}

	fn backpressure(&self) -> BackpressureSnapshot {
		BackpressureSnapshot {
			queued: self.queued.len(),
			leased: self.leased.len(),
			max_in_flight: self.max_in_flight,
			available_slots: self.max_in_flight.saturating_sub(self.leased.len()),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn envelope(entry_id: &str, attempt: u32) -> DispatchEnvelope {
		DispatchEnvelope {
			entry_id: entry_id.to_string(),
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			attempt,
			payload: format!("payload-{entry_id}"),
		}
	}

	#[test]
	fn publish_claim_ack_roundtrip() {
		let mut queue = InMemoryDispatchQueue::with_limits(1, 10);
		queue
			.publish(envelope("entry-1", 1))
			.expect("publish should succeed");

		let claim = queue
			.claim("worker-a", 100)
			.expect("claim should succeed")
			.expect("entry should be available");
		assert_eq!(claim.envelope.entry_id, "entry-1");
		assert_eq!(queue.backpressure().leased, 1);

		queue.ack(&claim.lease).expect("ack should succeed");
		assert_eq!(queue.backpressure().leased, 0);
		assert_eq!(queue.backpressure().queued, 0);
	}

	#[test]
	fn nack_requeues_with_retry_attempt() {
		let mut queue = InMemoryDispatchQueue::with_limits(1, 10);
		queue
			.publish(envelope("entry-1", 1))
			.expect("publish should succeed");

		let claim = queue
			.claim("worker-a", 100)
			.expect("claim should succeed")
			.expect("entry should be available");
		queue
			.nack(
				&claim.lease,
				RetryClaim {
					next_attempt: 2,
					reason: "retryable".to_string(),
				},
			)
			.expect("nack should succeed");

		let reclaimed = queue
			.claim("worker-b", 101)
			.expect("second claim should succeed")
			.expect("entry should be requeued");
		assert_eq!(reclaimed.envelope.attempt, 2);
	}

	#[test]
	fn backpressure_blocks_claims_at_in_flight_limit() {
		let mut queue = InMemoryDispatchQueue::with_limits(1, 10);
		queue
			.publish(envelope("entry-1", 1))
			.expect("publish should succeed");
		queue
			.publish(envelope("entry-2", 1))
			.expect("publish should succeed");

		let first = queue.claim("worker-a", 100).expect("claim should succeed");
		assert!(first.is_some());
		let blocked = queue.claim("worker-b", 101).expect("claim should succeed");
		assert!(blocked.is_none());
		assert_eq!(queue.backpressure().queued, 1);
		assert_eq!(queue.backpressure().available_slots, 0);
	}

	#[test]
	fn expired_leases_are_requeued_for_other_consumers() {
		let mut queue = InMemoryDispatchQueue::with_limits(1, 10);
		queue
			.publish(envelope("entry-1", 1))
			.expect("publish should succeed");

		let first = queue
			.claim("worker-a", 100)
			.expect("claim should succeed")
			.expect("entry should be claimed");
		let second = queue
			.claim("worker-b", 111)
			.expect("claim should succeed")
			.expect("expired lease should be requeued");

		assert_eq!(first.envelope.entry_id, second.envelope.entry_id);
		assert_eq!(second.lease.consumer_id, "worker-b");
	}
}
