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

use thiserror::Error;

use roku_mcp_bridge::McpError;

#[derive(Debug, Error)]
pub enum CodingProviderError {
	#[error("invalid coding work contract: {0}")]
	InvalidContract(String),
	#[error("mcp bridge error: {0}")]
	Bridge(#[from] McpError),
	#[error("coding provider rejected work contract: {0}")]
	ProviderRejected(String),
	#[error("invalid coding provider response: {0}")]
	InvalidResponse(String),
}
