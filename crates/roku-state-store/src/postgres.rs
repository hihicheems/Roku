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

use std::env;

use postgres::{Client, NoTls};
use roku_common_types::{
	ApprovalId, ApprovalTicket, ConversationRole, ConversationTurn, NodeId, PlanningModeHint,
	ResultEnvelope, SessionPreferences, Task, TaskEvent, TaskId,
};

use crate::{
	ApprovalRepository, ConversationRepository, EventRepository, ResultRepository,
	SessionPreferenceRepository, StoreError, TaskRepository,
};

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

pub struct PostgresTaskRepository {
	database_url: String,
	schema: String,
}

impl PostgresTaskRepository {
	pub fn connect(config: PostgresStoreConfig) -> Result<Self, StoreError> {
		let repository = Self {
			database_url: config.database_url,
			schema: config.schema,
		};
		repository.ensure_schema_objects()?;
		Ok(repository)
	}

	fn connect_client(&self) -> Result<Client, StoreError> {
		connect_client(&self.database_url)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_tasks_table(&mut client, &self.schema)
	}
}

impl TaskRepository for PostgresTaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"INSERT INTO {}.tasks (task_id, task_json) VALUES ($1, $2) \
			 ON CONFLICT (task_id) DO UPDATE SET task_json = EXCLUDED.task_json, updated_at = NOW()",
			self.schema,
		);
		let encoded = serde_json::to_string(&task)?;
		client
			.execute(&query, &[&task.task_id.0, &encoded])
			.map_err(postgres_error)?;
		Ok(())
	}

	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT task_json FROM {}.tasks WHERE task_id = $1",
			self.schema,
		);
		let row = client
			.query_opt(&query, &[&task_id.0])
			.map_err(postgres_error)?;
		row.map(|row| {
			serde_json::from_str::<Task>(&row.get::<_, String>(0)).map_err(StoreError::from)
		})
		.transpose()
	}
}

pub struct PostgresEventRepository {
	database_url: String,
	schema: String,
}

impl PostgresEventRepository {
	pub fn connect(config: PostgresStoreConfig) -> Result<Self, StoreError> {
		let repository = Self {
			database_url: config.database_url,
			schema: config.schema,
		};
		repository.ensure_schema_objects()?;
		Ok(repository)
	}

	fn connect_client(&self) -> Result<Client, StoreError> {
		connect_client(&self.database_url)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_task_events_table(&mut client, &self.schema)
	}
}

impl EventRepository for PostgresEventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"INSERT INTO {}.task_events (task_id, event_json) VALUES ($1, $2)",
			self.schema,
		);
		let encoded = serde_json::to_string(&event)?;
		client
			.execute(&query, &[&event.task_id.0, &encoded])
			.map_err(postgres_error)?;
		Ok(())
	}

	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT event_json FROM {}.task_events WHERE task_id = $1 ORDER BY seq ASC",
			self.schema,
		);
		let rows = client
			.query(&query, &[&task_id.0])
			.map_err(postgres_error)?;
		rows.into_iter()
			.map(|row| {
				serde_json::from_str::<TaskEvent>(&row.get::<_, String>(0))
					.map_err(StoreError::from)
			})
			.collect()
	}
}

pub struct PostgresApprovalRepository {
	database_url: String,
	schema: String,
}

impl PostgresApprovalRepository {
	pub fn connect(config: PostgresStoreConfig) -> Result<Self, StoreError> {
		let repository = Self {
			database_url: config.database_url,
			schema: config.schema,
		};
		repository.ensure_schema_objects()?;
		Ok(repository)
	}

	fn connect_client(&self) -> Result<Client, StoreError> {
		connect_client(&self.database_url)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_approval_tickets_table(&mut client, &self.schema)
	}
}

impl ApprovalRepository for PostgresApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"INSERT INTO {}.approval_tickets (approval_id, task_id, ticket_json) VALUES ($1, $2, $3) \
			 ON CONFLICT (approval_id) DO UPDATE SET task_id = EXCLUDED.task_id, ticket_json = EXCLUDED.ticket_json, updated_at = NOW()",
			self.schema,
		);
		let encoded = serde_json::to_string(&ticket)?;
		client
			.execute(
				&query,
				&[&ticket.approval_id.0, &ticket.task_id.0, &encoded],
			)
			.map_err(postgres_error)?;
		Ok(())
	}

	fn load_ticket(&self, approval_id: &ApprovalId) -> Result<Option<ApprovalTicket>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT ticket_json FROM {}.approval_tickets WHERE approval_id = $1",
			self.schema,
		);
		let row = client
			.query_opt(&query, &[&approval_id.0])
			.map_err(postgres_error)?;
		row.map(|row| {
			serde_json::from_str::<ApprovalTicket>(&row.get::<_, String>(0))
				.map_err(StoreError::from)
		})
		.transpose()
	}
}

pub struct PostgresResultRepository {
	database_url: String,
	schema: String,
}

impl PostgresResultRepository {
	pub fn connect(config: PostgresStoreConfig) -> Result<Self, StoreError> {
		let repository = Self {
			database_url: config.database_url,
			schema: config.schema,
		};
		repository.ensure_schema_objects()?;
		Ok(repository)
	}

