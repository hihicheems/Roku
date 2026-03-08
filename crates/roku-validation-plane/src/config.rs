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

#[derive(Debug, Clone)]
pub struct ValidationConfig {
	pub require_evidence: bool,
	pub min_confidence: f32,
	pub require_non_empty_producer: bool,
	pub enable_cross_checks: bool,
}

impl Default for ValidationConfig {
	fn default() -> Self {
		Self {
			require_evidence: true,
			min_confidence: 0.1,
			require_non_empty_producer: true,
			enable_cross_checks: true,
		}
	}
}
