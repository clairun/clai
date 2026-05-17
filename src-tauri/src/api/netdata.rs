//! Netdata Cloud API implementation.
//!
//! This module provides a typed interface to the Netdata Cloud API.
//! All API calls are made through the `NetdataApi` struct.
//!
//! # Rust Learning: API Client Pattern
//!
//! We use a struct to hold the configuration (client, base_url, token)
//! and methods to make API calls. This is similar to creating a class
//! in JavaScript:
//!
//! ```javascript
//! // JavaScript equivalent
//! class NetdataApi {
//!     constructor(client, baseUrl, token) { ... }
//!     async getUserInfo() { ... }
//! }
//! ```

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::api::client::DEFAULT_TIMEOUT;
use crate::api::error::{ApiError, ApiResult, NetdataErrorResponse};

/// Netdata Cloud API client.
///
/// # Rust Learning: Struct Fields
///
/// Unlike JavaScript objects, Rust structs have fixed fields defined at compile time.
/// Each field has a specific type, and the compiler ensures type safety.
pub struct NetdataApi {
    /// The HTTP client (reused for connection pooling)
    client: Client,
    /// Base URL of the Netdata Cloud API
    base_url: String,
    /// Bearer token for authentication
    token: String,
}

impl NetdataApi {
    /// Creates a new API client instance.
    ///
    /// # Arguments
    ///
    /// * `client` - Reusable HTTP client
    /// * `base_url` - API base URL (e.g., "https://app.netdata.cloud")
    /// * `token` - Bearer token for authentication
    pub fn new(client: Client, base_url: String, token: String) -> Self {
        Self {
            client,
            base_url,
            token,
        }
    }

    /// Builds the Authorization header value.
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Handles HTTP response, converting status codes to appropriate errors.
    ///
    /// # Rust Learning: Generics with Trait Bounds
    ///
    /// `T: for<'de> Deserialize<'de>` means "T can be deserialized from any lifetime".
    /// This is the standard way to accept any deserializable type.
    ///
    /// # Error Handling
    ///
    /// When the API returns an error, we try to parse the response body as a
    /// `NetdataErrorResponse` to extract detailed error information. This allows
    /// JavaScript to match on specific error conditions via `errorMsgKey`.
    async fn handle_response<T>(&self, response: reqwest::Response) -> ApiResult<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let status = response.status();

        if status.is_success() {
            response
                .json()
                .await
                .map_err(|e| ApiError::ParseError(e.to_string()))
        } else {
            // Try to parse the error response body to extract Netdata error details
            let netdata_error: Option<NetdataErrorResponse> = response.json().await.ok(); // If parsing fails, we'll use defaults

            Err(ApiError::from_response(status, netdata_error))
        }
    }

    // =========================================================================
    // API Methods
    // =========================================================================

    /// Gets data with complex aggregation and filtering options.
    ///
    /// Endpoint: `POST /api/v3/spaces/{space_id}/rooms/{room_id}/data`
    ///
    /// This is used for fetching metrics data with flexible aggregation,
    /// grouping, and time windowing options.
    ///
    /// # Arguments
    ///
    /// * `space_id` - The ID of the space
    /// * `room_id` - The ID of the room
    /// * `query` - The data query parameters
    pub async fn get_data(
        &self,
        space_id: &str,
        room_id: &str,
        query: DataQuery,
    ) -> ApiResult<DataResponse> {
        let response = self
            .client
            .post(format!(
                "{}/api/v3/spaces/{}/rooms/{}/data",
                self.base_url, space_id, room_id
            ))
            .timeout(DEFAULT_TIMEOUT)
            .header("Authorization", self.auth_header())
            .json(&query)
            .send()
            .await?;

        self.handle_response(response).await
    }

    /// Gets available contexts (metrics) for a space/room.
    ///
    /// Endpoint: `POST /api/v3/spaces/{space_id}/rooms/{room_id}/contexts`
    ///
    /// This is used to discover what metrics are available for querying.
    ///
    /// # Arguments
    ///
    /// * `space_id` - The ID of the space
    /// * `room_id` - The ID of the room
    /// * `query` - The contexts query parameters
    pub async fn get_contexts(
        &self,
        space_id: &str,
        room_id: &str,
        query: ContextsQuery,
    ) -> ApiResult<ContextsResponse> {
        let response = self
            .client
            .post(format!(
                "{}/api/v3/spaces/{}/rooms/{}/contexts",
                self.base_url, space_id, room_id
            ))
            .timeout(DEFAULT_TIMEOUT)
            .header("Authorization", self.auth_header())
            .json(&query)
            .send()
            .await?;

        self.handle_response(response).await
    }
}

