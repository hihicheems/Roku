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

//! Capability issuance and verification.

use roku_common_types::{CapabilityToken, ResourceSelector};

#[derive(Debug, Clone)]
pub struct CapabilityRequest {
	pub subject: String,
	pub resource: ResourceSelector,
	pub actions: Vec<String>,
	pub granted_capabilities: Vec<String>,
	pub expires_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct AuditEvent {
	pub token_id: String,
	pub accepted: bool,
	pub reason: String,
}

#[derive(Debug, Default)]
pub struct CapabilityAuthority {
	audit_log: Vec<AuditEvent>,
}

impl CapabilityAuthority {
	pub fn issue(&mut self, request: CapabilityRequest) -> CapabilityToken {
		let token = CapabilityToken {
			token_id: format!("cap-{}", self.audit_log.len() + 1),
			subject: request.subject,
			resource: request.resource,
			actions: request.actions,
			granted_capabilities: request.granted_capabilities,
			expires_at_unix: request.expires_at_unix,
		};
		self.audit_log.push(AuditEvent {
			token_id: token.token_id.clone(),
			accepted: true,
			reason: "issued".to_string(),
		});
		token
	}

	pub fn attenuate(
		&mut self,
		parent: &CapabilityToken,
		allowed_actions: &[String],
	) -> CapabilityToken {
		let actions = parent
			.actions
			.iter()
			.filter(|a| allowed_actions.contains(a))
			.cloned()
			.collect::<Vec<_>>();

		let token = CapabilityToken {
			token_id: format!("{}-child", parent.token_id),
			subject: parent.subject.clone(),
			resource: parent.resource.clone(),
			actions,
			granted_capabilities: parent.granted_capabilities.clone(),
			expires_at_unix: parent.expires_at_unix,
		};

		self.audit_log.push(AuditEvent {
			token_id: token.token_id.clone(),
			accepted: true,
			reason: "attenuated".to_string(),
		});

		token
	}

	pub fn verify(
		&mut self,
		token: &CapabilityToken,
		required_action: &str,
		required_capability: Option<&str>,
		now_unix: u64,
	) -> bool {
		let accepted = token.expires_at_unix >= now_unix
			&& token.actions.iter().any(|action| action == required_action)
			&& required_capability.is_none_or(|capability| {
				token
					.granted_capabilities
					.iter()
					.any(|granted| granted == capability)
			});

		self.audit_log.push(AuditEvent {
			token_id: token.token_id.clone(),
			accepted,
			reason: if accepted {
				"verified".to_string()
			} else {
				"rejected".to_string()
			},
		});

		accepted
	}

	pub fn audit_log(&self) -> &[AuditEvent] {
		&self.audit_log
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn deny_missing_action() {
		let mut auth = CapabilityAuthority::default();
		let token = auth.issue(CapabilityRequest {
			subject: "agent-a".to_string(),
			resource: ResourceSelector::tool("tool.echo"),
			actions: vec!["read".to_string()],
			granted_capabilities: vec!["tool.echo".to_string()],
			expires_at_unix: 999,
		});

		assert!(!auth.verify(&token, "invoke", Some("tool.echo"), 100));
	}

	#[test]
	fn attenuation_drops_unlisted_actions() {
		let mut auth = CapabilityAuthority::default();
		let parent = auth.issue(CapabilityRequest {
			subject: "agent-a".to_string(),
			resource: ResourceSelector::tool("tool.echo"),
			actions: vec!["invoke".to_string(), "read".to_string()],
			granted_capabilities: vec!["tool.echo".to_string()],
			expires_at_unix: 999,
		});
		let child = auth.attenuate(&parent, &[String::from("invoke")]);

		assert!(auth.verify(&child, "invoke", Some("tool.echo"), 100));
		assert!(!auth.verify(&child, "read", Some("tool.echo"), 100));
	}

	#[test]
	fn expired_token_is_denied() {
		let mut auth = CapabilityAuthority::default();
		let token = auth.issue(CapabilityRequest {
			subject: "agent-a".to_string(),
			resource: ResourceSelector::tool("tool.echo"),
			actions: vec!["invoke".to_string()],
			granted_capabilities: vec!["tool.echo".to_string()],
			expires_at_unix: 10,
		});

		assert!(!auth.verify(&token, "invoke", Some("tool.echo"), 11));
	}
}