	fn connect_client(&self) -> Result<Client, StoreError> {
		connect_client(&self.database_url)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_node_results_table(&mut client, &self.schema)
	}
}

impl ResultRepository for PostgresResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"INSERT INTO {}.node_results (task_id, node_id, result_json) VALUES ($1, $2, $3) \
			 ON CONFLICT (task_id, node_id) DO UPDATE SET result_json = EXCLUDED.result_json, updated_at = NOW()",
			self.schema,
		);
		let encoded = serde_json::to_string(&result)?;
		client
			.execute(&query, &[&result.task_id.0, &result.node_id.0, &encoded])
			.map_err(postgres_error)?;
		Ok(())
	}

	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT result_json FROM {}.node_results WHERE task_id = $1 AND node_id = $2",
			self.schema,
		);
		let row = client
			.query_opt(&query, &[&task_id.0, &node_id.0])
			.map_err(postgres_error)?;
		row.map(|row| {
			serde_json::from_str::<ResultEnvelope>(&row.get::<_, String>(0))
				.map_err(StoreError::from)
		})
		.transpose()
	}

	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, StoreError> {
		let mut client = self.connect_client()?;
		let query = format!(
			"SELECT result_json FROM {}.node_results WHERE task_id = $1 ORDER BY node_id ASC",
			self.schema,
		);
		let rows = client
			.query(&query, &[&task_id.0])
			.map_err(postgres_error)?;
		rows.into_iter()
			.map(|row| {
				serde_json::from_str::<ResultEnvelope>(&row.get::<_, String>(0))
					.map_err(StoreError::from)
			})
			.collect()
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
		connect_client(&self.database_url)
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
		connect_client(&self.database_url)
	}

	fn ensure_schema_objects(&self) -> Result<(), StoreError> {
		let mut client = self.connect_client()?;
		ensure_schema(&mut client, &self.schema)?;
		ensure_conversation_turns_table(&mut client, &self.schema)
	}
}

impl ConversationRepository for PostgresConversationRepository {
	fn append_turn(&mut self, session_id: &str, turn: ConversationTurn) -> Result<(), StoreError> {
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
				&[&session_id, &(i64::try_from(limit).unwrap_or(i64::MAX))],
			)
			.map_err(postgres_error)?;
		let mut turns = rows
			.into_iter()
			.filter_map(|row| {
				parse_conversation_role(row.get::<_, String>(0).as_str()).map(|role| {
					ConversationTurn {
						role,
						content: row.get::<_, String>(1),
						created_at_unix_ms: row.get::<_, i64>(2).try_into().unwrap_or(u64::MAX),
					}
				})
			})
			.collect::<Vec<_>>();
		turns.reverse();
		Ok(turns)
	}
}

fn connect_client(database_url: &str) -> Result<Client, StoreError> {
	Client::connect(database_url, NoTls).map_err(postgres_error)
}

fn ensure_schema(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
		.map_err(postgres_error)
}

fn ensure_tasks_table(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!(
			"CREATE TABLE IF NOT EXISTS {schema}.tasks (\
			 task_id TEXT PRIMARY KEY,\
			 task_json TEXT NOT NULL,\
			 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
			 )"
		))
		.map_err(postgres_error)
}

fn ensure_task_events_table(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!(
			"CREATE TABLE IF NOT EXISTS {schema}.task_events (\
			 seq BIGSERIAL PRIMARY KEY,\
			 task_id TEXT NOT NULL,\
			 event_json TEXT NOT NULL,\
			 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
			 );\
			 CREATE INDEX IF NOT EXISTS idx_{schema}_task_events_task_seq\
			 ON {schema}.task_events (task_id, seq ASC)"
		))
		.map_err(postgres_error)
}

fn ensure_approval_tickets_table(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!(
			"CREATE TABLE IF NOT EXISTS {schema}.approval_tickets (\
			 approval_id TEXT PRIMARY KEY,\
			 task_id TEXT NOT NULL,\
			 ticket_json TEXT NOT NULL,\
			 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
			 );\
			 CREATE INDEX IF NOT EXISTS idx_{schema}_approval_tickets_task\
			 ON {schema}.approval_tickets (task_id)"
		))
		.map_err(postgres_error)
}

fn ensure_node_results_table(client: &mut Client, schema: &str) -> Result<(), StoreError> {
	client
		.batch_execute(&format!(
			"CREATE TABLE IF NOT EXISTS {schema}.node_results (\
			 task_id TEXT NOT NULL,\
			 node_id TEXT NOT NULL,\
			 result_json TEXT NOT NULL,\
			 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),\
			 PRIMARY KEY (task_id, node_id)\
			 );\
			 CREATE INDEX IF NOT EXISTS idx_{schema}_node_results_task\
			 ON {schema}.node_results (task_id, node_id)"
		))
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

	#[test]
	fn parse_conversation_role_roundtrip_is_stable() {
		assert_eq!(
			parse_conversation_role(conversation_role_label(ConversationRole::Assistant)),
			Some(ConversationRole::Assistant)
		);
	}
}