// =============================================================================
// Data Types
// =============================================================================
//
// These structs match the JSON responses from the Netdata Cloud API.
// Serde automatically converts between JSON and these Rust types.

// =============================================================================
// Data Query Types
// =============================================================================
//
// These types are used for the complex getData and getContexts endpoints.
// They mirror the JavaScript client's parameter structure.

/// Request body for the data query endpoint.
///
/// # Rust Learning: Optional Fields with `skip_serializing_if`
///
/// The `#[serde(skip_serializing_if = "Option::is_none")]` attribute
/// tells serde to omit this field from JSON if it's `None`.
/// This keeps the request body clean and matches the JS behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataQuery {
    /// Response format (default: "json2")
    #[serde(default = "default_format")]
    pub format: String,

    /// Query options
    #[serde(default = "default_data_options")]
    pub options: Vec<String>,

    /// Data scope definition
    pub scope: DataScope,

    /// Data selectors
    #[serde(default)]
    pub selectors: DataSelectors,

    /// Aggregation configuration
    pub aggregations: DataAggregations,

    /// Time window
    pub window: DataWindow,

    /// Request timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_format() -> String {
    "json2".to_string()
}

fn default_data_options() -> Vec<String> {
    vec![
        "jsonwrap".to_string(),
        "nonzero".to_string(),
        "flip".to_string(),
        "ms".to_string(),
        "jw-anomaly-rates".to_string(),
        "minify".to_string(),
    ]
}

fn default_timeout() -> u64 {
    10000
}

/// Scope definition for data queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataScope {
    /// Context patterns (e.g., ["system.cpu"])
    pub contexts: Vec<String>,

    /// Node IDs
    pub nodes: Vec<String>,

    /// Optional instance patterns
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instances: Option<Vec<String>>,

    /// Optional dimension names
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Vec<String>>,

    /// Optional label filters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
}

/// Selectors for data queries (defaults to "*" for all).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DataSelectors {
    /// Context patterns to select
    #[serde(default = "default_wildcard")]
    pub contexts: Vec<String>,

    /// Node IDs to select
    #[serde(default = "default_wildcard")]
    pub nodes: Vec<String>,

    /// Instance patterns to select
    #[serde(default = "default_wildcard")]
    pub instances: Vec<String>,

    /// Dimension names to select
    #[serde(default = "default_wildcard")]
    pub dimensions: Vec<String>,

    /// Label filters to select
    #[serde(default = "default_wildcard")]
    pub labels: Vec<String>,

    /// Alert filters (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alerts: Option<Vec<String>>,
}

fn default_wildcard() -> Vec<String> {
    vec!["*".to_string()]
}

/// Aggregation configuration for data queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataAggregations {
    /// Metric aggregations
    pub metrics: Vec<MetricAggregation>,

    /// Time aggregation settings
    pub time: TimeAggregation,
}

/// Individual metric aggregation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricAggregation {
    /// Aggregation function (sum, avg, min, max, etc.)
    pub aggregation: String,

    /// Group by dimensions/nodes
    #[serde(default)]
    pub group_by: Vec<String>,

    /// Group by label keys
    #[serde(default)]
    pub group_by_label: Vec<String>,
}

/// Time aggregation settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeAggregation {
    /// Time grouping method
    pub time_group: String,

    /// Resampling interval in seconds
    pub time_resampling: i64,

    /// Additional time group options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_group_options: Option<String>,
}

/// Time window for data queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataWindow {
    /// Unix timestamp (seconds) for start time
    pub after: i64,

    /// Unix timestamp (seconds) for end time
    pub before: i64,

    /// Number of points to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<i64>,

    /// Duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,

    /// Data tier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<i64>,

    /// Baseline window configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<serde_json::Value>,
}

// =============================================================================
// Data Response Types
// =============================================================================
//
// These types represent the response from the getData API endpoint.
// The API uses short field names (nd, mg, sts, etc.) for efficiency.

