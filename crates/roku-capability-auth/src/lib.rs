//! Capability issuance and verification.

use roku_common_types::CapabilityToken;

#[derive(Debug, Clone)]
pub struct CapabilityRequest {
	pub subject: String,
	pub resource: String,
	pub actions: Vec<String>,
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
		now_unix: u64,
	) -> bool {
		let accepted = token.expires_at_unix >= now_unix
			&& token.actions.iter().any(|action| action == required_action);

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
			resource: "tool.echo".to_string(),
			actions: vec!["read".to_string()],
			expires_at_unix: 999,
		});

		assert!(!auth.verify(&token, "invoke", 100));
	}
}
