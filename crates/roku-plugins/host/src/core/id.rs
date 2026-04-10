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

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::PluginIdError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId(String);

impl PluginId {
	pub fn new(value: impl Into<String>) -> Result<Self, PluginIdError> {
		let value = value.into();
		validate_plugin_id(&value)?;
		Ok(Self(value))
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for PluginId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

impl FromStr for PluginId {
	type Err = PluginIdError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		Self::new(value)
	}
}

impl TryFrom<String> for PluginId {
	type Error = PluginIdError;

	fn try_from(value: String) -> Result<Self, Self::Error> {
		Self::new(value)
	}
}

impl From<PluginId> for String {
	fn from(value: PluginId) -> Self {
		value.0
	}
}

impl Serialize for PluginId {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_str(&self.0)
	}
}

impl<'de> Deserialize<'de> for PluginId {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		Self::new(value).map_err(serde::de::Error::custom)
	}
}

fn validate_plugin_id(value: &str) -> Result<(), PluginIdError> {
	if value.trim().is_empty() {
		return Err(PluginIdError::Empty);
	}

	let mut chars = value.chars();
	let first = chars.next().ok_or(PluginIdError::Empty)?;
	let last = value.chars().last().ok_or(PluginIdError::Empty)?;
	if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
		return Err(PluginIdError::InvalidBoundary);
	}

	for character in value.chars() {
		if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-' {
			continue;
		}
		return Err(PluginIdError::InvalidCharacter { character });
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::PluginId;

	#[test]
	fn accept_lowercase_hyphenated_ids() {
		let id = PluginId::new("builtin-tools").expect("id should be valid");
		assert_eq!(id.as_str(), "builtin-tools");
	}

	#[test]
	fn reject_non_ascii_or_uppercase_ids() {
		assert!(PluginId::new("BuiltinTools").is_err());
		assert!(PluginId::new("builtin_tools").is_err());
	}
}