/// Response from the data query endpoint (v3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataResponse {
    pub summary: DataSummary,
    pub result: DataResult,
    pub view: DataView,
}

/// Summary section of data response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSummary {
    #[serde(default)]
    pub nodes: Vec<NodeSummary>,
    #[serde(default)]
    pub instances: Vec<InstanceSummary>,
    #[serde(default)]
    pub dimensions: Vec<DimensionSummary>,
    #[serde(default)]
    pub labels: Vec<LabelSummary>,
}

/// Node summary with statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    /// Node ID (shortened field name in API)
    #[serde(rename = "nd")]
    pub node_id: String,
    /// Machine GUID
    #[serde(rename = "mg")]
    pub machine_guid: String,
    /// Statistics (optional - not always present)
    #[serde(rename = "sts", default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsSummary>,
}

/// Instance summary with statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSummary {
    pub id: String,
    #[serde(rename = "sts", default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsSummary>,
}

/// Dimension summary with statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionSummary {
    pub id: String,
    #[serde(rename = "sts", default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsSummary>,
}

/// Label summary with values and statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelSummary {
    pub id: String,
    #[serde(rename = "sts", default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsSummary>,
    /// Label values
    #[serde(rename = "vl", default)]
    pub values: Vec<LabelValueSummary>,
}

/// Label value summary with statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelValueSummary {
    pub id: String,
    #[serde(rename = "sts", default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsSummary>,
}

/// Statistics summary for nodes, instances, dimensions, and labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummary {
    /// Anomaly rate percentage
    #[serde(rename = "arp", default)]
    pub anomaly_rate: f64,
    /// Contribution percentage
    #[serde(rename = "con", default)]
    pub contribution_percentage: f64,
}

/// Result section of data response containing the actual data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataResult {
    /// Column labels
    #[serde(default)]
    pub labels: Vec<String>,
    /// Point index mapping (column name -> index)
    #[serde(default)]
    pub point: std::collections::HashMap<String, i32>,
    /// Data rows (each row is an array of mixed types: timestamps, values, etc.)
    #[serde(default)]
    pub data: Vec<Vec<serde_json::Value>>,
}

/// View configuration section of data response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataView {
    /// Units for the data (can be various types)
    pub units: serde_json::Value,
}

// =============================================================================
// Contexts Query Types
// =============================================================================

/// Request body for the contexts query endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextsQuery {
    /// Response format (default: "json2")
    #[serde(default = "default_format")]
    pub format: String,

    /// Data scope definition
    pub scope: ContextsScope,

    /// Data selectors
    pub selectors: ContextsSelectors,

    /// Time window
    pub window: ContextsWindow,

    /// Request timeout in milliseconds
    #[serde(default = "default_contexts_timeout")]
    pub timeout: u64,
}

fn default_contexts_timeout() -> u64 {
    20000
}

/// Scope definition for contexts queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextsScope {
    /// Context patterns
    #[serde(default = "default_wildcard")]
    pub contexts: Vec<String>,

    /// Node IDs
    #[serde(default)]
    pub nodes: Vec<String>,
}

/// Selectors for contexts queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextsSelectors {
    /// Context patterns to select
    #[serde(default = "default_wildcard")]
    pub contexts: Vec<String>,

    /// Node IDs to select
    #[serde(default = "default_wildcard")]
    pub nodes: Vec<String>,
}

/// Time window for contexts queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextsWindow {
    /// Unix timestamp (seconds) for start time
    pub after: i64,

    /// Unix timestamp (seconds) for end time
    pub before: i64,
}

// =============================================================================
// Contexts Response Types
// =============================================================================

/// Response from the contexts query endpoint (v3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextsResponse {
    /// Map of context name to context info
    #[serde(default)]
    pub contexts: std::collections::HashMap<String, ContextInfo>,
}

/// Information about a single context (metric).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInfo {
    /// Context family (grouping)
    #[serde(default)]
    pub family: String,
    /// First data entry timestamp (Unix seconds)
    #[serde(default)]
    pub first_entry: i64,
    /// Last data entry timestamp (Unix seconds)
    #[serde(default)]
    pub last_entry: i64,
}
