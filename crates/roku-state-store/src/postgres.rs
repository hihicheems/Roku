use std::env;

use postgres::{Client, NoTls};
use roku_common_types::{ConversationRole, ConversationTurn, PlanningModeHint, SessionPreferences};

use crate::{ConversationRepository, SessionPreferenceRepository, StoreError};

const DEFAULT_POSTGRES_SCHEMA: &str = "roku_agent";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresStoreConfig {
	pub database_url: String,
	pub schema: String,
}

impl PostgresStoreConfig {
	pub fn from_env() -> Result<Option<Self>, StoreError> {
		let Some(database_url) = env::var("ROKU_DATABASE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.or_else(|| {
				env::var("DATABASE_URL")
					.ok()
					.filter(|value| !value.trim().is_empty())
			})
		else {
			return Ok(None);
		};

		let schema = env::var("ROKU_DATABASE_SCHEMA")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.unwrap_or_else(|| DEFAULT_POSTGRES_SCHEMA.to_string());
		validate_identifier(&schema)?;

		Ok(Some(Self {
			database_url,
			schema,
		}))
	}
}

pub struct PostgresSessionPreferenceRepository {
	database_url: String,
	schema: String,
}

impl PostgresSessionPreferenceRepository {
	pub fn connect(config: PostgresStoreConfig) -> Result<Self, StoreError> {
		let repository = Self {
			database_url: config.database_url,
			schema: config.schema,
		};
		repository.ensure_schema_objects()?;
		Ok(repository)
	}

	fn connect_client(&self) -> Result<Client, StoreError> {
		Client::connect(&self.database_url, NoTls).map_err(postgres_error)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_session_preferences_table(&mut client, &self.schema)
	}
}

impl SessionPreferenceRepository for PostgresSessionPreferenceRepository {
	fn save_preferences(
		&mut self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"INSERT INTO {}.session_preferences (session_id, planning_mode) VALUES ($1, $2) \
			 ON CONFLICT (session_id) DO UPDATE SET planning_mode = EXCLUDED.planning_mode, updated_at = NOW()",
			self.schema,
		);
		let planning_mode = preferences.planning_mode.map(planning_mode_label);
		client
			.execute(&query, &[&session_id, &planning_mode])
			.map_err(postgres_error)?;
		Ok(())
	}

	fn load_preferences(&self, session_id: &str) -> Result<Option<SessionPreferences>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT planning_mode FROM {}.session_preferences WHERE session_id = $1",
			self.schema,
		);
		let row = client
			.query_opt(&query, &[&session_id])
			.map_err(postgres_error)?;
		Ok(row.map(|row| SessionPreferences {
			planning_mode: row
				.get::<_, Option<String>>(0)
				.as_deref()
				.and_then(parse_planning_mode),
		}))
	}
}

pub struct PostgresConversationRepository {
	database_url: String,
	schema: String,
}

impl PostgresConversationRepository {
	pub fn connect(config: PostgresStoreConfig) -> Result<Self, StoreError> {
		let repository = Self {
			database_url: config.database_url,
			schema: config.schema,
		};
		repository.ensure_schema_objects()?;
		Ok(repository)
	}

	fn connect_client(&self) -> Result<Client, StoreError> {
		Client::connect(&self.database_url, NoTls).map_err(postgres_error)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_conversation_turns_table(&mut client, &self.schema)
	}
}

