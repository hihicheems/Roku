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

//! Unified resource catalog with lexical and embedding-style retrieval.

use std::collections::{BTreeSet, HashMap};

use roku_common_types::{ResourceSelector, ToolContract};
use serde::{Deserialize, Serialize};

// Retrieval invariants: these shape the in-process embedding/BM25 scoring model and are not
// operator-facing runtime knobs. They stay in code so catalog behaviour remains stable across
// deployments unless retrieval itself is intentionally redesigned.
const EMBEDDING_DIMENSIONS: usize = 64;
const BM25_K1: f32 = 1.5;
const BM25_B: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResourceKind {
	#[default]
	Tool,
	Skill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResourceRisk {
	#[default]
	Low,
	Medium,
	High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceCost {
	pub estimated_tokens: u64,
	pub estimated_latency_ms: u64,
}

/// Metadata for one catalog entry (tool or skill) used for discovery and routing.
///
/// Built from builtin modules (e.g. `fs::catalog_descriptors()`, `table::catalog_descriptors()`)
/// and skill registry; merged into a [`ResourceCatalog`] so the router/classifier can:
/// - **Retrieve** by user goal (BM25 + embedding over [`searchable_text`](CatalogDescriptor::searchable_text));
/// - **Prompt** the route LLM with name, description, summary, examples as "Current inventory";
/// - **Resolve** a chosen tool name to a [`ResourceSelector`] and use risk/cost for routing.
///
/// Field groups:
/// - **Identity:** `selector`, `kind`, `name`, `role`
/// - **Discovery text:** `description`, `summary`, `tags`, `examples`, `key_commands`, `use_cases`, `input_schema` (all contribute to retrieval and LLM inventory)
/// - **Routing:** `risk`, `cost`, `discoverable`, `required_capabilities`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogDescriptor {
	pub selector: ResourceSelector,
	pub kind: ResourceKind,
	pub name: String,
	#[serde(default)]
	pub role: Option<String>,
	pub description: String,
	#[serde(default = "default_discoverable")]
	pub discoverable: bool,
	#[serde(default)]
	pub tags: Vec<String>,
	#[serde(default)]
	pub examples: Vec<String>,
	#[serde(default)]
	pub input_schema: Vec<String>,
	#[serde(default)]
	pub risk: ResourceRisk,
	#[serde(default)]
	pub cost: ResourceCost,
	#[serde(default)]
	pub required_capabilities: Vec<String>,
	#[serde(default)]
	pub summary: String,
	#[serde(default)]
	pub key_commands: Vec<String>,
	#[serde(default)]
	pub use_cases: Vec<String>,
	#[serde(default)]
	pub contract: Option<ToolContract>,
}

impl CatalogDescriptor {
	/// Single blob of text used for lexical (BM25) and embedding retrieval.
	///
	/// [`ResourceCatalog::new`] tokenizes this for each entry and builds term stats and embeddings;
	/// [`ResourceCatalog::retrieve`] matches the user goal against these. Concatenating all
	/// discovery-related fields ensures queries like "list files" or "read xlsx" can match
	/// the right tool even when the match is in tags/examples/key_commands rather than description.
	pub fn searchable_text(&self) -> String {
		[
			self.name.as_str(),
			self.role.as_deref().unwrap_or_default(),
			self.description.as_str(),
			self.summary.as_str(),
			&self.tags.join(" "),
			&self.examples.join(" "),
			&self.input_schema.join(" "),
			&self.key_commands.join(" "),
			&self.use_cases.join(" "),
			self.contract
				.as_ref()
				.map(ToolContract::searchable_text)
				.as_deref()
				.unwrap_or_default(),
		]
		.join(" ")
	}
}

fn default_discoverable() -> bool {
	true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogMatch {
	pub descriptor: CatalogDescriptor,
	pub bm25_score: f32,
	pub embedding_score: f32,
	pub score: f32,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceCatalog {
	entries: Vec<CatalogDescriptor>,
	documents: Vec<CatalogDocument>,
	average_doc_len: f32,
}

#[derive(Debug, Clone)]
struct CatalogDocument {
	index: usize,
	terms: HashMap<String, usize>,
	length: usize,
	embedding: Vec<f32>,
}

impl ResourceCatalog {
	pub fn new(entries: Vec<CatalogDescriptor>) -> Self {
		let documents = entries
			.iter()
			.enumerate()
			.map(|(index, descriptor)| {
				let tokens = tokenize(&descriptor.searchable_text());
				let mut terms = HashMap::new();
				for token in &tokens {
					*terms.entry(token.clone()).or_insert(0) += 1;
				}

				CatalogDocument {
					index,
					terms,
					length: tokens.len().max(1),
					embedding: embed_text(&descriptor.searchable_text()),
				}
			})
			.collect::<Vec<_>>();
		let average_doc_len = if documents.is_empty() {
			1.0
		} else {
			documents.iter().map(|doc| doc.length as f32).sum::<f32>() / documents.len() as f32
		};

		Self {
			entries,
			documents,
			average_doc_len,
		}
	}

	pub fn entries(&self) -> &[CatalogDescriptor] {
		&self.entries
	}

	pub fn descriptor(&self, selector: &ResourceSelector) -> Option<&CatalogDescriptor> {
		self.entries
			.iter()
			.find(|entry| &entry.selector == selector)
	}

	pub fn descriptors_for_kind(&self, kind: ResourceKind) -> Vec<CatalogDescriptor> {
		self.entries
			.iter()
			.filter(|entry| entry.kind == kind)
			.cloned()
			.collect()
	}

	pub fn retrieve(
		&self,
		query: &str,
		kind: Option<ResourceKind>,
		limit: usize,
	) -> Vec<CatalogMatch> {
		if query.trim().is_empty() || limit == 0 {
			return Vec::new();
		}

		let query_tokens = tokenize(query);
		if query_tokens.is_empty() {
			return Vec::new();
		}
		let query_embedding = embed_text(query);
		let doc_frequency = document_frequency(&self.documents);
		let doc_count = self.documents.len() as f32;

		let mut matches = self
			.documents
			.iter()
			.filter_map(|document| {
				let descriptor = self.entries.get(document.index)?;
				if kind.is_some_and(|resource_kind| descriptor.kind != resource_kind) {
					return None;
				}

				let bm25_score = bm25(
					&query_tokens,
					document,
					&doc_frequency,
					doc_count,
					self.average_doc_len,
				);
				let embedding_score = cosine_similarity(&query_embedding, &document.embedding);
				let score = (bm25_score * 0.65) + (embedding_score * 0.35);
				if score <= 0.0 {
					return None;
				}

				Some(CatalogMatch {
					descriptor: descriptor.clone(),
					bm25_score,
					embedding_score,
					score,
				})
			})
			.collect::<Vec<_>>();

		matches.sort_by(|left, right| {
			right
				.score
				.total_cmp(&left.score)
				.then_with(|| left.descriptor.name.cmp(&right.descriptor.name))
		});
		matches.truncate(limit);
		matches
	}
}

fn document_frequency(documents: &[CatalogDocument]) -> HashMap<String, usize> {
	let mut counts = HashMap::new();
	for document in documents {
		for term in document.terms.keys() {
			*counts.entry(term.clone()).or_insert(0) += 1;
		}
	}
	counts
}

fn bm25(
	query_tokens: &[String],
	document: &CatalogDocument,
	doc_frequency: &HashMap<String, usize>,
	doc_count: f32,
	average_doc_len: f32,
) -> f32 {
	let mut seen_terms = BTreeSet::new();
	let mut score = 0.0;
	for token in query_tokens {
		if !seen_terms.insert(token.clone()) {
			continue;
		}

		let tf = *document.terms.get(token).unwrap_or(&0) as f32;
		if tf == 0.0 {
			continue;
		}
		let df = *doc_frequency.get(token).unwrap_or(&0) as f32;
		let idf = ((doc_count - df + 0.5) / (df + 0.5) + 1.0).ln();
		let norm =
			tf + BM25_K1 * (1.0 - BM25_B + BM25_B * (document.length as f32 / average_doc_len));
		score += idf * (tf * (BM25_K1 + 1.0) / norm);
	}
	score
}

fn tokenize(text: &str) -> Vec<String> {
	let mut tokens = Vec::new();
	let mut current = String::new();

	for character in text.chars() {
		if character.is_ascii_alphanumeric() {
			current.push(character.to_ascii_lowercase());
			continue;
		}
		if is_cjk(character) {
			if !current.is_empty() {
				tokens.push(current.clone());
				current.clear();
			}
			tokens.push(character.to_string());
			continue;
		}
		if !current.is_empty() {
			tokens.push(current.clone());
			current.clear();
		}
	}

	if !current.is_empty() {
		tokens.push(current);
	}

	tokens
}

fn embed_text(text: &str) -> Vec<f32> {
	let mut vector = vec![0.0_f32; EMBEDDING_DIMENSIONS];
	let normalized = text.to_ascii_lowercase();
	for token in tokenize(&normalized) {
		project_feature(&mut vector, &token);
	}
	for trigram in char_trigrams(&normalized) {
		project_feature(&mut vector, &trigram);
	}

	let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
	if norm > 0.0 {
		for value in &mut vector {
			*value /= norm;
		}
	}
	vector
}

fn project_feature(vector: &mut [f32], feature: &str) {
	let hash = fnv1a64(feature.as_bytes());
	let slot = (hash as usize) % EMBEDDING_DIMENSIONS;
	let sign = if (hash >> 63) == 0 { 1.0 } else { -1.0 };
	vector[slot] += sign;
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
	left.iter()
		.zip(right.iter())
		.map(|(left, right)| left * right)
		.sum::<f32>()
		.max(0.0)
}

fn char_trigrams(text: &str) -> Vec<String> {
	let compact = text
		.chars()
		.filter(|character| character.is_ascii_alphanumeric() || is_cjk(*character))
		.collect::<Vec<_>>();
	if compact.len() < 3 {
		return Vec::new();
	}

	compact
		.windows(3)
		.map(|window| window.iter().collect::<String>())
		.collect()
}

fn is_cjk(character: char) -> bool {
	matches!(character as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
	// FNV hashing constants are protocol-level retrieval internals, not deploy-time config.
	const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
	const PRIME: u64 = 0x100000001b3;

	let mut hash = OFFSET_BASIS;
	for byte in bytes {
		hash ^= u64::from(*byte);
		hash = hash.wrapping_mul(PRIME);
	}
	hash
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tool_descriptor(name: &str, description: &str) -> CatalogDescriptor {
		CatalogDescriptor {
			selector: ResourceSelector::tool(name),
			kind: ResourceKind::Tool,
			name: name.to_string(),
			role: None,
			description: description.to_string(),
			discoverable: true,
			tags: Vec::new(),
			examples: Vec::new(),
			input_schema: Vec::new(),
			risk: ResourceRisk::Low,
			cost: ResourceCost::default(),
			required_capabilities: Vec::new(),
			summary: description.to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
			contract: None,
		}
	}

	#[test]
	fn retrieves_best_matching_tool() {
		let catalog = ResourceCatalog::new(vec![
			tool_descriptor("data.execute", "Analyze tabular datasets and metrics"),
			tool_descriptor("review.assess", "Review correctness and risk"),
		]);

		let matches = catalog.retrieve("analyze dataset metrics", Some(ResourceKind::Tool), 2);
		assert_eq!(matches[0].descriptor.name, "data.execute");
	}

	#[test]
	fn filters_by_kind() {
		let catalog = ResourceCatalog::new(vec![
			CatalogDescriptor {
				selector: ResourceSelector::skill("postgres-backup"),
				kind: ResourceKind::Skill,
				name: "postgres-backup".to_string(),
				role: None,
				description: "Back up postgres databases".to_string(),
				discoverable: true,
				..tool_descriptor("ignored", "ignored")
			},
			tool_descriptor("data.execute", "Analyze datasets"),
		]);

		let matches = catalog.retrieve("postgres backup", Some(ResourceKind::Skill), 5);
		assert_eq!(matches.len(), 1);
		assert_eq!(matches[0].descriptor.kind, ResourceKind::Skill);
	}
}