impl ConversationRepository for PostgresConversationRepository {
	fn append_turn(
		&mut self,
		session_id: &str,
		turn: ConversationTurn,
	) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"INSERT INTO {}.conversation_turns (session_id, role, content, created_at_unix_ms) VALUES ($1, $2, $3, $4)",
			self.schema,
		);
		let role = conversation_role_label(turn.role);
		client
			.execute(
				&query,
				&[
					&session_id,
					&role,
					&turn.content,
					&(turn.created_at_unix_ms as i64),
				],
			)
			.map_err(postgres_error)?;
		Ok(())
	}

	fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT role, content, created_at_unix_ms FROM {}.conversation_turns WHERE session_id = $1 ORDER BY seq DESC LIMIT $2",
			self.schema,
		);
		let rows = client
			.query(
				&query,
				&[
					&session_id,
					&(i64::try_from(limit).unwrap_or(i64::MAX)),
				],
			)
			.map_err(postgres_error)?;
		let mut turns = rows
			.into_iter()
			.filter_map(|row| {
				parse_conversation_role(row.get::<_, String>(0).as_str()).map(|role| ConversationTurn {
					role,
					content: row.get::<_, String>(1),
					created_at_unix_ms: row.get::<_, i64>(2).try_into().unwrap_or(u64::MAX),
				})
			})
			.collect::<Vec<_>>();
		turns.reverse();
		Ok(turns)
	}
}

fn ensure_schema(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
		.map_err(postgres_error)
}

fn ensure_session_preferences_table(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!(
			"CREATE TABLE IF NOT EXISTS {schema}.session_preferences (\
			 session_id TEXT PRIMARY KEY,\
			 planning_mode TEXT NULL,\
			 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
			 )"
		))
		.map_err(postgres_error)
}

fn ensure_conversation_turns_table(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!(
			"CREATE TABLE IF NOT EXISTS {schema}.conversation_turns (\
			 seq BIGSERIAL PRIMARY KEY,\
			 session_id TEXT NOT NULL,\
			 role TEXT NOT NULL,\
			 content TEXT NOT NULL,\
			 created_at_unix_ms BIGINT NOT NULL\
			 );\
			 CREATE INDEX IF NOT EXISTS idx_{schema}_conversation_turns_session_seq\
			 ON {schema}.conversation_turns (session_id, seq DESC)"
		))
		.map_err(postgres_error)
}

fn validate_identifier(identifier: &str) -> Result<(), StoreError> {
	if identifier.is_empty()
		|| !identifier
			.chars()
			.all(|character| character.is_ascii_alphanumeric() || character == '_')
	{
		return Err(StoreError::Postgres(format!(
			"invalid postgres identifier: {identifier}"
		)));
	}

	Ok(())
}

fn planning_mode_label(mode: PlanningModeHint) -> &'static str {
	match mode {
		PlanningModeHint::ReAct => "ReAct",
		PlanningModeHint::TaskDecomposition => "TaskDecomposition",
		PlanningModeHint::TreeSearch => "TreeSearch",
		PlanningModeHint::IterativeRefinement => "IterativeRefinement",
	}
}

fn parse_planning_mode(value: &str) -> Option<PlanningModeHint> {
	match value {
		"ReAct" => Some(PlanningModeHint::ReAct),
		"TaskDecomposition" => Some(PlanningModeHint::TaskDecomposition),
		"TreeSearch" => Some(PlanningModeHint::TreeSearch),
		"IterativeRefinement" => Some(PlanningModeHint::IterativeRefinement),
		_ => None,
	}
}

fn conversation_role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}

fn parse_conversation_role(value: &str) -> Option<ConversationRole> {
	match value {
		"user" => Some(ConversationRole::User),
		"assistant" => Some(ConversationRole::Assistant),
		"system" => Some(ConversationRole::System),
		_ => None,
	}
}

fn postgres_error(error: postgres::Error) -> StoreError {
	StoreError::Postgres(error.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validate_identifier_rejects_invalid_schema_name() {
		let error = validate_identifier("roku-agent").expect_err("invalid schema should fail");
		assert!(error.to_string().contains("invalid postgres identifier"));
	}

	#[test]
	fn parse_mode_roundtrip_is_stable() {
		assert_eq!(
			parse_planning_mode(planning_mode_label(PlanningModeHint::TreeSearch)),
			Some(PlanningModeHint::TreeSearch)
		);
	}
}
